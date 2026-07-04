//! ADR-015 / issue #264 — self-request loopback.
//!
//! A node fetching its own node id cannot dial itself over QUIC (iroh's
//! transport refuses self-connection). [`iroh_http_core::fetch`] therefore
//! short-circuits self-targeted requests into the node's own serve service,
//! in-process. These tests exercise serve + self-fetch on a **single**
//! endpoint (acceptance criterion #4), plus the clear-error path when no
//! server is registered (acceptance criterion #2).

#![allow(clippy::disallowed_types)] // test file — RequestPayload and friends are valid here
mod common;

use bytes::Bytes;
use iroh_http_core::{
    fetch, ffi_serve, respond, ErrorCode, IrohEndpoint, NetworkingOptions, NodeOptions,
};
use iroh_http_core::{RequestPayload, ServeOptions};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Bind a single loopback-only endpoint (relay disabled).
async fn single_node() -> IrohEndpoint {
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

/// Serve an echo handler that returns `path` in the body and the
/// authenticated `peer-id` it observed in a response header.
fn serve_echo(ep: &IrohEndpoint) {
    let server_ep = ep.clone();
    ffi_serve(
        ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            let path = payload
                .url
                .split("://")
                .nth(1)
                .and_then(|s| s.find('/').map(|i| s[i..].to_string()))
                .unwrap_or_else(|| "/".to_string());
            let peer = payload.remote_node_id.clone();

            respond(
                server_ep.handles(),
                payload.req_handle,
                200,
                vec![
                    ("content-type".into(), "text/plain".into()),
                    ("x-observed-peer".into(), peer),
                ],
            )
            .unwrap();

            let handle = payload.res_body_handle;
            let server_ep = server_ep.clone();
            tokio::spawn(async move {
                server_ep
                    .handles()
                    .send_chunk(handle, Bytes::from(path.into_bytes()))
                    .await
                    .unwrap();
                server_ep.handles().finish_body(handle).unwrap();
            });
        },
    );
}

/// A node fetching its own id round-trips through its own serve service,
/// in-process, and the handler observes the node's own id as the peer.
#[tokio::test]
async fn self_fetch_round_trips_in_process() {
    let ep = single_node().await;
    let own_id = ep.node_id().to_string();
    serve_echo(&ep);

    let res = fetch(
        &ep,
        &own_id,
        "/loopback",
        "GET",
        &[],
        None,
        None,
        None, // no direct addrs — never touches the wire
        None,
        true,
        None,
    )
    .await
    .expect("self-fetch should succeed when serving");

    assert_eq!(res.status, 200);

    // The authenticated peer the handler saw is this node itself — truthful,
    // since the request never left the process.
    let observed = res
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-observed-peer"))
        .map(|(_, v)| v.clone())
        .expect("x-observed-peer header present");
    assert_eq!(observed, own_id, "self-request peer id should be own id");

    let mut body = Vec::new();
    while let Some(chunk) = ep.handles().next_chunk(res.body_handle).await.unwrap() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(String::from_utf8(body).unwrap(), "/loopback");
}

/// Fetching your own id with no server registered fails with a clear,
/// actionable error — not the opaque upstream self-dial refusal.
#[tokio::test]
async fn self_fetch_without_server_errors_clearly() {
    let ep = single_node().await;
    let own_id = ep.node_id().to_string();

    let err = fetch(
        &ep,
        &own_id,
        "/loopback",
        "GET",
        &[],
        None,
        None,
        None,
        None,
        true,
        None,
    )
    .await
    .expect_err("self-fetch without a server should error");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("self-request") || msg.contains("no active server"),
        "error should explain the missing server, got: {msg}"
    );
}

