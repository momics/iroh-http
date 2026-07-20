#![allow(clippy::disallowed_types)] // test/bench file — RequestPayload and friends are valid here
mod common;

use bytes::Bytes;
use iroh_http_core::respond;
use iroh_http_core::{
    fetch, ffi_serve, IrohEndpoint, NetworkingOptions, NodeOptions, RequestPayload, ServeOptions,
};

// -- Graceful shutdown (patch 15) ---------------------------------------------

/// Graceful shutdown: in-flight request completes, then drain finishes.
#[tokio::test]
async fn graceful_shutdown_drains_in_flight() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // Use a notify to confirm the handler is actually running.
    let handler_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let handler_started_tx = handler_started.clone();
    // Use a notify to let the handler proceed after shutdown is triggered.
    let handler_proceed = std::sync::Arc::new(tokio::sync::Notify::new());
    let handler_proceed_rx = handler_proceed.clone();

    let handle = ffi_serve(
        server_ep.clone(),
        ServeOptions {
            drain_timeout_ms: Some(10_000),
            ..Default::default()
        },
        move |payload: RequestPayload| {
            let res_h = payload.res_body_handle;
            let req_h = payload.req_handle;
            let started = handler_started_tx.clone();
            let proceed = handler_proceed_rx.clone();
            let server_ep = server_ep.clone();
            tokio::spawn(async move {
                // Signal that the handler is running.
                started.notify_one();
                // Wait for the test to signal us to proceed (deterministic sync).
                proceed.notified().await;
                respond(
                    server_ep.handles(),
                    req_h,
                    200,
                    vec![("content-length".into(), "2".into())],
                )
                .unwrap();
                server_ep
                    .handles()
                    .send_chunk(res_h, Bytes::from_static(b"ok"))
                    .await
                    .unwrap();
                server_ep.handles().finish_body(res_h).unwrap();
            });
        },
    );

    // Start a request that will take 1s to complete.
    let fetch_task = {
        let client = client_ep.clone();
        let sid = server_id.clone();
        let a = addrs.clone();
        tokio::spawn(async move {
            fetch(
                &client,
                &sid,
                "/slow",
                "GET",
                &[],
                None,
                None,
                Some(&a),
                None,
                true,
                None, // max_response_body_bytes
            )
            .await
        })
    };

    // Wait for the handler to actually start running before we trigger shutdown.
    handler_started.notified().await;

    // Start drain in the background — it should block until the handler finishes.
    let drain_done = std::sync::Arc::new(tokio::sync::Notify::new());
    let drain_done_rx = drain_done.clone();
    tokio::spawn(async move {
        handle.drain().await;
        drain_done.notify_one();
    });

    // Give a brief yield to ensure drain has started waiting.
    tokio::task::yield_now().await;

    // Drain should NOT have completed yet because the handler hasn't finished.
    // (We can't assert this deterministically, but the handler_proceed signal
    // below ensures the handler runs to completion before drain finishes.)

    // Let the handler complete.
    handler_proceed.notify_one();

    // Drain should complete now that the handler has finished and the peer has
    // acknowledged the response stream. ASan reliably stresses the client
    // enough to expose a transport-delivery race here: graceful shutdown used
    // to close the QUIC connection immediately after queuing the response,
    // before the peer received even the response head.
    tokio::time::timeout(std::time::Duration::from_secs(10), drain_done_rx.notified())
        .await
        .expect("drain should complete after handler finishes");

    // The in-flight request should have succeeded.
    let result = fetch_task.await.unwrap();
    assert!(
        result.is_ok(),
        "in-flight request should succeed: {:?}",
        result
    );
    let res = result.unwrap();
    assert_eq!(res.status, 200);
}

/// Force close aborts immediately without draining.
#[tokio::test]
async fn force_close_aborts_immediately() {
    let (server_ep, _client_ep) = common::make_pair().await;

    let _handle = ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |_payload: RequestPayload| {},
    );

    let start = std::time::Instant::now();
    server_ep.close_force().await;
    let elapsed = start.elapsed();

    // Force close should complete quickly (well under 5 seconds).
    // iroh's QUIC close path can take ~1s on slow machines, so we use 5s.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "force close took too long: {elapsed:?}"
    );
}

