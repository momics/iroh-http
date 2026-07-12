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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh_http_core::{fetch, ffi_serve, respond, IrohEndpoint, RequestPayload, ServeOptions};

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
            client,
            server_id,
            path,
            "GET",
            &[],
            None,
            None,
            Some(addrs),
            None,
            true,
            None,
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

/// A single fetch with NO external retry loop, returning the routed status or
/// the error. Used to prove that `fetch_request`'s *internal* transparent
/// retry recovers a pooled connection the server force-closed on serve-replace
/// — the caller must not need its own retry.
async fn single_fetch(
    client: &IrohEndpoint,
    server_id: &str,
    addrs: &[std::net::SocketAddr],
    path: &str,
) -> Result<u16, String> {
    single_fetch_method(client, server_id, addrs, path, "GET").await
}

/// Like [`single_fetch`] but with a caller-chosen HTTP method and NO body, so a
/// non-idempotent method (POST/PATCH) can be exercised against the transparent
/// retry gate.
async fn single_fetch_method(
    client: &IrohEndpoint,
    server_id: &str,
    addrs: &[std::net::SocketAddr],
    path: &str,
    method: &str,
) -> Result<u16, String> {
    fetch(
        client,
        server_id,
        path,
        method,
        &[],
        None,
        None,
        Some(addrs),
        None,
        true,
        None,
    )
    .await
    .map(|res| res.status)
    .map_err(|e| e.to_string())
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
    let waited =
        tokio::time::timeout(Duration::from_millis(400), server_ep.wait_serve_stop()).await;
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
            client,
            server_id,
            path,
            "GET",
            &[],
            None,
            None,
            Some(addrs),
            None,
            true,
            None,
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
    let status = fetch_once_status(
        &client_ep,
        &server_id,
        &addrs,
        "/hello",
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(
        status, None,
        "request was answered (status {status:?}) after stop_serve() — the \
         stopped serve loop is still serving on a pooled connection (the \
         on-device \"serve keeps answering post-stop\" bug)",
    );
}

/// A fetch on a pooled connection that a serve-replace has force-closed must be
/// transparently retried on a fresh connection — the caller should NOT have to
/// retry. Regression for #119: the #336 replace-path `shutdown_and_close`
/// severs the client's pooled QUIC connection at the top of the next serve
/// iteration, so a request reusing that connection races a close. Before the
/// client-side single-retry fix this surfaced as a hard
/// `NetworkError: ... serve loop replaced` / `send_request: connection error`.
#[tokio::test]
async fn fetch_retries_pooled_connection_closed_by_serve_replace() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // Warm the pool: first serve loop + one request establishes a pooled
    // client→server QUIC connection that later iterations will try to reuse.
    serve_router(&server_ep);
    assert_eq!(
        single_fetch(&client_ep, &server_id, &addrs, "/status/200")
            .await
            .expect("warm-up fetch should succeed"),
        200,
    );

    // Each iteration replaces the serve loop (force-closing the pooled
    // connection, exactly like the #119 burst test) then fires a SINGLE fetch
    // with no external retry. Every one must still route correctly.
    for iter in 0..10 {
        // Replace the serve loop: set_serve_handle → shutdown_and_close on the
        // previous handle severs the client's pooled connection.
        serve_router(&server_ep);

        let status = single_fetch(&client_ep, &server_id, &addrs, "/status/503")
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "iter {iter}: single fetch after serve-replace failed ({e}) — \
                     the client did not transparently retry the pooled connection \
                     the server force-closed (#119)"
                )
            });
        assert_eq!(
            status, 503,
            "iter {iter}: fetch after serve-replace must route to 503, got {status}",
        );
    }
}

/// F1 (lost close-wakeup): after a serve-replace force-closes active
/// connections, an **idle** pooled connection must actually be torn down, so a
/// later request over it can never be served by the stale/replaced loop.
///
/// The per-connection task parks in `accept_bi()`. Teardown sets `close_flag`
/// then `close_connections.notify_waiters()`, which stores no permit and only
/// wakes already-registered waiters. If the task loaded the flag *before*
/// registering its waiter, the store+notify could land in that gap and be lost
/// — the idle task would then sleep forever in `accept_bi()`, never re-check the
/// flag, and keep serving the stale handler over the pooled connection. The fix
/// registers the waiter (`Notified::enable()`) before the flag load. This test
/// establishes an idle pooled connection, replaces the loop, and asserts the
/// next fetch routes to the NEW handler (not the stale one).
#[tokio::test]
async fn idle_pooled_connection_closed_on_serve_replace() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // Stale loop answers everything 200; warm a pooled connection so its
    // per-connection task is left parked idle in `accept_bi()`.
    serve_constant(&server_ep, 200);
    assert_eq!(
        single_fetch(&client_ep, &server_id, &addrs, "/hello")
            .await
            .expect("warm-up fetch should succeed"),
        200,
    );

    // Let the per-connection task settle back into its idle wait so the replace
    // below hits the exact "parked in accept_bi" state the F1 race needs.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Replace the loop with one that answers 503. This force-closes the idle
    // pooled connection; the parked task MUST wake and close it.
    serve_constant(&server_ep, 503);

    // Give the wake+close a moment to propagate before re-fetching.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The next request must route to the NEW loop. A 200 here means the idle
    // connection was never closed (lost wakeup) and the stale loop still serves.
    let status = single_fetch(&client_ep, &server_id, &addrs, "/hello")
        .await
        .expect("fetch after replace should succeed (fresh connection)");
    assert_eq!(
        status, 503,
        "idle pooled connection was still served by the stale loop after \
         serve-replace (got {status}) — the close wakeup was lost (F1)",
    );
}