/// A self-request that times out must release its fetch cancellation token
/// before propagating the error — a leak would accumulate handles on every
/// timed-out request. Regression guard for the `?`-before-cleanup bug.
#[tokio::test]
async fn self_fetch_timeout_releases_token() {
    let ep = single_node().await;
    let own_id = ep.node_id().to_string();
    // Handler that accepts the request but never responds → forces timeout.
    let server_ep = ep.clone();
    ffi_serve(
        ep.clone(),
        ServeOptions::default(),
        move |_payload: RequestPayload| {
            let _ = &server_ep; // hold handler open; never call respond()
        },
    );

    for _ in 0..20 {
        let token = ep.handles().alloc_fetch_token().unwrap();
        let err = fetch(
            &ep,
            &own_id,
            "/slow",
            "GET",
            &[],
            None,
            Some(token),
            None,
            Some(std::time::Duration::from_millis(50)),
            true,
            None,
        )
        .await
        .expect_err("self-fetch should time out");
        assert!(
            err.to_string().to_lowercase().contains("timed out"),
            "expected timeout error, got: {err}"
        );
        assert!(
            ep.handles().get_fetch_cancel_notify(token).is_none(),
            "fetch token leaked after timeout"
        );
    }
}

/// Regression: #284 — cancelling an in-flight self-request must release its
/// fetch cancellation token before propagating `Cancelled`.
#[tokio::test]
async fn self_fetch_cancel_releases_token() {
    let ep = single_node().await;
    let own_id = ep.node_id().to_string();
    // Handler that accepts the request but never responds → fetch hangs until cancelled.
    let server_ep = ep.clone();
    ffi_serve(
        ep.clone(),
        ServeOptions::default(),
        move |_payload: RequestPayload| {
            let _ = &server_ep; // hold handler open; never call respond()
        },
    );

    let token = ep.handles().alloc_fetch_token().unwrap();
    let fetch_ep = ep.clone();
    let fetch_own_id = own_id.clone();
    let task = tokio::spawn(async move {
        fetch(
            &fetch_ep,
            &fetch_own_id,
            "/slow",
            "GET",
            &[],
            None,
            Some(token),
            None,
            None,
            true,
            None,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(75)).await;
    ep.handles().cancel_in_flight(token);

    let err = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("cancelled self-fetch task should complete")
        .expect("self-fetch task should not panic")
        .expect_err("self-fetch should be cancelled");
    assert_eq!(err.code, ErrorCode::Cancelled);
    assert!(
        ep.handles().get_fetch_cancel_notify(token).is_none(),
        "fetch token leaked after cancellation"
    );
}

/// After the server stops, a self-request again fails cleanly rather than
/// dispatching to a torn-down handler.
#[tokio::test]
async fn self_fetch_after_stop_errors_clearly() {
    let ep = single_node().await;
    let own_id = ep.node_id().to_string();
    serve_echo(&ep);

    // Sanity: works while serving.
    fetch(
        &ep,
        &own_id,
        "/loopback",
        "GET",
        &[],
        None,
        None,
        None,
        None,
        true,
        None,
    )
    .await
    .expect("self-fetch ok while serving");

    ep.stop_serve();

    let err = fetch(
        &ep,
        &own_id,
        "/loopback",
        "GET",
        &[],
        None,
        None,
        None,
        None,
        true,
        None,
    )
    .await
    .expect_err("self-fetch after stop_serve should error");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("self-request") || msg.contains("no active server"),
        "error should explain the missing server, got: {msg}"
    );
}

/// Regression: #282 — the endpoint self-request slot must not strongly retain
/// the locally-running FFI service. Draining the returned serve handle without
/// going through endpoint teardown does not explicitly clear the slot, so this
/// proves the slot itself cannot keep the dispatcher callback alive.
#[tokio::test]
async fn local_service_slot_holds_weak_no_leak() {
    struct DropSentinel(Arc<AtomicBool>);

    impl Drop for DropSentinel {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let ep = single_node().await;
    let dropped = Arc::new(AtomicBool::new(false));
    let sentinel = DropSentinel(dropped.clone());

    let handle = ffi_serve(ep.clone(), ServeOptions::default(), move |_payload| {
        let _keep_alive = &sentinel;
    });

    drop(ep);
    handle.drain().await;
    tokio::task::yield_now().await;

    assert!(
        dropped.load(Ordering::SeqCst),
        "local_service retained the dispatcher callback after the serve task exited"
    );
}
