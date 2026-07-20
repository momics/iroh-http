//! Endpoint recovery lifecycle (#336).
//!
//! Simulates the iOS background→foreground recovery sequence at the core
//! transport level: a serving endpoint is torn down and recreated with the
//! *same supplied key*, and a peer that already holds stale pooled connection
//! state must be able to reach the recreated node again — bounded by a
//! configured timeout, never hanging indefinitely.

#![allow(clippy::disallowed_types)] // test file — RequestPayload is valid here
mod common;

use std::net::SocketAddr;
use std::time::Duration;

use iroh_http_core::{
    fetch, ffi_serve, respond, IrohEndpoint, NetworkingOptions, NodeOptions, RequestPayload,
    ServeOptions,
};

fn loopback_opts_with_key(key: [u8; 32]) -> NodeOptions {
    NodeOptions {
        key: Some(key),
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Serve a handler that answers every request with `200` and an empty body.
fn serve_ok(ep: &IrohEndpoint) {
    let sep = ep.clone();
    ffi_serve(
        sep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            respond(
                sep.handles(),
                payload.req_handle,
                200,
                vec![("content-length".into(), "0".into())],
            )
            .unwrap();
            sep.handles().finish_body(payload.res_body_handle).unwrap();
        },
    );
}

/// Fetch with a bounded per-attempt timeout, retrying a few times so a stale
/// pooled connection that has not yet observed its peer's close is evicted and
/// replaced with a fresh one. Mirrors the mobile guidance: bounded timeouts +
/// retry, never an unbounded hang.
async fn fetch_ok_bounded(
    client: &IrohEndpoint,
    server_id: &str,
    addrs: &[SocketAddr],
) -> Result<u16, String> {
    let mut last_err = String::new();
    for _ in 0..5 {
        match fetch(
            client,
            server_id,
            "/ok",
            "GET",
            &[],
            None,
            None,
            Some(addrs),
            Some(Duration::from_millis(500)),
            true,
            None,
        )
        .await
        {
            Ok(res) => return Ok(res.status),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(last_err)
}

/// Recreating a node with the same key keeps the same public key and becomes
/// reachable again from a peer that still holds stale pooled state.
#[tokio::test]
async fn recreate_same_key_is_reachable_through_stale_pool() {
    let key = [9u8; 32];

    // Client endpoint reused across both server incarnations — it accumulates
    // pooled connection state keyed by the server's (stable) node id.
    let client = IrohEndpoint::bind(NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap();

    // ── First incarnation ────────────────────────────────────────────────────
    let server1 = IrohEndpoint::bind(loopback_opts_with_key(key))
        .await
        .unwrap();
    let server_id = server1.node_id().to_string();
    let addrs1 = common::server_addrs(&server1);
    serve_ok(&server1);

    let status = fetch_ok_bounded(&client, &server_id, &addrs1)
        .await
        .expect("first fetch should succeed");
    assert_eq!(status, 200);

    // ── Teardown (simulates the transport dying under iOS suspension) ─────────
    server1.close_force().await;

    // ── Recreate with the SAME key ───────────────────────────────────────────
    let server2 = IrohEndpoint::bind(loopback_opts_with_key(key))
        .await
        .unwrap();
    assert_eq!(
        server2.node_id(),
        server_id,
        "recreated node must keep the same public key"
    );
    let addrs2 = common::server_addrs(&server2);
    serve_ok(&server2);

    // The client still has stale pooled state for `server_id`. A bounded fetch
    // using fresh address info must recover rather than hang forever.
    let status = fetch_ok_bounded(&client, &server_id, &addrs2)
        .await
        .expect("fetch after recreate should recover and succeed");
    assert_eq!(status, 200);
}
