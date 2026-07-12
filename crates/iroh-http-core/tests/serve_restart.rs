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

/// Every graceful `stop_serve()` must let `wait_serve_stop()` resolve, for
/// arbitrarily many serve/stop cycles on the SAME endpoint. Regression for the
/// iOS "serve() is already running" bug: the JS `serveRunning` guard only
/// clears once `wait_serve_stop()` (the loop-done signal) resolves, so if a
/// later cycle's done signal never fires the next `serve()` throws.
#[tokio::test]
async fn wait_serve_stop_resolves_every_cycle() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    for cycle in 0..4 {
        serve_router(&server_ep);
        assert_eq!(
            fetch_status(&client_ep, &server_id, &addrs, "/status/503").await,
            503,
            "cycle {cycle}: routed 503 must work while serving",
        );

        server_ep.stop_serve();
        tokio::time::timeout(Duration::from_secs(5), server_ep.wait_serve_stop())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "cycle {cycle}: wait_serve_stop() must resolve after stop_serve() \
                     — a stuck done signal is what leaves the JS guard set and makes \
                     the next serve() throw \"already running\""
                )
            });
    }
}

/// `wait_serve_stop()` must not resolve against a *previous* cycle's done
/// signal. The Tauri adapter issues `serve` and `wait_serve_stop` as two
/// separate, unordered IPC calls: `wait_serve_stop` for cycle N can land before
/// cycle N's `serve` registers its handle, so it would read the still-`true`
/// done signal from cycle N-1 and resolve immediately — resolving the JS
/// `finished`/`loopDone` promise while the new loop is actually still running.
/// A correct implementation waits for the *current* serve loop to stop.
#[tokio::test]
async fn wait_serve_stop_ignores_stale_done_signal() {
    let (server_ep, _client_ep) = common::make_pair().await;

    // ── Cycle A: serve then fully stop so the done signal latches `true`. ────
    serve_router(&server_ep);
    server_ep.stop_serve();
    tokio::time::timeout(Duration::from_secs(5), server_ep.wait_serve_stop())
        .await
        .expect("cycle A: wait_serve_stop should resolve");

    // ── Cycle B: start a new loop, then wait. wait_serve_stop must block on
    //    the NEW loop, not return instantly off cycle A's stale `true`. ──────
    serve_router(&server_ep);
    let waited = tokio::time::timeout(Duration::from_millis(400), server_ep.wait_serve_stop()).await;
    assert!(
        waited.is_err(),
        "wait_serve_stop resolved while the current serve loop is still running \
         — it latched a stale previous-cycle done signal (this prematurely \
         resolves the JS finished/loopDone promise and corrupts the serve guard)",
    );

    // Cleanup: stop the live loop and confirm it now resolves.
    server_ep.stop_serve();
    tokio::time::timeout(Duration::from_secs(5), server_ep.wait_serve_stop())
        .await
        .expect("cycle B: wait_serve_stop should resolve after stop_serve");
}

/// A single fetch with no reconnect retry, bounded by `timeout`. Returns the
/// routed status if the request was *answered*, or `None` if it was refused /
/// not answered within the bound. Used to assert a stopped serve loop does not
/// keep answering.
async fn fetch_once_status(
    client: &IrohEndpoint,
    server_id: &str,
    addrs: &[std::net::SocketAddr],
    path: &str,
    timeout: Duration,
) -> Option<u16> {
    match tokio::time::timeout(
        timeout,
        fetch(
            client, server_id, path, "GET", &[], None, None, Some(addrs), None, true, None,
        ),
    )
    .await
    {
        Ok(Ok(res)) => Some(res.status),
        // Connection refused/failed, or bounded-out: not answered.
        Ok(Err(_)) | Err(_) => None,
    }
}

/// An explicit `stop_serve()` must be authoritative: after it, a peer whose
/// QUIC connection is still open must NOT keep getting served. Regression for
/// the on-device iOS symptom where, after stopping the server, the desktop peer
/// kept receiving `200`s over its already-open pooled connection — requests
/// were dispatched to live per-connection tasks *seconds after* the serve loop
/// logged "all in-flight requests drained".
///
/// Root cause: graceful `stop_serve()` → `shutdown()` broke only the *accept*
/// loop (no new connections) but left the detached per-connection tasks
/// `accept_bi()`-ing on pooled connections. The fix closes active connections
/// after the drain completes (drain-then-close), so a stopped loop never
/// serves.
#[tokio::test]
async fn stop_serve_closes_active_connections() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // ── Serve, then establish a pooled connection with one served request. ───
    serve_constant(&server_ep, 200);
    assert_eq!(
        fetch_status(&client_ep, &server_id, &addrs, "/hello").await,
        200,
        "baseline: request should be served while the server is running",
    );

    // ── Stop serving (mirrors the tauri stop_serve + wait_serve_stop path). ──
    server_ep.stop_serve();
    tokio::time::timeout(Duration::from_secs(5), server_ep.wait_serve_stop())
        .await
        .expect("serve loop should stop");

    // Give the drain-then-close a moment to tear down the pooled connection,
    // then fire a request like the desktop peer does after the server stops.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The stopped loop must NOT answer over the peer's still-open pooled
    // connection. Before the fix this returned 200 (the per-connection task was
    // still alive and serving); now the connection is closed so the request is
    // refused / not answered.
    let status =
        fetch_once_status(&client_ep, &server_id, &addrs, "/hello", Duration::from_secs(3)).await;
    assert_eq!(
        status, None,
        "request was answered (status {status:?}) after stop_serve() — the \
         stopped serve loop is still serving on a pooled connection (the \
         on-device \"serve keeps answering post-stop\" bug)",
    );
}
