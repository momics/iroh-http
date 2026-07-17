//! Accept loop body extracted from `serve_with_events` so the
//! public entry stays close to the axum reference shape (≤ 200 LoC).
//!
//! `accept_loop` owns the per-endpoint loop: pull the next incoming
//! connection (or shutdown), enforce per-peer and global connection caps,
//! spawn a per-connection task that drives `accept_bi` and feeds each
//! bistream to [`crate::http::server::pipeline::serve_bistream`], then run
//! the graceful drain on shutdown.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use hyper_util::rt::TokioIo;
use tower::{limit::ConcurrencyLimitLayer, Service, ServiceBuilder, ServiceExt};

use crate::{base32_encode, http::transport::io::IrohStream, Body, IrohEndpoint, ALPN};

use super::lifecycle::{ConnectionTracker, DeliveryTracker, RequestTracker};
use super::stack::{CompressionOptions, StackConfig};
use super::{ConnectionEventFn, RemoteNodeId};

/// Tunables resolved from `ServeOptions`, passed by value to the spawned
/// loop so the caller's `ServeOptions` is not held across the await.
pub(super) struct AcceptConfig {
    pub max: usize,
    pub request_timeout: Duration,
    pub max_conns_per_peer: usize,
    pub max_request_body_wire_bytes: Option<usize>,
    pub max_request_body_decoded_bytes: Option<usize>,
    pub max_total_connections: Option<usize>,
    pub drain_timeout: Duration,
    pub load_shed_enabled: bool,
    pub max_header_size: usize,
    pub stack_compression: Option<CompressionOptions>,
    pub decompression: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn accept_loop<S>(
    endpoint: IrohEndpoint,
    cfg: AcceptConfig,
    svc: S,
    on_connection_event: Option<ConnectionEventFn>,
    shutdown_listen: Arc<tokio::sync::Notify>,
    close_flag: Arc<std::sync::atomic::AtomicBool>,
    close_connections: Arc<tokio::sync::Notify>,
    done_tx: tokio::sync::watch::Sender<bool>,
) where
    S: Service<
            hyper::Request<Body>,
            Response = hyper::Response<Body>,
            Error = std::convert::Infallible,
        > + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send + 'static,
{
    let peer_counts: Arc<Mutex<HashMap<iroh::PublicKey, usize>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let conn_event_fn: Option<ConnectionEventFn> = on_connection_event;

    // In-flight request counter: incremented on accept, decremented on drop.
    // Used for graceful drain (wait until zero or timeout).
    let in_flight: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let drain_notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());

    // F3: set when a graceful stop BEGINS (the accept loop breaks below), before
    // the drain starts. Per-connection tasks check it at the top of their loop
    // and stop pulling NEW bistreams, so a stream accepted *after* the drain
    // observes `in_flight == 0` can't slip in and get truncated by the
    // subsequent force-close. Streams already in flight when the flag is set are
    // still counted and waited for by the drain; only brand-new accepts are
    // suppressed. Distinct from `close_flag`, which severs the connection
    // outright — this one only stops new work while in-flight requests drain.
    let no_new_streams: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // SEC-002: build the concurrency limiter as a *layer* once so every
    // per-connection stack we wrap with it shares the same `Arc<Semaphore>`,
    // enforcing a true global request cap across all connections.
    let conc_layer = ConcurrencyLimitLayer::new(cfg.max);

    // Re-use the endpoint's shared counters so that endpoint_stats() reflects
    // the live connection and request counts at all times.
    let total_connections = endpoint.active_connections_arc();
    let total_requests = endpoint.active_requests_arc();
    let endpoint_closed_tx = endpoint.connection_closed_tx();

    let ep = endpoint.raw().clone();

