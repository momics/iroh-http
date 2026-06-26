#![allow(clippy::disallowed_types)] // test file — RequestPayload and friends are valid here
//! Resilience / chaos tests (issue #205).
//!
//! These exercise degraded conditions a peer-to-peer HTTP library hits in
//! production: backpressure saturation, handle/resource leaks under churn,
//! mid-stream client disconnect, and accept-loop abuse under a connect/reset
//! flood. Ordering is driven by `tokio::sync::Notify` and single-poll
//! inspection (`futures::poll!`); time-based waits are bounded failsafes or
//! convergence checks, never the mechanism under test.
//!
//! Out of scope here (tracked separately under #205's remediation list): the
//! discovery-crate resilience scenarios (interface-down, duplicate announce,
//! stale-cache), which live in `iroh-http-discovery`.

mod common;

use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use iroh_http_core::{
    fetch, ffi_serve, respond, IrohEndpoint, NetworkingOptions, NodeOptions, RequestPayload,
    ServeOptions,
};
use tokio::sync::Notify;

/// Spin up a fresh loopback client endpoint (relay disabled).
async fn bind_client() -> IrohEndpoint {
    IrohEndpoint::bind(NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap()
}

/// Poll an endpoint's live-handle count until it returns to `baseline` (±1),
/// sweeping each iteration. Returns the final observed total. Bounded so a
/// genuine leak still fails the caller's assertion rather than hanging.
async fn await_handle_baseline(ep: &IrohEndpoint, baseline: usize) -> usize {
    let mut total = ep.handles().count_handles().3;
    for _ in 0..200 {
        if total <= baseline + 1 {
            return total;
        }
        ep.sweep_now();
        tokio::time::sleep(Duration::from_millis(10)).await;
        total = ep.handles().count_handles().3;
    }
    total
}

// ── 1. Backpressure: a slow consumer blocks the sender (AC #1) ────────────────

/// A body writer must apply backpressure: once the bounded channel is full and
/// the consumer has read nothing, `send_chunk` parks instead of buffering
/// unbounded, and resumes the moment the consumer drains a slot.
#[tokio::test]
async fn backpressure_slow_consumer_blocks_sender() {
    let ep = bind_client().await;
    let handles = ep.handles();

    // Owned reader = the (currently idle) consumer; writer handle = the sender.
    let (writer, reader) = handles.alloc_body_writer().unwrap();

    // Buffer chunks until the sender parks. A bounded library buffers at most
    // ~channel_capacity chunks before applying backpressure; an unbounded one
    // would never return Pending and trip the guard below.
    let mut buffered = 0usize;
    let blocked = loop {
        let mut fut = Box::pin(handles.send_chunk(writer, Bytes::from_static(b"chunk")));
        match futures::poll!(fut.as_mut()) {
            Poll::Ready(res) => {
                res.expect("buffered send should succeed while slots remain");
                buffered += 1;
                assert!(
                    buffered <= 1024,
                    "sender buffered {buffered} chunks without backpressure — unbounded growth"
                );
            }
            Poll::Pending => break fut,
        }
    };

    assert!(
        buffered > 0,
        "expected at least one chunk to buffer before the sender parked"
    );

    // The sender is parked because the consumer hasn't read anything yet.
    // Drain exactly one chunk — that frees one slot.
    let first = reader.next_chunk().await;
    assert!(
        first.is_some(),
        "consumer should read the first buffered chunk"
    );

    // The previously-parked send must now make progress and complete.
    tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .expect("parked send should complete once the consumer frees a slot")
        .expect("send should succeed after a slot is freed");
}

// ── 2. Handle count returns to baseline after churn (AC #2) ───────────────────

/// Repeated create/serve/fetch/close cycles must not leak handles: the live
/// handle count returns to its pre-traffic baseline once requests settle.
#[tokio::test]
async fn handle_count_returns_to_baseline_after_churn() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    let serve_ep = server_ep.clone();
    let _handle = ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            let ep = serve_ep.clone();
            tokio::spawn(async move {
                let req_h = payload.req_handle;
                let req_body_h = payload.req_body_handle;
                let res_h = payload.res_body_handle;
                // A well-behaved handler drains the inbound body, releasing the
                // request-body reader handle instead of leaving it for the TTL
                // sweep.
                if req_body_h != 0 {
                    while ep
                        .handles()
                        .next_chunk(req_body_h)
                        .await
                        .ok()
                        .flatten()
                        .is_some()
                    {}
                }
                respond(
                    ep.handles(),
                    req_h,
                    200,
                    vec![("content-length".into(), "2".into())],
                )
                .unwrap();
                ep.handles()
                    .send_chunk(res_h, Bytes::from_static(b"ok"))
                    .await
                    .unwrap();
                ep.handles().finish_body(res_h).unwrap();
            });
        },
    );

    let server_baseline = server_ep.handles().count_handles().3;
    let client_baseline = client_ep.handles().count_handles().3;

    for _ in 0..20 {
        let res = fetch(
            &client_ep,
            &server_id,
            "/echo",
            "GET",
            &[],
            None,
            None,
            Some(&addrs),
            None,
            true,
            None,
        )
        .await
        .expect("fetch should succeed");
        assert_eq!(res.status, 200);
        // Drain the response body fully so both sides release their handles.
        while client_ep
            .handles()
            .next_chunk(res.body_handle)
            .await
            .unwrap()
            .is_some()
        {}
    }

    let server_total = await_handle_baseline(&server_ep, server_baseline).await;
    assert!(
        server_total <= server_baseline + 1,
        "server leaked handles: {server_total} (baseline {server_baseline})"
    );
    let client_total = await_handle_baseline(&client_ep, client_baseline).await;
    assert!(
        client_total <= client_baseline + 1,
        "client leaked handles: {client_total} (baseline {client_baseline})"
    );
}