/// F2 (lost-stop race): `stop_serve()` fired concurrently with a serve restart
/// must never be dropped, and must never leave `serve_stopped_early` stuck true
/// (which would spuriously shut down the NEXT healthy serve loop).
///
/// Best-effort stress test: repeatedly race `stop_serve()` against a serve
/// restart to exercise the window where `set_serve_handle` momentarily holds no
/// registered handle. Afterwards a fresh serve must be fully healthy (routes
/// correctly, not immediately shut down) and still stoppable — proving no stop
/// was lost into a stuck flag.
#[tokio::test]
async fn stop_serve_racing_restart_never_loses_stop() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    serve_router(&server_ep);

    for _ in 0..25 {
        // Race a stop against a restart on the same endpoint.
        let racer = server_ep.clone();
        let stopper = tokio::spawn(async move { racer.stop_serve() });
        serve_router(&server_ep);
        stopper.await.unwrap();

        // Whoever won, quiesce to a known-stopped state before the next round.
        server_ep.stop_serve();
        let _ = tokio::time::timeout(Duration::from_secs(5), server_ep.wait_serve_stop()).await;
    }

    // After all the racing, a clean serve must be fully alive — if a lost stop
    // had left `serve_stopped_early` stuck true, this loop would be shut down
    // the instant it registers and the fetch would never succeed.
    serve_router(&server_ep);
    assert_eq!(
        fetch_status(&client_ep, &server_id, &addrs, "/status/503").await,
        503,
        "a fresh serve loop was not healthy after concurrent stop/restart — \
         a lost stop left serve_stopped_early stuck, spuriously stopping it (F2)",
    );

    // And it must still stop cleanly.
    server_ep.stop_serve();
    tokio::time::timeout(Duration::from_secs(5), server_ep.wait_serve_stop())
        .await
        .expect("serve loop should stop after the racing rounds");
}

/// P1 (data-corruption): a non-idempotent request (empty-body POST) that fails
/// on a REUSED pooled connection must NOT be transparently retried. The #119
/// retry recovers GET (idempotent) transparently, but replaying a POST the
/// peer's old serve loop may already have processed would double-execute it
/// (RFC 9110 §9.2.2 permits replay only for idempotent methods). The FFI builds
/// `Body::empty()` for any method, so body-emptiness alone must not authorize a
/// resend.
///
/// Setup mirrors the GET retry test: warm a pooled connection, then replace the
/// serve loop (force-closing that connection) and fire a POST with no external
/// retry. On the iterations where attempt 1 reuses the just-closed connection
/// and fails with `ConnectionFailed`, a correct client surfaces the error
/// instead of retrying — so at least one POST must return `Err`. Before the
/// idempotency guard every such POST was silently retried onto the new loop and
/// returned `Ok(200)`, so `saw_error` stayed false (RED).
#[tokio::test]
async fn post_not_retried_on_pooled_connection_closed_by_serve_replace() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    serve_router(&server_ep);
    single_fetch_method(&client_ep, &server_id, &addrs, "/status/200", "GET")
        .await
        .expect("warm-up GET should succeed");

    let mut saw_error = false;
    for _ in 0..12 {
        // Replace the serve loop → shutdown_and_close force-closes the pooled
        // connection, so the next reuse races a closed connection.
        serve_router(&server_ep);

        if single_fetch_method(&client_ep, &server_id, &addrs, "/status/200", "POST")
            .await
            .is_err()
        {
            saw_error = true;
        }
    }

    assert!(
        saw_error,
        "an empty-body POST that failed on a reused pooled connection was \
         transparently retried instead of surfacing ConnectionFailed — a \
         non-idempotent request must never be replayed, or the peer double- \
         executes it (P1 / RFC 9110 §9.2.2)",
    );
}

