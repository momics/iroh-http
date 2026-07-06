//! `ServeOptions` and the default tunables consumed by
//! [`crate::http::server::serve_with_events`].
//!
//! Split out of `mod.rs` per Slice C.7 of #182 so the accept loop in
//! `mod.rs` stays close to the axum reference shape (≤ 200 LoC).

/// Options for the HTTP serve loop.
///
/// Passed directly to [`crate::http::server::serve`] or
/// [`crate::http::server::serve_with_events`]. These govern
/// per-request middleware (Tower layers), inbound connection caps, and
/// serve-loop lifecycle — they do **not** affect outgoing fetch calls.
#[derive(Debug, Clone, Default)]
pub struct ServeOptions {
    /// Maximum simultaneous in-flight requests.  Default: 1024.
    pub max_concurrency: Option<usize>,
    /// Per-request timeout in milliseconds.  Default: 60 000.
    pub request_timeout_ms: Option<u64>,
    /// Maximum connections from a single peer.  Default: 8.
    pub max_connections_per_peer: Option<usize>,
    /// Reject request bodies larger than this many **wire** bytes (compressed).
    /// Default: 16 MiB.
    pub max_request_body_wire_bytes: Option<usize>,
    /// Reject request bodies larger than this many **decoded** bytes (after
    /// decompression). This is the primary compression-bomb guard.
    /// Default: 16 MiB.
    pub max_request_body_decoded_bytes: Option<usize>,
    /// Graceful shutdown drain window in milliseconds.  Default: 30 000.
    pub drain_timeout_ms: Option<u64>,
    /// Maximum total QUIC connections the server will accept.  Default: unlimited.
    pub max_total_connections: Option<usize>,
    /// When `true` (the default), reject new requests immediately with `503
    /// Service Unavailable` when `max_concurrency` is already reached rather
    /// than queuing them.  Prevents thundering-herd on recovery.
    pub load_shed: Option<bool>,
    /// When `true` (the default), automatically decompress compressed request
    /// bodies before handing them to the handler.  Set to `false` to receive
    /// the raw wire bytes (e.g. for relay/proxy use-cases that forward the
    /// body downstream without inspecting it).
    pub decompression: Option<bool>,
}

pub(crate) const DEFAULT_CONCURRENCY: usize = 1024;
pub(crate) const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;
pub(crate) const DEFAULT_MAX_CONNECTIONS_PER_PEER: usize = 8;
pub(crate) const DEFAULT_DRAIN_TIMEOUT_MS: u64 = 30_000;
/// 16 MiB — applied when `max_request_body_wire_bytes` or
/// `max_request_body_decoded_bytes` is not explicitly set.
/// Prevents memory exhaustion from unbounded request bodies.
pub(crate) const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
/// 256 MiB — applied when `max_response_body_bytes` is not explicitly set.
/// Prevents memory exhaustion from a malicious server sending a compressed
/// response that expands to an unbounded size (compression bomb).
pub(crate) const DEFAULT_MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024 * 1024;