// ── 3. Mid-stream client disconnect → server cleanup (AC #3) ──────────────────

/// When the client drops mid-stream, the server's `send_chunk` must surface an
/// error (not hang forever), and the server-side response handle must be
/// released rather than leaked.
#[tokio::test]
async fn midstream_client_disconnect_errors_server_and_cleans_up() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    let streaming = std::sync::Arc::new(Notify::new());
    let streaming_tx = streaming.clone();
    let send_errored = std::sync::Arc::new(Notify::new());
    let send_errored_tx = send_errored.clone();

    let serve_ep = server_ep.clone();
    let server_baseline = server_ep.handles().count_handles().3;
    let _handle = ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            let ep = serve_ep.clone();
            let started = streaming_tx.clone();
            let errored = send_errored_tx.clone();
            tokio::spawn(async move {
                let req_h = payload.req_handle;
                let res_h = payload.res_body_handle;
                respond(ep.handles(), req_h, 200, vec![]).unwrap();
                started.notify_one();
                // Stream until the consumer goes away; capture the disconnect.
                let chunk = Bytes::from(vec![0u8; 16 * 1024]);
                loop {
                    if ep.handles().send_chunk(res_h, chunk.clone()).await.is_err() {
                        errored.notify_one();
                        break;
                    }
                }
                // Best-effort: release the writer handle deterministically.
                let _ = ep.handles().finish_body(res_h);
            });
        },
    );

    let res = fetch(
        &client_ep,
        &server_id,
        "/stream",
        "GET",
        &[],
        None,
        None,
        Some(&addrs),
        None,
        true,
        None,
    )
    .await
    .expect("fetch head should arrive");
    assert_eq!(res.status, 200);

    // Wait for the server to actually be streaming, read a couple of chunks to
    // confirm flow, then abruptly drop the client (simulated network loss).
    streaming.notified().await;
    for _ in 0..2 {
        let _ = client_ep.handles().next_chunk(res.body_handle).await;
    }
    client_ep.close_force().await;

    // The server's send must error within a bounded window, never hang.
    tokio::time::timeout(Duration::from_secs(15), send_errored.notified())
        .await
        .expect("server send_chunk should error after the client disconnects");

    // The server-side handle must be released, not leaked.
    let server_total = await_handle_baseline(&server_ep, server_baseline).await;
    assert!(
        server_total <= server_baseline + 1,
        "server leaked handles after disconnect: {server_total} (baseline {server_baseline})"
    );
}

// ── 4. Accept loop survives a rapid connect/reset flood (AC #4) ───────────────

/// A burst of clients that connect and immediately reset must not kill the
/// accept loop: a clean request still succeeds afterwards.
#[tokio::test]
async fn accept_loop_survives_connect_reset_flood() {
    let (server_ep, clean_client) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    let serve_ep = server_ep.clone();
    let _handle = ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            let ep = serve_ep.clone();
            tokio::spawn(async move {
                let req_h = payload.req_handle;
                let res_h = payload.res_body_handle;
                respond(
                    ep.handles(),
                    req_h,
                    200,
                    vec![("content-length".into(), "2".into())],
                )
                .unwrap();
                let _ = ep
                    .handles()
                    .send_chunk(res_h, Bytes::from_static(b"ok"))
                    .await;
                let _ = ep.handles().finish_body(res_h);
            });
        },
    );

    // Flood: short-lived clients that start a request then reset almost
    // immediately, exercising the accept loop's per-connection error path.
    let mut floods = Vec::new();
    for _ in 0..16 {
        let sid = server_id.clone();
        let a = addrs.clone();
        floods.push(tokio::spawn(async move {
            let c = bind_client().await;
            let _ = tokio::time::timeout(
                Duration::from_millis(50),
                fetch(
                    &c,
                    &sid,
                    "/",
                    "GET",
                    &[],
                    None,
                    None,
                    Some(&a),
                    None,
                    true,
                    None,
                ),
            )
            .await;
            c.close_force().await;
        }));
    }
    for f in floods {
        let _ = f.await;
    }

    // The accept loop must still be alive — a clean request goes through.
    let res = tokio::time::timeout(
        Duration::from_secs(15),
        fetch(
            &clean_client,
            &server_id,
            "/",
            "GET",
            &[],
            None,
            None,
            Some(&addrs),
            None,
            true,
            None,
        ),
    )
    .await
    .expect("clean fetch should not hang after the flood")
    .expect("clean fetch should succeed after the flood");
    assert_eq!(res.status, 200);
    while clean_client
        .handles()
        .next_chunk(res.body_handle)
        .await
        .unwrap()
        .is_some()
    {}
}
