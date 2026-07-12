#![allow(clippy::disallowed_types)] // test file — RequestPayload and friends are valid here
//! Regression tests for #336: restarting `serve()` on the same endpoint must
//! re-attach the app's request handler, not fall back to a default responder.
//!
//! On iOS the interop harness observed that after background→foreground the
//! serve loop came back "half-live": every routed request returned a blanket
//! 200 instead of the status the handler produced (e.g. `/status/503`). These
//! tests reproduce a serve → stop → serve cycle on a live endpoint and assert a
//! routed non-200 still works after the restart, mirroring the tauri command
//! path (`serve` → `set_serve_handle`, `stop_serve`, `wait_serve_stop`).

mod common;

use std::time::Duration;

use iroh_http_core::{
    fetch, ffi_serve, respond, IrohEndpoint, RequestPayload, ServeOptions,
};

/// Install a handler that routes `/status/<code>` to that HTTP status and
/// everything else to 418, then register it as the endpoint's serve handle
/// exactly like the tauri `serve` command does.
fn serve_router(ep: &IrohEndpoint) {
    let handler_ep = ep.clone();
    let handle = ffi_serve(
        ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            let ep = handler_ep.clone();
            tokio::spawn(async move {
                // Parse `/status/<code>` from the path; default to 418 so a
                // missing router is obvious (never a blanket 200).
                let status = payload
                    .url
                    .rsplit('/')
                    .next()
                    .and_then(|s| s.parse::<u16>().ok())
                    .filter(|_| payload.url.contains("/status/"))
                    .unwrap_or(418);
                respond(ep.handles(), payload.req_handle, status, vec![]).unwrap();
                ep.handles().finish_body(payload.res_body_handle).unwrap();
            });
        },
    );
    ep.set_serve_handle(handle);
}

/// Install a handler that answers **every** request with `status`, then
/// register it as the endpoint's serve handle (mirrors the tauri `serve`
/// command's `ep.set_serve_handle(...)`).
fn serve_constant(ep: &IrohEndpoint, status: u16) {
    let handler_ep = ep.clone();
    let handle = ffi_serve(
        ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            let ep = handler_ep.clone();
            tokio::spawn(async move {
                respond(ep.handles(), payload.req_handle, status, vec![]).unwrap();
                ep.handles().finish_body(payload.res_body_handle).unwrap();
            });
        },
    );
    ep.set_serve_handle(handle);
}

async fn fetch_status(
    client: &IrohEndpoint,
    server_id: &str,
    addrs: &[std::net::SocketAddr],
    path: &str,
) -> u16 {
    // Model a real client: a pooled connection that the server has closed
    // (serve restart / recovery) fails fast with ConnectionFailed; the client
    // evicts it and the next attempt reconnects with fresh connection state.
    // Never a silent hang past the timeout (#336).
    let mut last_err = None;
    for _ in 0..10 {
        match fetch(
            client, server_id, path, "GET", &[], None, None, Some(addrs), None, true, None,
        )
        .await
        {
            Ok(res) => return res.status,
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    panic!("fetch never succeeded: {last_err:?}");
}

/// A routed non-200 must survive a serve → stop → serve restart on the same
/// live endpoint. Regression for the iOS foreground "blanket 200" symptom.
#[tokio::test]
async fn routed_status_survives_serve_restart() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // ── First serve: router is live ─────────────────────────────────────────
    serve_router(&server_ep);
    assert_eq!(
        fetch_status(&client_ep, &server_id, &addrs, "/status/503").await,
        503,
        "baseline: router should return 503 before restart",
    );

    // ── Stop serve (mirrors tauri stop_serve + wait_serve_stop) ─────────────
    server_ep.stop_serve();
    tokio::time::timeout(Duration::from_secs(5), server_ep.wait_serve_stop())
        .await
        .expect("serve loop should stop");

    // ── Restart serve on the SAME endpoint ──────────────────────────────────
    serve_router(&server_ep);

    // The restarted loop must re-attach the same router — not answer 200.
    assert_eq!(
        fetch_status(&client_ep, &server_id, &addrs, "/status/503").await,
        503,
        "regression #336: routed 503 must still work after serve restart, \
         got a different status (blanket 200 means the handler was not re-attached)",
    );
    assert_eq!(
        fetch_status(&client_ep, &server_id, &addrs, "/status/404").await,
        404,
        "regression #336: routed 404 must still work after serve restart",
    );
}

/// The iOS "blanket 200" smoking gun: after foreground the app restarts
/// `serve()` on the same endpoint. If the previous serve loop is not shut down
/// when the new one is registered, both accept loops race on `ep.accept()` and
/// the stale handler keeps answering — so a fraction of requests get the OLD
/// response (e.g. 200) instead of the NEW routed status (e.g. 503). This
/// reproduces that nondeterministic "expected 503, got 200" symptom.
#[tokio::test]
async fn restarting_serve_replaces_stale_loop() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // ── Original serve loop: answers everything 200 (the "stale" handler) ────
    serve_constant(&server_ep, 200);
    assert_eq!(
        fetch_status(&client_ep, &server_id, &addrs, "/status/503").await,
        200,
        "baseline: original loop answers 200",
    );

    // ── Restart serve with a NEW handler, WITHOUT an explicit stop_serve ─────
    // (mirrors an app that naively calls `serve()` again on foreground). The
    // new handler answers 503; registering it must tear down the old loop.
    serve_constant(&server_ep, 503);

    // Every request must now hit the NEW loop. A stale competing loop would
    // leak 200s here.
    for i in 0..20 {
        let status = fetch_status(&client_ep, &server_id, &addrs, "/status/503").await;
        assert_eq!(
            status, 503,
            "request {i}: restarted serve must fully replace the stale loop \
             (got {status}; a 200 means the old serve loop is still competing — \
             the iOS #336 blanket-200 symptom)",
        );
    }
}
