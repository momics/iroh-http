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
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use hyper_util::rt::TokioIo;
use tower::{limit::ConcurrencyLimitLayer, Service, ServiceBuilder, ServiceExt};

use crate::{base32_encode, http::transport::io::IrohStream, Body, IrohEndpoint, ALPN};

use super::lifecycle::{ConnectionTracker, RequestTracker};
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
                let (send, recv) = match conn.accept_bi().await {
                    Ok(pair) => pair,
                    Err(_) => break,
                };

                let io = TokioIo::new(IrohStream::new(send, recv));
                let svc = conn_stack.clone();
                let req_counter = conn_requests.clone();
                req_counter.fetch_add(1, Ordering::Relaxed);
                in_flight_conn.fetch_add(1, Ordering::Relaxed);

                let in_flight_req = in_flight_conn.clone();
                let drain_notify_req = drain_notify_conn.clone();

                tokio::spawn(async move {
                    // Owns the per-connection and crate-wide in-flight
                    // counters; notifies drain waiters when in-flight
                    // reaches zero. See `lifecycle.rs`.
                    let _req_tracker =
                        RequestTracker::new(req_counter, in_flight_req, drain_notify_req);
                    super::pipeline::serve_bistream(
                        io,
                        svc,
                        effective_header_limit,
                        header_read_timeout,
                    )
                    .await;
                });
            }
        });
    }

    // Graceful drain: wait for all in-flight requests to finish, or give
    // up after `drain_timeout`. Loop avoids the race between `in_flight ==
    // 0` check and `notified()`: if the last request finishes between the
    // load and the await, the loop re-checks immediately after the
    // timeout wakes it.
    #[allow(clippy::arithmetic_side_effects)] // fallback: 86400s is safe
    let deadline = tokio::time::Instant::now()
        .checked_add(cfg.drain_timeout)
        .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
    loop {
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
            _ = drain_notify.notified() => {}
            _ = tokio::time::sleep(remaining) => {}
        }
    }
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
