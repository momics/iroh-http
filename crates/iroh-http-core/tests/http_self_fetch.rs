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
use iroh_http_core::{fetch, ffi_serve, respond, IrohEndpoint, NetworkingOptions, NodeOptions};
use iroh_http_core::{RequestPayload, ServeOptions};

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
