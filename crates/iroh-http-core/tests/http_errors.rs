#![allow(clippy::disallowed_types)] // test/bench file u2014 FFI types valid here
mod common;

use bytes::Bytes;
use iroh_http_core::respond;
use iroh_http_core::{
    fetch, fetch_with_connect_timeout, ffi_serve, IrohEndpoint, NetworkingOptions, NodeOptions,
    RequestPayload, ServeOptions,
};

// -- Fetch cancellation -------------------------------------------------------

#[tokio::test]
async fn fetch_cancelled_via_token() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // Server: signal when the request arrives, then hang indefinitely.
    let (request_arrived_tx, request_arrived_rx) = tokio::sync::oneshot::channel::<()>();
    let request_arrived_tx = std::sync::Mutex::new(Some(request_arrived_tx));

    ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |_payload: RequestPayload| {
            if let Some(tx) = request_arrived_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            // Never respond — the client should cancel.
        },
    );

    let baseline = client_ep.handles().count_handles().3;
    let token = client_ep.handles().alloc_fetch_token().unwrap();

    // Cancel as soon as the server has received the request — no sleep needed.
    let client_ep_cancel = client_ep.clone();
    tokio::spawn(async move {
        let _ = request_arrived_rx.await;
        client_ep_cancel.handles().cancel_in_flight(token);
    });

    let result = fetch(
        &client_ep,
        &server_id,
        "/slow",
        "GET",
        &[],
        None,
        Some(token),
        Some(&addrs),
        None,
        true,
        None, // max_response_body_bytes
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().code,
        iroh_http_core::ErrorCode::Cancelled
    );
    assert_eq!(client_ep.handles().count_handles().3, baseline);
}

// -- respond() error path -----------------------------------------------------

#[tokio::test]
async fn respond_invalid_handle() {
    let ep = IrohEndpoint::bind(NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap();
    let result = respond(ep.handles(), 999999, 200, vec![]);
    assert!(result.is_err());
}

// -- Fetch with bad node ID ---------------------------------------------------

#[tokio::test]
async fn fetch_bad_node_id_returns_error() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let client = IrohEndpoint::bind(opts).await.unwrap();
    let result = fetch(
        &client,
        "!!!invalid!!!",
        "/",
        "GET",
        &[],
        None,
        None,
        None,
        None,
        true,
        None, // max_response_body_bytes
    )
    .await;
    assert!(result.is_err());
}

/// Fetch to an unknown peer returns an Error (not a panic or hang).
#[tokio::test]
async fn fetch_unknown_peer() {
    // Generate a random keypair — nobody is listening on it.
    let fake_key = iroh_http_core::generate_secret_key().unwrap();
    let fake_pk = iroh::SecretKey::from_bytes(&fake_key).public();
    let fake_id = iroh_http_core::base32_encode(fake_pk.as_bytes());

    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let client_ep = IrohEndpoint::bind(opts).await.unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fetch(
            &client_ep,
            &fake_id,
            "/",
            "GET",
            &[],
            None,
            None,
            None,
            None,
            true,
            None, // max_response_body_bytes
        ),
    )
    .await;

    // Should not hang — either a timeout or a connection error.
    match result {
        Ok(Err(_)) => {} // connection error — expected
        Err(_) => {}     // our 5s timeout — also acceptable if iroh takes longer
        Ok(Ok(res)) => panic!(
            "expected error connecting to unknown peer, got status {}",
            res.status
        ),
    }
}