    loop {
        let incoming = tokio::select! {
            biased;
            _ = shutdown_listen.notified() => {
                tracing::info!("iroh-http: serve loop shutting down");
                break;
            }
            inc = ep.accept() => match inc {
                Some(i) => i,
                None => {
                    tracing::info!("iroh-http: endpoint closed (accept returned None)");
                    let _ = endpoint_closed_tx.send(true);
                    break;
                }
            }
        };

        let conn = match incoming.await {
            Ok(c) => c,
            Err(e) => {
                // In QUIC, errors from awaiting an Incoming are always
                // per-connection handshake failures (peer reset, timeout,
                // protocol violation). They don't indicate systemic resource
                // exhaustion — the next incoming connection is unrelated.
                // Log and retry immediately. The serve loop is immortal:
                // it only exits on explicit shutdown or endpoint close.
                tracing::debug!("iroh-http: connection handshake error: {e}");
                continue;
            }
        };

        let remote_pk = conn.remote_id();

        // Enforce the negotiated ALPN. This is the HTTP serve loop, so a
        // connection that negotiated a different protocol (e.g. the duplex
        // session ALPN) must not be handed to the HTTP service — doing so is
        // protocol confusion. Reject it.
        let alpn = conn.alpn();
        if alpn != ALPN {
            tracing::debug!(
                "iroh-http: rejecting connection with non-HTTP ALPN {:?}",
                String::from_utf8_lossy(alpn),
            );
            conn.close(0u32.into(), b"unexpected ALPN");
            continue;
        }

        // Enforce total connection limit.
        if let Some(max_total) = cfg.max_total_connections {
            let current = total_connections.load(Ordering::Relaxed);
            if current >= max_total {
                tracing::warn!("iroh-http: total connection limit reached ({current}/{max_total})");
                conn.close(0u32.into(), b"server at capacity");
                continue;
            }
        }

        let remote_id = base32_encode(remote_pk.as_bytes());

        let conn_tracker = match ConnectionTracker::acquire(
            &peer_counts,
            remote_pk,
            remote_id.clone(),
            cfg.max_conns_per_peer,
            conn_event_fn.clone(),
            total_connections.clone(),
        ) {
            Some(g) => g,
            None => {
                tracing::warn!("iroh-http: peer {remote_id} exceeded connection limit");
                conn.close(0u32.into(), b"too many connections");
                continue;
            }
        };

        // Build the per-connection service: user-supplied `svc` with the
        // peer's `remote_node_id` injected as a [`RemoteNodeId`] request
        // extension (closes #177), wrapped with the shared concurrency
        // limiter, then type-erased into ServeService. Per ADR-014 D2 /
        // #175 this is the *only* place that names the concrete inner
        // stack — every downstream consumer sees the box.
        let conn_svc: super::pipeline::ServeService = ServiceBuilder::new()
            .layer(conc_layer.clone())
            .layer(tower_http::add_extension::AddExtensionLayer::new(
                RemoteNodeId(Arc::new(remote_id)),
            ))
            .service(svc.clone())
            .boxed_clone();

        let conn_requests = total_requests.clone();
        let in_flight_conn = in_flight.clone();
        let drain_notify_conn = drain_notify.clone();
        let max_header_size = cfg.max_header_size;

        // Build the tower stack once per connection. All StackConfig
        // fields are fixed for the lifetime of the serve loop, so the
        // composed service can be cloned cheaply per-bistream (single
        // Arc::clone inside BoxCloneService). Previously this was done
        // per-bistream — 5–6 unnecessary heap allocations per request.
        let timeout_dur = if cfg.request_timeout.is_zero() {
            Duration::MAX
        } else {
            cfg.request_timeout
        };
        let stack_cfg = StackConfig {
            timeout: if timeout_dur == Duration::MAX {
                None
            } else {
                Some(timeout_dur)
            },
            max_request_body_wire_bytes: cfg.max_request_body_wire_bytes,
            max_request_body_decoded_bytes: cfg.max_request_body_decoded_bytes,
            load_shed: cfg.load_shed_enabled,
            compression: cfg.stack_compression.clone(),
            decompression: cfg.decompression,
        };
        let conn_stack = super::stack::build_stack(conn_svc, &stack_cfg);

        // Bound the request-head read on each bistream (slowloris defence).
        // Reuse the request timeout: headers must arrive within it. `None`
        // means "no request timeout configured" → unbounded head read.
        let header_read_timeout = stack_cfg.timeout;

        let close_flag_conn = close_flag.clone();
        let close_conns_conn = close_connections.clone();
        let no_new_streams_conn = no_new_streams.clone();

        tokio::spawn(async move {
            // Owns the per-peer count, total-connection counter, and
            // connect/disconnect event firing for this connection's
            // lifetime. See `lifecycle.rs`.
            let _conn_tracker = conn_tracker;

            // ISS-001: clamp to hyper's minimum safe buffer size of 8192.
            // ISS-020: a stored value of 0 means "use the default" (64 KB).
            let effective_header_limit = if max_header_size == 0 {
                64 * 1024
            } else {
                max_header_size.max(8192)
            };

            loop {
                // F1: register the close waiter BEFORE reading `close_flag`.
                //
                // Teardown does `close_flag.store(true, Release);
                // close_connections.notify_waiters()`. `notify_waiters()`
                // stores NO permit — it only wakes tasks already registered as
                // waiters. If we registered the waiter *inside* the select
                // (after the flag load), a store+notify landing in the gap
                // between the load and the registration would be lost: this
                // task would then park in `accept_bi()` with no pending wakeup
                // and, if the peer is idle, never re-check the flag — leaving a
                // stopped/replaced loop serving a later request over this
                // connection (the exact serve-after-stop bug) and leaking the
                // task + connection.
                //
                // `Notified::enable()` registers the waiter up front. The
                // Notify's internal lock serializes `enable()` against
                // `notify_waiters()`, so afterwards either we were woken, or we
                // observe `close_flag == true` on the load below (the store
                // happens-before the notify, which happens-before our enable).
                let close_fut = close_conns_conn.notified();
                tokio::pin!(close_fut);
                close_fut.as_mut().enable();

                // #336: when the serve loop is stopped or replaced, force this
                // connection closed so the peer redials and lands on the new
                // loop instead of being served stale responses over a pooled
                // connection.
                if close_flag_conn.load(Ordering::Acquire) {
                    conn.close(0u32.into(), b"serve loop replaced");
                    break;
                }

                // F3: graceful stop has begun — stop accepting NEW bistreams so
                // one can't be accepted after the drain reaches zero and then be
                // truncated by the force-close. Park on the close signal (our
                // waiter is already registered above) and loop back to hit the
                // `close_flag` branch once the drain completes and close fires.
                if no_new_streams_conn.load(Ordering::Acquire) {
                    close_fut.as_mut().await;
                    continue;
                }

                let accepted = tokio::select! {
                    biased;
                    _ = &mut close_fut => None,
                    bi = conn.accept_bi() => Some(bi),
                };
                let (send, recv) = match accepted {
                    // Close signal woke us; re-check the flag at the loop top.
                    None => continue,
                    Some(Ok(pair)) => {
                        // M1: the flag check at the loop top can't catch a
                        // stream that `accept_bi()` was already parked on when
                        // graceful stop began — that call returns ONE more
                        // stream. `no_new_streams` is stored BEFORE the drain's
                        // zero-check, so re-loading it here (after accept_bi
                        // returns, before we count the request in-flight)
                        // closes the residual window: refuse the late stream and
                        // fall through to the close branch instead of dispatching
                        // it, so it is cleanly rejected rather than served and
                        // then truncated mid-response by the force-close.
                        if no_new_streams_conn.load(Ordering::Acquire) {
                            drop(pair);
                            continue;
                        }
                        pair
                    }
                    Some(Err(_)) => break,
                };

                // Register the transport-delivery waiter before handing the
                // send stream to hyper. `serve_bistream` returning only means
                // hyper queued the response bytes and FIN; it does not mean
                // the peer acknowledged them. A graceful stop closes pooled
                // QUIC connections after `in_flight` reaches zero, and iroh's
                // immediate `Connection::close` may otherwise discard those
                // queued bytes before the peer receives even the response
                // head (deterministic under ASan).
                let response_stopped = send.stopped();
                let io = TokioIo::new(IrohStream::new(send, recv));
                let svc = conn_stack.clone();
                let req_counter = conn_requests.clone();
                req_counter.fetch_add(1, Ordering::Relaxed);
                in_flight_conn.fetch_add(1, Ordering::Relaxed);

                let in_flight_req = in_flight_conn.clone();
                let drain_notify_req = drain_notify_conn.clone();

                tokio::spawn(async move {
                    // Public request processing and the private transport-
                    // delivery drain boundary have distinct lifetimes. Keep
                    // endpoint stats scoped to Hyper processing, while the
                    // delivery tracker below remains live through QUIC ACK.
                    let _delivery_tracker = DeliveryTracker::new(in_flight_req, drain_notify_req);
                    {
                        let _request_tracker = RequestTracker::new(req_counter);
                        super::pipeline::serve_bistream(
                            io,
                            svc,
                            effective_header_limit,
                            header_read_timeout,
                        )
                        .await;
                    }

                    // Keep the request in-flight until QUIC reports that the
                    // peer either acknowledged the finished response stream or
                    // explicitly stopped it. This gives graceful drain a real
                    // transport-delivery boundary before it closes the pooled
                    // connection. Connection failure/force-close also resolves
                    // this future, while the serve loop's existing drain
                    // timeout remains the upper bound during shutdown.
                    if let Err(error) = response_stopped.await {
                        tracing::debug!(%error, "iroh-http: response stream ended before delivery acknowledgement");
                    }
                });
            }
        });
    }