/// P1 (at-most-once, stronger than the client-error assertion above): prove by
/// SERVER-SIDE execution count that a non-idempotent request is executed at
/// most once even in the dangerous window the idempotency gate closes — where
/// attempt 1 REACHES the peer and the handler RUNS, and only then the
/// connection is severed so the client sees `ConnectionFailed`. Without the
/// gate the client transparently replays the POST onto the new serve loop →
/// the handler runs a SECOND time → double execution / data corruption.
///
/// To make that window deterministic (rather than racing the #119 close) the
/// test uses two serve generations:
///   * a SLOW loop whose handler increments the counter, signals the test, then
///     stalls without responding — so attempt 1 has provably executed on the
///     peer but its response never arrives;
///   * a FAST loop (installed by the replace, which force-closes attempt 1's
///     connection) whose handler increments and responds immediately — where a
///     buggy replay lands and executes a second time.
/// With the gate the POST is never replayed: the counter stays at 1 and the
/// client surfaces the error. RED before the gate: the counter reaches 2.
#[tokio::test]
async fn post_executed_at_most_once_across_serve_replace() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // Executions of the target POST path, across both serve generations.
    let exec_count = Arc::new(AtomicUsize::new(0));
    // Fired by the slow handler once it has received (and counted) the request.
    let received = Arc::new(tokio::sync::Notify::new());

    // Generation 1: slow. Non-target paths (warm-up) answer immediately; the
    // target path counts, signals, then stalls so attempt 1 executes on the
    // peer but never gets a response.
    {
        let handler_ep = server_ep.clone();
        let exec = exec_count.clone();
        let received = received.clone();
        let handle = ffi_serve(
            server_ep.clone(),
            ServeOptions::default(),
            move |payload: RequestPayload| {
                let ep = handler_ep.clone();
                let exec = exec.clone();
                let received = received.clone();
                tokio::spawn(async move {
                    if payload.url.contains("/exec/target") {
                        exec.fetch_add(1, Ordering::SeqCst);
                        received.notify_one();
                        // Stall: attempt 1 has executed but its response never
                        // arrives, so the replace below severs it mid-request.
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                    respond(ep.handles(), payload.req_handle, 200, vec![]).unwrap();
                    ep.handles().finish_body(payload.res_body_handle).unwrap();
                });
            },
        );
        server_ep.set_serve_handle(handle);
    }

    // Warm a pooled connection so attempt 1 reuses it (reused == true, the only
    // case the transparent retry fires for).
    single_fetch_method(&client_ep, &server_id, &addrs, "/warm", "GET")
        .await
        .expect("warm-up GET should succeed");

    // Fire the POST; it will reach the slow loop and stall.
    let client = client_ep.clone();
    let sid = server_id.clone();
    let addrs_c = addrs.clone();
    let post_task = tokio::spawn(async move {
        single_fetch_method(&client, &sid, &addrs_c, "/exec/target", "POST").await
    });

    // Wait until the peer has provably executed attempt 1.
    received.notified().await;

    // Generation 2: fast. Installing it force-closes attempt 1's connection
    // (shutdown_and_close), so attempt 1's `svc.oneshot` fails with
    // ConnectionFailed *after* the handler already ran. A buggy replay lands
    // here and executes a second time.
    {
        let handler_ep = server_ep.clone();
        let exec = exec_count.clone();
        let handle = ffi_serve(
            server_ep.clone(),
            ServeOptions::default(),
            move |payload: RequestPayload| {
                let ep = handler_ep.clone();
                let exec = exec.clone();
                tokio::spawn(async move {
                    if payload.url.contains("/exec/target") {
                        exec.fetch_add(1, Ordering::SeqCst);
                    }
                    respond(ep.handles(), payload.req_handle, 200, vec![]).unwrap();
                    ep.handles().finish_body(payload.res_body_handle).unwrap();
                });
            },
        );
        server_ep.set_serve_handle(handle);
    }

    // The POST resolves (Err with the gate; Ok via replay without it).
    let _ = tokio::time::timeout(Duration::from_secs(5), post_task)
        .await
        .expect("POST task did not settle within 5s");

    // Allow any (buggy) replay to reach and execute on the fast loop.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        exec_count.load(Ordering::SeqCst),
        1,
        "the POST executed on the server more than once — a non-idempotent \
         request was transparently replayed across a serve replace after it had \
         already run on the peer, double-executing it (P1 / RFC 9110 §9.2.2)",
    );
}

