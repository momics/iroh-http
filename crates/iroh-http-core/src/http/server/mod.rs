//! Incoming HTTP request — pure-Rust `serve()` implementation.
//!
//! Each accepted QUIC bidirectional stream is driven by hyper's HTTP/1.1
//! server connection. The user supplies a `tower::Service<Request<Body>,
//! Response = Response<Body>, Error = Infallible>`; the per-connection
//! `AddExtensionLayer` makes the authenticated peer id available as a
//! [`RemoteNodeId`] request extension (closes #177).
//!
//! The accept loop body lives in [`accept`]; this file is the public
//! surface and option-resolution glue, kept close to the axum reference
//! shape. Sub-modules: [`options`] (`ServeOptions` + defaults), [`handle`]
//! (`ServeHandle`), [`error_layer`] (tower → HTTP error converter),
//! [`pipeline`] / [`stack`] / [`lifecycle`] (per-bistream chain and RAII
//! guards).
//!
//! The FFI-shaped callback API ([`crate::ffi::dispatcher::ffi_serve_with_callback`])
//! is one specific consumer of this entry — it constructs an
//! `IrohHttpService` around the JS callback and hands it in like any
//! other service.

pub(crate) mod accept;
pub(crate) mod error_layer;
pub(crate) mod handle;
pub(crate) mod lifecycle;
pub(crate) mod options;
pub(crate) mod pipeline;
pub(crate) mod stack;

use std::{sync::Arc, time::Duration};

use tower::Service;

use crate::{Body, ConnectionEvent, IrohEndpoint};

use self::accept::{accept_loop, AcceptConfig};
use self::options::{
    DEFAULT_CONCURRENCY, DEFAULT_DRAIN_TIMEOUT_MS, DEFAULT_MAX_CONNECTIONS_PER_PEER,
    DEFAULT_MAX_REQUEST_BODY_BYTES, DEFAULT_REQUEST_TIMEOUT_MS,
};

// Re-exported from sub-modules so external paths
// (`crate::http::server::ServeOptions`, `…::ServeHandle`,
// `…::HandleLayerErrorLayer`, `…::DEFAULT_MAX_RESPONSE_BODY_BYTES`) stay
// unchanged after Slice C.7 split.
pub(crate) use self::error_layer::HandleLayerErrorLayer;
pub use self::handle::ServeHandle;
pub use self::options::ServeOptions;
pub(crate) use self::options::DEFAULT_MAX_RESPONSE_BODY_BYTES;

// ── Connection-event callback type ───────────────────────────────────────────

pub(crate) type ConnectionEventFn = Arc<dyn Fn(ConnectionEvent) + Send + Sync>;

/// Authenticated peer node id of the QUIC connection a request arrived
/// on. Inserted as a request extension by the per-connection
/// [`tower_http::add_extension::AddExtensionLayer`] in
/// [`serve_with_events`].
///
/// User-facing pure-Rust services consume it with
/// `req.extensions().get::<RemoteNodeId>()`. Closes #177.
#[derive(Clone, Debug)]
pub struct RemoteNodeId(pub Arc<String>);

/// Pure-Rust serve entry — convenience 3-arg wrapper that omits the
/// connection-event callback. Equivalent to `serve_with_events(ep,
/// opts, svc, None)`.
pub fn serve<S>(endpoint: IrohEndpoint, options: ServeOptions, svc: S) -> ServeHandle
where
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
    serve_with_events(endpoint, options, svc, None)
}

/// Pure-Rust serve entry — the canonical inbound API.
///
/// Accepts any `tower::Service<Request<Body>, Response = Response<Body>,
/// Error = Infallible>` (`Clone + Send + Sync + 'static`, with `Send`
/// futures). Each accepted QUIC bidirectional stream is driven by
/// hyper's HTTP/1.1 server connection through the per-connection tower
/// stack composed in [`stack::build_stack`]; the user service sees
/// requests with the authenticated peer id available as a typed
/// [`RemoteNodeId`] request extension.
///
/// `on_connection_event` is called on 0→1 (first connection from a peer)
/// and 1→0 (last connection from a peer closed) count transitions.
///
/// # Security
///
/// Calling this opens a **public endpoint** on the Iroh overlay network.
/// Any peer that knows or discovers your node's public key can connect
/// and send requests. Iroh QUIC authenticates the peer's *identity*
/// cryptographically, but does not enforce *authorization*. Inspect
/// [`RemoteNodeId`] in your service and reject untrusted peers.
pub fn serve_with_events<S>(
    endpoint: IrohEndpoint,
    options: ServeOptions,
    svc: S,
    on_connection_event: Option<ConnectionEventFn>,
) -> ServeHandle
where
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
    let cfg = AcceptConfig {
        max: options.max_concurrency.unwrap_or(DEFAULT_CONCURRENCY),
        request_timeout: options
            .request_timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS)),
        max_conns_per_peer: options
            .max_connections_per_peer
            .unwrap_or(DEFAULT_MAX_CONNECTIONS_PER_PEER),
        max_request_body_wire_bytes: options
            .max_request_body_wire_bytes
            .or(Some(DEFAULT_MAX_REQUEST_BODY_BYTES)),
        max_request_body_decoded_bytes: options
            .max_request_body_decoded_bytes
            .or(Some(DEFAULT_MAX_REQUEST_BODY_BYTES)),
        max_total_connections: options.max_total_connections,
        drain_timeout: Duration::from_millis(
            options.drain_timeout_ms.unwrap_or(DEFAULT_DRAIN_TIMEOUT_MS),
        ),
        // Load-shed is opt-out — default `true`.
        load_shed_enabled: options.load_shed.unwrap_or(true),
        max_header_size: endpoint.max_header_size(),
        stack_compression: endpoint.compression().cloned(),
        // Decompression is opt-out — default `true`.
        decompression: options.decompression.unwrap_or(true),
    };

    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let close_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let close_connections = Arc::new(tokio::sync::Notify::new());
    let drain_dur = cfg.drain_timeout;
    let (done_tx, done_rx) = tokio::sync::watch::channel(false);

    let join = tokio::spawn(accept_loop(
        endpoint,
        cfg,
        svc,
        on_connection_event,
        shutdown_notify.clone(),
        close_flag.clone(),
        close_connections.clone(),
        done_tx,
    ));

    ServeHandle {
        join,
        shutdown_notify,
        close_flag,
        close_connections,
        drain_timeout: drain_dur,
        done_rx,
    }
}
