//! Per-connection hyper seam for [`crate::http::server`].
//!
//! Exists to keep the accept loop free of hyper / tower wiring details.
//! Composition of the actual layer stack lives one module over in
//! [`super::stack`] (see Slice B of #182 — issue #184). This file only owns:
//!
//! * the hyper `http1::Builder` configuration,
//! * the body-normalisation `MapRequestBodyLayer` at the hyper → tower
//!   seam (ADR-014 D2 / #175),
//! * the per-bistream `tracing` site for hyper connection errors.

use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tower::ServiceBuilder;

pub(crate) use super::stack::ServeService;
use crate::http::transport::io::IrohStream;
use crate::Body;
use std::time::Duration;

/// Drive a single hyper HTTP/1.1 connection over `io` with a pre-built
/// tower stack.
///
/// Returns the future. The accept loop spawns it; this function neither
/// spawns nor logs any lifecycle event of its own (drop guards live in the
/// caller's task). Connection-level errors are logged at `debug!` level
/// because most of them are routine end-of-life conditions on a P2P
/// transport (peer reset, idle timeout, decoder rejection of malformed
/// inbound bytes) — promoting them to `warn` would flood operators with
/// noise. Layer-side service errors that *do* warrant escalation are
/// surfaced via `HandleLayerErrorLayer`, which converts them into
/// structured HTTP responses before they ever reach this point.
///
/// `effective_header_limit` configures the hyper `http1::Builder`'s
/// `max_buf_size`; it is **not** part of [`super::stack::StackConfig`]
/// because it is a hyper framing knob, not a tower layer.
///
/// `header_read_timeout` bounds how long hyper will wait for a complete
/// request head on a freshly-accepted bistream. The tower `Timeout` layer
/// only starts once hyper has parsed a request and called the service, so
/// without this a peer can open a bistream and trickle (or withhold) header
/// bytes indefinitely (slowloris). `None` leaves the head read unbounded.
pub(crate) async fn serve_bistream(
    io: TokioIo<IrohStream>,
    stack: ServeService,
    effective_header_limit: usize,
    header_read_timeout: Option<Duration>,
) {
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .max_buf_size(effective_header_limit)
        .max_headers(128)
        // F17: the protocol contract is one request/response exchange per QUIC
        // bi-stream (the client opens a fresh `open_bi` per request, and the
        // dispatcher finishes the send stream once the response body ends). Make
        // that explicit at the hyper layer too by disabling HTTP/1 keep-alive:
        // left on (hyper's default), a peer that kept an accepted stream open
        // could submit a second request on it after a graceful stop began —
        // `no_new_streams` gates only *new* bi-streams, not a second request on
        // an existing one — running handler side effects inside the drain
        // window. Off, hyper closes the connection after a single exchange.
        .keep_alive(false);
    if let Some(timeout) = header_read_timeout {
        builder.header_read_timeout(timeout);
    }

    // ADR-014 D2 / #175 — body normalisation at the hyper seam.
    //
    // hyper hands `Request<Incoming>`; the rest of the chain (built by
    // `build_stack`) is uniformly `Service<Request<Body>>`. The single
    // `MapRequestBodyLayer` here is the only place that names
    // `hyper::body::Incoming` — every layer downstream sees `Body`.
    // Same shape as `axum::serve`'s
    // `make_service.call(...).map_request(|r| r.map(Body::new))`.
    use tower_http::map_request_body::MapRequestBodyLayer;
    let from_incoming = MapRequestBodyLayer::new(|b: hyper::body::Incoming| Body::new(b));

    let full_stack = ServiceBuilder::new().layer(from_incoming).service(stack);

    let result = builder
        .serve_connection(io, TowerToHyperService::new(full_stack))
        .with_upgrades()
        .await;

    if let Err(e) = result {
        tracing::debug!("iroh-http: http1 connection error: {e}");
    }
}
