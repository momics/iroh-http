//! `iroh-http-core` — Iroh QUIC endpoint, HTTP/1.1 via hyper, fetch and serve.
//!
//! This crate owns the Iroh endpoint and wires HTTP/1.1 framing to QUIC
//! streams via hyper.  Nothing in here knows about JavaScript.
//!
//! Per epic #182 the crate is split into two top-level modules with a
//! one-way dependency:
//!
//! - [`http`] — pure-Rust HTTP-over-iroh primitives. No `u64` handles,
//!   no callbacks. A pure-Rust application can call [`http::client::fetch_request`]
//!   and [`http::server::serve`] without touching any FFI type.
//! - [`ffi`] — FFI bridge: handle store, callback-shaped serve, flat
//!   fetch. Wraps [`http`]; never imported in the reverse direction
//!   (enforced by `tests/architecture.rs`).
#![deny(unsafe_code)]

pub mod endpoint;

pub(crate) mod ffi;
pub(crate) mod http;

// Cross-cutting utility modules (extracted from lib.rs, see #198).
mod addr;
mod crypto;
mod encoding;
mod error;

// Thin re-export modules preserving external API paths.
pub mod events {
    pub use crate::http::events::*;
}
pub mod registry {
    pub use crate::ffi::registry::*;
}

// ── Pure-Rust HTTP API surface (`mod http`) ───────────────────────────────────
pub use http::body::{Body, BoxError};
pub use http::client::{fetch_request, FetchError};
pub use http::server::{serve, serve_with_events, RemoteNodeId, ServeHandle, ServeOptions};

// ── FFI bridge surface (`mod ffi`) ────────────────────────────────
pub use ffi::dispatcher::{ffi_serve, ffi_serve_with_callback, respond};
pub use ffi::fetch::fetch;
#[allow(clippy::disallowed_types)] // FFI re-exports at crate root
pub use ffi::handles::{
    make_body_channel, BodyReader, HandleStore, ResponseHeadEntry, StoreConfig,
};
pub use ffi::session::{CloseInfo, Session};
#[allow(clippy::disallowed_types)] // FFI re-exports at crate root
pub use ffi::types::{FfiDuplexStream, FfiResponse, RequestPayload};

// ── Other re-exports kept at crate root ───────────────────────────────────────
pub use endpoint::{
    parse_direct_addrs, ConnectionEvent, DiscoveryOptions, EndpointStats, IrohEndpoint,
    NetworkingOptions, NodeAddrInfo, NodeOptions, PathInfo, PeerStats, PoolOptions,
    StreamingOptions,
};
pub use events::TransportEvent;
pub use http::server::stack::{CompressionOptions, StackConfig};
pub use registry::{get_endpoint, insert_endpoint, remove_endpoint};

// ── Structured error types ────────────────────────────────────────────────────
pub use error::{CoreError, ErrorCode};

// ── ALPN protocol identifiers ─────────────────────────────────────────────────

/// ALPN for the HTTP/1.1-over-QUIC protocol (version 2 wire format).
pub const ALPN: &[u8] = b"iroh-http/2";
/// ALPN for base + bidirectional streaming sessions.
pub const ALPN_DUPLEX: &[u8] = b"iroh-http/2-duplex";

/// String form of [`ALPN`], for use in [`NodeOptions::capabilities`].
pub const ALPN_STR: &str = "iroh-http/2";
/// String form of [`ALPN_DUPLEX`], for use in [`NodeOptions::capabilities`].
pub const ALPN_DUPLEX_STR: &str = "iroh-http/2-duplex";

/// All recognised ALPN capability strings.
pub const KNOWN_ALPNS: &[&str] = &[ALPN_STR, ALPN_DUPLEX_STR];

// ── Key operations ───────────────────────────────────────────────────────────
pub use crypto::{generate_secret_key, public_key_verify, secret_key_sign};

// ── Encoding utilities ────────────────────────────────────────────────────────
pub(crate) use encoding::base32_decode;
pub use encoding::base32_encode;

// ── Node address parsing ──────────────────────────────────────────────────────
pub use addr::{node_ticket, parse_node_addr, ParsedNodeAddr};