/// A node with no serve loop should close immediately.
#[tokio::test]
async fn close_without_serve_is_immediate() {
    let ep = IrohEndpoint::bind(NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap();

    let start = std::time::Instant::now();
    ep.close().await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "close without serve took too long: {elapsed:?}"
    );
}

/// After shutdown, new requests are rejected (connection refused).
#[tokio::test(start_paused = true)]
async fn shutdown_rejects_new_requests() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    let server_ep_handler = server_ep.clone();
    let handle = ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            respond(
                server_ep_handler.handles(),
                payload.req_handle,
                200,
                vec![("content-length".into(), "0".into())],
            )
            .unwrap();
            server_ep_handler
                .handles()
                .finish_body(payload.res_body_handle)
                .unwrap();
        },
    );

    // First request should succeed.
    let res = fetch(
        &client_ep,
        &server_id,
        "/before",
        "GET",
        &[],
        None,
        None,
        Some(&addrs),
        None,
        true,
        None, // max_response_body_bytes
    )
    .await
    .unwrap();
    assert_eq!(res.status, 200);
    while let Ok(Some(_)) = client_ep.handles().next_chunk(res.body_handle).await {}

    // Shut down the serve loop.
    handle.drain().await;

    // Close the endpoint too so the client gets a clean rejection.
    server_ep.close_force().await;

    // Request after shutdown should fail.
    let result = fetch(
        &client_ep,
        &server_id,
        "/after",
        "GET",
        &[],
        None,
        None,
        Some(&addrs),
        None,
        true,
        None, // max_response_body_bytes
    )
    .await;
    assert!(
        result.is_err(),
        "expected error after shutdown, got: {:?}",
        result
    );
}

/// ServeHandle::shutdown() returns immediately without blocking.
#[tokio::test]
async fn shutdown_returns_immediately() {
    let (server_ep, _client_ep) = common::make_pair().await;

    let handle = ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |_payload: RequestPayload| {},
    );

    let start = std::time::Instant::now();
    handle.shutdown();
    let elapsed = start.elapsed();

    // shutdown() should be non-blocking (< 10ms).
    assert!(
        elapsed < std::time::Duration::from_millis(100),
        "shutdown() blocked for {elapsed:?}"
    );
}

/// Graceful shutdown via `ServeHandle::drain` stops accepting new connections
/// but lets in-flight requests complete.
#[tokio::test(start_paused = true)]
async fn node_close_drains_in_flight() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // The server handler waits for a signal before responding.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));

    let handle = ffi_serve(
        server_ep.clone(),
        ServeOptions {
            drain_timeout_ms: Some(5_000),
            ..Default::default()
        },
        move |payload: RequestPayload| {
            let req_handle = payload.req_handle;
            let res_body = payload.res_body_handle;
            let tx_clone = tx.clone();
            let server_ep = server_ep.clone();
            tokio::spawn(async move {
                // Signal the test that the handler is in progress.
                if let Some(tx) = tx_clone.lock().await.take() {
                    let _ = tx.send(());
                }
                // Small pause to simulate work.
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                respond(server_ep.handles(), req_handle, 200, vec![]).unwrap();
                server_ep.handles().finish_body(res_body).unwrap();
            });
        },
    );

    // Start a request in the background.
    let fetch_task = tokio::spawn({
        let client_ep = client_ep.clone();
        async move {
            fetch(
                &client_ep,
                &server_id,
                "/drain-test",
                "GET",
                &[],
                None,
                None,
                Some(&addrs),
                None,
                true,
                None, // max_response_body_bytes
            )
            .await
        }
    });

    // Wait until the handler has started.
    let _ = rx.await;

    // Trigger graceful shutdown — should wait for the in-flight request.
    handle.drain().await;

    // The fetch should have completed successfully.
    let res = fetch_task.await.expect("join error");
    assert_eq!(res.unwrap().status, 200);
}