    // Graceful drain: wait for all in-flight requests to finish, or give
    // up after `drain_timeout`. Loop avoids the race between `in_flight ==
    // 0` check and `notified()`: if the last request finishes between the
    // load and the await, the loop re-checks immediately after the
    // timeout wakes it.
    //
    // F3: signal per-connection tasks to stop accepting NEW bistreams before we
    // start counting down. Any stream already accepted is still counted here and
    // waited for; each connection accepts at most one more in-progress stream
    // (which the drain then waits for) before parking, so no new stream can push
    // `in_flight` back above zero once it has drained and the force-close below
    // can't truncate a freshly-accepted request.
    no_new_streams.store(true, Ordering::Release);
    #[allow(clippy::arithmetic_side_effects)] // fallback: 86400s is safe
    let deadline = tokio::time::Instant::now()
        .checked_add(cfg.drain_timeout)
        .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
    loop {
        // M2: register the drain waiter BEFORE loading `in_flight`, same shape
        // as F1. `RequestTracker::drop` wakes us via `notify_waiters()`, which
        // stores NO permit — it only wakes tasks already registered. If we
        // created `notified()` *inside* the select (after the load), a
        // last-request drain landing in the gap between the load and the
        // registration would be lost, and we'd sleep the full `remaining`
        // (up to the whole `drain_timeout`, default 30s) before re-checking —
        // stalling `done_tx.send(true)` and, downstream, the JS `serveRunning`
        // guard. `enable()` registers up front so any drain after the load
        // wakes us and any before is caught by the `== 0` load.
        let drain_fut = drain_notify.notified();
        tokio::pin!(drain_fut);
        drain_fut.as_mut().enable();

        if in_flight.load(Ordering::Acquire) == 0 {
            tracing::info!("iroh-http: all in-flight requests drained");
            break;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                "iroh-http: drain timed out after {}s",
                cfg.drain_timeout.as_secs()
            );
            break;
        }
        tokio::select! {
            _ = &mut drain_fut => {}
            _ = tokio::time::sleep(remaining) => {}
        }
    }

    // #336 (stop path): the drain above only stops the *accept* loop and waits
    // for in-flight requests to finish. The per-connection tasks spawned above
    // are detached and keep `accept_bi()`-ing on already-open (pooled) QUIC
    // connections, so without closing them a peer whose connection is still
    // open keeps getting served AFTER stop — requests dispatched seconds after
    // "all in-flight requests drained" (the "serve keeps answering post-stop"
    // symptom). Now that in-flight work has drained, force the active
    // connections closed so no new request is served on this stopped loop and
    // the peer must redial (landing on a fresh loop if serve is restarted).
    //
    // This is the graceful, drain-THEN-close counterpart to the replace path's
    // `shutdown_and_close()`, which sets `close_flag` up front for an immediate
    // sever. Setting it here (idempotent: replace may have set it already) is
    // harmless and unifies the invariant: a stopped serve loop never serves.
    close_flag.store(true, Ordering::Release);
    close_connections.notify_waiters();

    let _ = done_tx.send(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensures a `drain_timeout` of `Duration::MAX` (or any value that
    /// overflows `Instant::now() + duration`) does not panic. Regression
    /// test for #200.
    #[test]
    #[allow(clippy::arithmetic_side_effects)]
    fn drain_deadline_saturates_on_huge_timeout() {
        let now = tokio::time::Instant::now();
        let huge = Duration::from_millis(u64::MAX);
        let deadline = now
            .checked_add(huge)
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
        // Must not panic; deadline should be roughly 24h in the future.
        assert!(deadline > now);
    }
}