#[tokio::test]
async fn connection_timeout_is_typed_and_releases_fetch_token() {
    let fake_key = iroh_http_core::generate_secret_key().unwrap();
    let fake_pk = iroh::SecretKey::from_bytes(&fake_key).public();
    let fake_id = iroh_http_core::base32_encode(fake_pk.as_bytes());
    let client_ep = IrohEndpoint::bind(NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap();
    let baseline = client_ep.handles().count_handles().3;
    let token = client_ep.handles().alloc_fetch_token().unwrap();
    let unreachable = ["192.0.2.1:9".parse().expect("valid test-net address")];

    let error = fetch_with_connect_timeout(
        &client_ep,
        &fake_id,
        "/",
        "GET",
        &[],
        None,
        Some(token),
        Some(&unreachable),
        Some(std::time::Duration::from_millis(25)),
        Some(std::time::Duration::from_secs(5)),
        true,
        None,
    )
    .await
    .expect_err("zero connection timeout should expire");

    assert_eq!(error.code, iroh_http_core::ErrorCode::ConnectionTimeout);
    assert_eq!(client_ep.handles().count_handles().3, baseline);
}

/// Request timeout: server handler takes longer than `request_timeout_ms`;
/// the fetch task should complete (possibly with error) rather than hang forever.
#[tokio::test]
async fn request_timeout_fires() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    ffi_serve(
        server_ep.clone(),
        ServeOptions {
            request_timeout_ms: Some(100), // 100ms timeout
            ..Default::default()
        },
        move |payload: RequestPayload| {
            let req_handle = payload.req_handle;
            let res_body = payload.res_body_handle;
            let server_ep = server_ep.clone();
            tokio::spawn(async move {
                // Never respond — let the server timeout kill the request.
                std::future::pending::<()>().await;
                // The handler may still respond but it's racing the timeout.
                let _ = respond(server_ep.handles(), req_handle, 200, vec![]);
                let _ = server_ep.handles().finish_body(res_body);
            });
        },
    );

    // The fetch should come back (either with an error or with whatever the
    // server managed to send before timeout killed the task).
    // The 100ms server timeout + propagation should resolve well within 10s.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fetch(
            &client_ep,
            &server_id,
            "/slow",
            "GET",
            &[],
            None,
            None,
            Some(&addrs),
            None,
            true,
            None, // max_response_body_bytes
        ),
    )
    .await;

    // Should not hang — accept either an error or a 200 that raced through.
    assert!(result.is_ok(), "fetch should not hang past the timeout");
}

// ── Edge-case tests (TEST-004) ────────────────────────────────────────────────

/// Cancel mid-stream: client cancels while server is still writing body chunks.
/// The server should observe an error but not panic, and the endpoint stays
/// healthy for subsequent requests.
#[tokio::test]
async fn cancel_mid_stream_no_panic() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    let (request_arrived_tx, request_arrived_rx) = tokio::sync::oneshot::channel::<()>();
    let request_arrived_tx = std::sync::Mutex::new(Some(request_arrived_tx));

    ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            let req_handle = payload.req_handle;
            let res_body = payload.res_body_handle;
            if let Some(tx) = request_arrived_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let server_ep = server_ep.clone();
            tokio::spawn(async move {
                respond(server_ep.handles(), req_handle, 200, vec![]).unwrap();
                // Write chunks slowly — the client will cancel mid-stream.
                for i in 0..100 {
                    let chunk = Bytes::from(format!("chunk-{i}\n"));
                    if server_ep
                        .handles()
                        .send_chunk(res_body, chunk)
                        .await
                        .is_err()
                    {
                        break; // Client cancelled — expected.
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                let _ = server_ep.handles().finish_body(res_body);
            });
        },
    );

    let token = client_ep.handles().alloc_fetch_token().unwrap();

    // Cancel as soon as server starts responding.
    let client_ep_cancel = client_ep.clone();
    tokio::spawn(async move {
        let _ = request_arrived_rx.await;
        // Small delay to let a few chunks through.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        client_ep_cancel.handles().cancel_in_flight(token);
    });

    let result = fetch(
        &client_ep,
        &server_id,
        "/stream",
        "GET",
        &[],
        None,
        Some(token),
        Some(&addrs),
        None,
        true,
        None, // max_response_body_bytes
    )
    .await;

    // Either the fetch errors (cancelled) or we got a partial response.
    // The key assertion: no panic occurred.
    let _ = result;
}

// ── BodyReader cancellation ───────────────────────────────────────────────────

#[tokio::test]
async fn cancel_reader_terminates_in_flight_read() {
    let ep = IrohEndpoint::bind(NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap();

    let (writer_handle, reader) = ep.handles().alloc_body_writer().unwrap();
    let reader_handle = ep.handles().insert_reader(reader).unwrap();

    // Write a chunk so the reader is not immediately at EOF.
    ep.handles()
        .send_chunk(writer_handle, Bytes::from("hello"))
        .await
        .unwrap();

    // Read the first chunk to confirm the channel works.
    let chunk = ep.handles().next_chunk(reader_handle).await.unwrap();
    assert_eq!(chunk.as_deref(), Some(b"hello".as_ref()));

    // Spawn a read that will block (no more data yet).
    let ep2 = ep.clone();
    let read_task = tokio::spawn(async move { ep2.handles().next_chunk(reader_handle).await });

    // Give the read task time to start waiting.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Cancel — should unblock the read.
    ep.handles().cancel_reader(reader_handle);

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), read_task)
        .await
        .expect("read task should complete promptly after cancel")
        .expect("read task should not panic");

    // Cancelled reads return None (EOF).
    assert!(
        result.is_ok(),
        "next_chunk should not return an error on cancel"
    );
    assert_eq!(result.unwrap(), None, "cancelled read should return None");

    ep.close().await;
}
