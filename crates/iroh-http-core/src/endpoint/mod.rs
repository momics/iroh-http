//! Iroh endpoint lifecycle — create, share, and close.
//!
//! [`IrohEndpoint`] is a thin façade over [`EndpointInner`], which is
//! composed of the four named subsystems from ADR-014 D1:
//!
//! - [`transport::Transport`] — raw QUIC endpoint and stable identity.
//! - [`http_runtime::HttpRuntime`] — pool, HTTP limits, in-flight counters.
//! - [`session_runtime::SessionRuntime`] — serve loop, lifecycle signals,
//!   transport events, path subscriptions.
//! - [`ffi_bridge::FfiBridge`] — the opaque-handle store reachable from JS.
//!
//! No business logic lives in this module — only orchestration and the
//! public API surface. Sub-modules:
//! - [`bind`] — `IrohEndpoint::bind()` constructor.
//! - [`observe`] — observability and peer-info methods.
//! - [`config`] — `NodeOptions` and friends.
//! - [`stats`] — snapshot and event types.
// `handles()` returns `&HandleStore` which is in the disallowed-types list.
// The disallowed_types lint is used to prevent mod http from depending on mod
// ffi; endpoint/ is neither mod http nor mod ffi, so the allow is correct here.
#![allow(clippy::disallowed_types)]

use std::sync::Arc;

pub(in crate::endpoint) mod bind;
pub(in crate::endpoint) mod ffi_bridge;
pub(in crate::endpoint) mod http_runtime;
pub(in crate::endpoint) mod lifecycle;
pub(in crate::endpoint) mod observe;
pub(in crate::endpoint) mod session_runtime;
pub(in crate::endpoint) mod transport;

pub mod config;
pub mod stats;

pub use bind::parse_direct_addrs;
pub use config::{DiscoveryOptions, NetworkingOptions, NodeOptions, PoolOptions, StreamingOptions};
pub use http::server::stack::CompressionOptions;
pub use stats::{ConnectionEvent, EndpointStats, NodeAddrInfo, PathInfo, PeerStats};

use crate::ffi::handles::HandleStore;
use crate::http;
use crate::http::transport::pool::ConnectionPool;

use ffi_bridge::FfiBridge;
use http_runtime::HttpRuntime;
use transport::Transport;

/// A shared Iroh endpoint.
///
/// Clone-able (cheap Arc clone).  All fetch and serve calls on the same node
/// share one endpoint and therefore one stable QUIC identity.
#[derive(Clone)]
pub struct IrohEndpoint {
    pub(in crate::endpoint) inner: Arc<EndpointInner>,
}

/// Composition of the four ADR-014 D1 subsystems. No business logic; the
/// public API on [`IrohEndpoint`] reaches into the appropriate subsystem.
pub(in crate::endpoint) struct EndpointInner {
    pub(in crate::endpoint) transport: Transport,
    pub(in crate::endpoint) http: HttpRuntime,
    pub(in crate::endpoint) session: session_runtime::SessionRuntime,
    pub(in crate::endpoint) ffi: FfiBridge,
}

impl IrohEndpoint {
    // ── Stable identity ──────────────────────────────────────────────────────

    /// The node's public key as a lowercase base32 string.
    pub fn node_id(&self) -> &str {
        &self.inner.transport.node_id_str
    }

    // ── Handle store ─────────────────────────────────────────────────────────

    /// Per-endpoint handle store.
    pub fn handles(&self) -> &HandleStore {
        &self.inner.ffi.handles
    }

    /// Immediately run a TTL sweep on all handle registries.
    pub fn sweep_now(&self) {
        let ttl = self.inner.ffi.handles.config.ttl;
        if !ttl.is_zero() {
            self.inner.ffi.handles.sweep(ttl);
        }
    }

    // ── Local serve service (self-request loopback) ──────────────────────────

    /// Allocate the stable identity for a new serve cycle on this endpoint.
    pub(crate) fn next_serve_token(&self) -> crate::http::server::handle::ServeToken {
        self.inner.session.next_serve_token()
    }

    /// Register the FFI service for in-process self-requests, but only while
    /// `handle` is still the active, unstopped serve cycle.
    ///
    /// The common server path has already installed the token before this is
    /// called. Holding the serve slot while checking identity and installing
    /// the weak service makes a concurrent `stop_serve` linearizable: either
    /// the service is installed and then cleared, or the stopped token rejects
    /// the late install.
    pub(crate) fn set_local_service_for(
        &self,
        handle: &crate::http::server::ServeHandle,
        svc: &crate::ffi::dispatcher::IrohHttpService,
    ) {
        let serve_guard = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let is_current = serve_guard
            .as_ref()
            .is_some_and(|current| current.is_same_cycle(handle));
        if is_current && !handle.is_shutdown_requested() {
            *self
                .inner
                .ffi
                .local_service
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(svc.downgrade());
        }
    }

    /// Clear the registered serve service. Called on serve stop / close so a
    /// self-request after teardown fails cleanly instead of dispatching to a
    /// dead handler. The slot holds only a weak handle, but explicit clearing
    /// preserves immediate post-teardown errors while the serve task winds down.
    pub(crate) fn clear_local_service(&self) {
        *self
            .inner
            .ffi
            .local_service
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// A clone of the locally-running serve service, if `serve()` is active.
    /// Used by [`crate::ffi::fetch`] to route self-requests in-process.
    pub(crate) fn local_service(&self) -> Option<crate::ffi::dispatcher::IrohHttpService> {
        self.inner
            .ffi
            .local_service
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(crate::ffi::dispatcher::IrohHttpService::upgrade)
    }

    // ── HTTP runtime accessors ────────────────────────────────────────────────

    /// Maximum byte size of an HTTP/1.1 head.
    pub fn max_header_size(&self) -> usize {
        self.inner.http.max_header_size
    }

    /// Maximum decompressed response-body bytes accepted per outgoing fetch.
    pub fn max_response_body_bytes(&self) -> usize {
        self.inner.http.max_response_body_bytes
    }

    /// Compression options, if the `compression` feature is enabled.
    pub fn compression(&self) -> Option<&CompressionOptions> {
        self.inner.http.compression.as_ref()
    }

    /// Access the connection pool.
    pub(crate) fn pool(&self) -> &ConnectionPool {
        &self.inner.http.pool
    }

    /// Shared active-connections counter (used by the accept loop).
    pub(crate) fn active_connections_arc(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.inner.http.active_connections.clone()
    }

    /// Shared active-requests counter (used by the accept loop).
    pub(crate) fn active_requests_arc(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.inner.http.active_requests.clone()
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    /// Closed-signal sender shared with the accept loop.
    pub(crate) fn connection_closed_tx(&self) -> tokio::sync::watch::Sender<bool> {
        self.inner.session.closed_tx.clone()
    }

    /// Access the raw Iroh endpoint.
    pub fn raw(&self) -> &iroh::Endpoint {
        &self.inner.transport.ep
    }
}