/// M1 / F3 (drain-then-close must not truncate): a request that is IN FLIGHT
/// when `stop_serve()` is called must complete uncorrupted. Graceful stop
/// drains in-flight requests before force-closing connections; if the drain or
/// the `no_new_streams`/close ordering were wrong, the force-close could sever
/// the connection mid-response and truncate (or error) the in-flight request.
///
/// The handler signals when it has begun (request now in-flight), then streams
/// a known multi-chunk body after a delay. The test waits for that signal, calls
/// `stop_serve()` while the request is mid-flight, then asserts the client still
/// receives the FULL, byte-exact body and a 200 — never a truncated body or a
/// connection error.
#[tokio::test]
async fn in_flight_request_completes_uncorrupted_across_stop() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    // A recognizable, chunk-spanning body so any truncation is detectable.
    let expected: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    let expected_handler = expected.clone();

    let started = Arc::new(tokio::sync::Notify::new());
    let started_handler = started.clone();

    let handler_ep = server_ep.clone();
    let handle = ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            let ep = handler_ep.clone();
            let started = started_handler.clone();
            let body = expected_handler.clone();
            tokio::spawn(async move {
                // Signal the test that the request is now in-flight, then hold
                // it in-flight so `stop_serve()` lands mid-request and the drain
                // has to wait for us.
                started.notify_one();
                tokio::time::sleep(Duration::from_millis(400)).await;
                respond(ep.handles(), payload.req_handle, 200, vec![]).unwrap();
                // Stream the body in two chunks so a mid-response close would
                // truncate it.
                let res = payload.res_body_handle;
                ep.handles()
                    .send_chunk(res, Bytes::from(body[..4096].to_vec()))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                ep.handles()
                    .send_chunk(res, Bytes::from(body[4096..].to_vec()))
                    .await
                    .unwrap();
                ep.handles().finish_body(res).unwrap();
            });
        },
    );
    server_ep.set_serve_handle(handle);

    // Fire the request and read its full body on a task.
    let client = client_ep.clone();
    let sid = server_id.clone();
    let addrs_c = addrs.clone();
    let fetch_task = tokio::spawn(async move {
        let res = fetch(
            &client,
            &sid,
            "/slow",
            "GET",
            &[],
            None,
            None,
            Some(&addrs_c),
            None,
            true,
            None,
        )
        .await
        .expect("in-flight fetch should return a response head, not a connection error");
        let mut body = Vec::new();
        while let Some(chunk) = client.handles().next_chunk(res.body_handle).await.unwrap() {
            body.extend_from_slice(&chunk);
        }
        (res.status, body)
    });

    // Wait until the request is actually in-flight, then stop while it runs.
    started.notified().await;
    server_ep.stop_serve();

    let (status, body) = tokio::time::timeout(Duration::from_secs(5), fetch_task)
        .await
        .expect("in-flight request did not complete within 5s after stop_serve")
        .expect("fetch task panicked");

    assert_eq!(status, 200, "in-flight request lost its status across stop");
    assert_eq!(
        body, expected,
        "in-flight response body was truncated/corrupted by the stop force-close \
         — drain-then-close must let an in-flight request finish intact (M1/F3)",
    );

    // And the loop must still stop cleanly afterwards.
    tokio::time::timeout(Duration::from_secs(5), server_ep.wait_serve_stop())
        .await
        .expect("serve loop should stop after the in-flight request drained");
}

/// Whole-fetch timeout budget (#336): a fetch that cannot reach the peer is
/// bounded by the configured timeout — the single budget wraps connect +
/// attempt + any transparent retry together, so total latency is ≈ `timeout`,
/// never a multiple of it. Guards against a regression that moves the timeout
/// inside a per-attempt scope (which would let connect+retry run up to 2×).
///
/// Points a fetch at an unroutable TEST-NET-1 address (RFC 5737, guaranteed to
/// black-hole) with a short timeout and asserts it returns a timeout well under
/// 2× the budget. (The #119 test proves the retry path itself recovers when the
/// peer IS reachable; this asserts the outer bound holds when it is not.)
#[tokio::test]
async fn fetch_is_bounded_by_configured_timeout() {
    let (_server_ep, client_ep) = common::make_pair().await;
    // A random, unknown node id at an unroutable address — the QUIC handshake
    // can never complete, so without the whole-fetch timeout this would hang.
    let unknown_id = common::node_id(&_server_ep);
    let black_hole: std::net::SocketAddr = "192.0.2.1:9".parse().unwrap();

    let budget = Duration::from_millis(800);
    let start = Instant::now();
    let res = fetch(
        &client_ep,
        &unknown_id,
        "/never",
        "GET",
        &[],
        None,
        None,
        Some(&[black_hole]),
        Some(budget),
        true,
        None,
    )
    .await;
    let elapsed = start.elapsed();

    assert!(
        res.is_err(),
        "fetch to an unreachable peer must fail, not hang"
    );
    assert!(
        elapsed < budget.saturating_mul(2),
        "fetch took {elapsed:?} — the whole-fetch timeout budget ({budget:?}) must \
         bound connect + attempt + retry together, never run to 2× (#336)",
    );
}
