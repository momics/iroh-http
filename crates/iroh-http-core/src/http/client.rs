//! Outgoing HTTP request — pure-Rust `fetch_request()` implementation.
//!
//! HTTP/1.1 framing is delegated entirely to hyper. Iroh's QUIC stream pair
//! is wrapped in `IrohStream` and handed to hyper's client connection API.
//!
//! Slice D (#186) split the original FFI-shaped `fetch(endpoint, &str, ...)`
//! into two layers:
//!
//! - [`fetch_request`] — pure-Rust API: takes a fully-formed
//!   [`hyper::Request<Body>`] and an [`iroh::EndpointAddr`], returns
//!   [`hyper::Response<Body>`] with a typed [`FetchError`]. No `u64`
//!   handles, no `BodyReader`, no string parsing of error messages, no
//!   imports from `crate::ffi`.
//! - [`crate::ffi::fetch::fetch`] — FFI-shaped wrapper that builds the
//!   `Request<Body>` from flat strings, calls [`fetch_request`], and
//!   translates the response into a [`crate::FfiResponse`].

use hyper_util::rt::TokioIo;

use crate::{
    http::{server::stack::StackConfig, transport::io::IrohStream},
    Body, IrohEndpoint, ALPN,
};

// ── Typed fetch error ────────────────────────────────────────────────────────

/// Typed error returned by the pure-Rust [`fetch_request`] API.
///
/// Header-oversize detection lives **above** this layer in
/// [`crate::ffi::fetch`], which compares the assembled response head
/// byte-count against the endpoint's `max_header_size`. That check is
/// deterministic and does not depend on hyper's error wording, so this
/// enum no longer disambiguates header parse failures from other
/// transport errors — they all surface as `ConnectionFailed`.
#[derive(Debug)]
#[non_exhaustive]
pub enum FetchError {
    /// Connection setup, hyper handshake, or send-request transport
    /// failure. Wraps the underlying [`hyper::Error`] when one is
    /// available so callers can downcast instead of substring-matching.
    ConnectionFailed {
        detail: String,
        source: Option<hyper::Error>,
    },
    /// Response head exceeded the endpoint's `max_header_size` budget.
    /// Produced only by [`crate::ffi::fetch`]'s post-receive byte-count
    /// check; [`fetch_request`] itself never emits this variant.
    HeaderTooLarge { detail: String },
    /// Response body exceeded the configured byte limit. Surfaced by the
    /// FFI wrapper after the body is drained.
    BodyTooLarge,
    /// `cfg.timeout` elapsed before the response head arrived.
    Timeout,
    /// Caller dropped the future or signalled cancellation via the FFI
    /// fetch token.
    Cancelled,
    /// Bug or unexpected internal failure (request build, body wrap, …).
    Internal(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::ConnectionFailed { detail, .. } => {
                write!(f, "connection failed: {detail}")
            }
            FetchError::HeaderTooLarge { detail } => {
                write!(f, "response header too large: {detail}")
            }
            FetchError::BodyTooLarge => f.write_str("response body too large"),
            FetchError::Timeout => f.write_str("request timed out"),
            FetchError::Cancelled => f.write_str("request cancelled"),
            FetchError::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for FetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FetchError::ConnectionFailed {
                source: Some(s), ..
            } => Some(s),
            _ => None,
        }
    }
}

// ── Pure-Rust fetch API ──────────────────────────────────────────────────────

/// Pure-Rust outbound entry — the canonical client API.
///
/// Establishes (or reuses, via [`crate::http::transport::pool`]) an Iroh
/// QUIC connection to `addr`, runs hyper's HTTP/1.1 client handshake on
/// a freshly opened bidirectional stream, dispatches `req` through the
/// shared client tower stack ([`crate::http::server::stack::build_client_stack`]),
/// and returns the response.
///
/// The returned [`hyper::Response<Body>`] streams its body lazily; the
/// caller is responsible for draining it.
///
/// # Errors
///
/// Returns [`FetchError::Timeout`] if `cfg.timeout` is set and elapsed
/// before the response head arrived. Connection / handshake / transport
/// failures map to [`FetchError::ConnectionFailed`].
pub async fn fetch_request(
    endpoint: &IrohEndpoint,
    addr: &iroh::EndpointAddr,
    req: hyper::Request<Body>,
    cfg: &StackConfig,
) -> Result<hyper::Response<Body>, FetchError> {
    let node_id = addr.id;
    let work = async {
        let ep_raw = endpoint.raw().clone();
        let addr_clone = addr.clone();
        let max_header_size = endpoint.max_header_size();

        let pooled = endpoint
            .pool()
            .get_or_connect(node_id, ALPN, || async move {
                ep_raw
                    .connect(addr_clone, ALPN)
                    .await
                    .map_err(|e| format!("connect: {e}"))
            })
            .await
            .map_err(|e| FetchError::ConnectionFailed {
                detail: e,
                source: None,
            })?;

        let conn = pooled.conn.clone();

        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| FetchError::ConnectionFailed {
                detail: format!("open_bi: {e}"),
                source: None,
            })?;
        let io = TokioIo::new(IrohStream::new(send, recv));

        let (sender, conn_task) = hyper::client::conn::http1::Builder::new()
            // hyper requires max_buf_size >= 8192; clamp upward so small
            // max_header_size values don't panic. Header-size enforcement
            // happens at the byte-count check in `ffi::fetch` after the
            // response is returned (deterministic, framing-independent).
            .max_buf_size(max_header_size.max(8192))
            .max_headers(128)
            .handshake::<_, Body>(io)
            .await
            .map_err(|e| FetchError::ConnectionFailed {
                detail: format!("hyper handshake: {e}"),
                source: Some(e),
            })?;

        // Drive the connection state machine in the background.
        tokio::spawn(conn_task);

        // Dispatch through the shared client stack (Slice B / #184).
        use tower::ServiceExt;
        let svc = crate::http::server::stack::build_client_stack(sender, cfg);
        svc.oneshot(req)
            .await
            .map_err(|e| FetchError::ConnectionFailed {
                detail: format!("send_request: {e}"),
                source: Some(e),
            })
    };

    let result = match cfg.timeout {
        Some(t) => match tokio::time::timeout(t, work).await {
            Ok(r) => r,
            Err(_) => Err(FetchError::Timeout),
        },
        None => work.await,
    };

    if matches!(
        result,
        Err(FetchError::Timeout | FetchError::ConnectionFailed { .. })
    ) {
        endpoint
            .pool()
            .evict_and_close(node_id, ALPN, b"iroh-http fetch failed")
            .await;
    }

    result
}

// `extract_path` and the hyper-body→channel pumps moved to `ffi/fetch.rs`
// and `ffi/pumps.rs` respectively (Slice D, #186). They were FFI plumbing
// in shape and use; keeping them in `mod http` was the last reason this
// module had to import from `crate::ffi`.
