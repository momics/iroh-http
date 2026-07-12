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

    // P2/L1: the dial_seq of the connection the most recent attempt actually
    // used. Eviction is scoped to this so we only ever close the connection
    // that failed — never a fresh replacement a concurrent request dialed under
    // the same key. A monotonic dial_seq (not a reusable stable_id pointer)
    // makes that guarantee ABA-proof. `None` = no connection was obtained
    // (connect failed / timed out before dialing), in which case eviction is a
    // no-op.
    let last_dial_seq: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

    // Split the request so a transparent retry can rebuild it. A request whose
    // body is empty (exact size 0 — every GET/HEAD/DELETE the FFI layer builds
    // with `Body::empty()`) can be reconstructed byte-for-byte on a fresh
    // connection. A streaming body may have been partially read by a failed
    // attempt, so it is never resent.
    let (parts, body) = req.into_parts();
    let body_resendable = http_body::Body::size_hint(&body).exact() == Some(0);
    let method = parts.method.clone();
    // P1: whether it is SAFE to transparently replay this request. RFC 9110
    // §9.2.2 only permits automatic replay when the method is idempotent AND no
    // response was received — body-emptiness is NOT a substitute for
    // idempotency. The FFI builds `Body::empty()` for *any* method (including
    // POST/PATCH) when no body reader is supplied, and `attempt_send` can
    // surface `ConnectionFailed` even after the peer's (old) serve loop already
    // processed the request bytes (the #336 replace-close race). Retrying an
    // empty-body POST/PATCH would silently double-execute it. Gate the retry on
    // `Method::is_idempotent()` (GET/HEAD/PUT/DELETE/OPTIONS/TRACE = true;
    // POST/PATCH = false). The #119 recovery path and all iOS interop traffic
    // are GET, so this does not regress it.
    let retry_safe = body_resendable && method.is_idempotent();
    let uri = parts.uri.clone();
    let version = parts.version;
    let headers = parts.headers.clone();
    let build_req = |body: Body| -> hyper::Request<Body> {
        let mut builder = hyper::Request::builder()
            .method(method.clone())
            .uri(uri.clone())
            .version(version);
        if let Some(h) = builder.headers_mut() {
            *h = headers.clone();
        }
        // Infallible: method/uri/version/headers all came from a valid request.
        builder
            .body(body)
            .expect("rebuild request from valid parts")
    };

    // Whole-fetch timeout budget: bounds attempt + retry together so a single
    // fetch never exceeds `cfg.timeout` even when it redials once (#336).
    let retry_seq = async {
        // ── Attempt 1: on the pooled (possibly reused) connection. ───────────
        let first_body = if body_resendable { Body::empty() } else { body };
        let (result, reused) = attempt_send(
            endpoint,
            addr,
            node_id,
            build_req(first_body),
            cfg,
            &last_dial_seq,
        )
        .await;

        let first_err = match result {
            Ok(resp) => return Ok(resp),
            Err(e) => e,
        };

        // Only connection-level failures are retryable: a stale pooled
        // connection the peer closed (serve-loop replace, idle close) surfaces
        // as `ConnectionFailed`, and no response head was received so resending
        // is safe. A `Timeout` means the request was likely accepted but slow —
        // resending would double the work, so it is not retried here.
        let retryable = matches!(first_err, FetchError::ConnectionFailed { .. });

        // Evict the (now known-bad) pooled connection so attempt 2 — and any
        // concurrent/future request — dials a fresh one instead of reusing it.
        // Scoped to the connection attempt 1 used (P2).
        if retryable {
            let failed_seq = *last_dial_seq.lock().unwrap_or_else(|e| e.into_inner());
            endpoint
                .pool()
                .evict_and_close(node_id, ALPN, b"iroh-http fetch failed", failed_seq)
                .await;
        }

        // Transparent single retry: only for a *reused* connection (a fresh
        // dial that already failed would just fail again) carrying a
        // resendable, idempotent request (`retry_safe`). This makes pooled-
        // connection reuse across a serve replace transparent (#119) while
        // preserving the #336 invariant that the peer lands on the new serve
        // loop via a fresh connection — and never double-executes a
        // non-idempotent request (P1).
        if retryable && reused && retry_safe {
            let (retry_result, _) = attempt_send(
                endpoint,
                addr,
                node_id,
                build_req(Body::empty()),
                cfg,
                &last_dial_seq,
            )
            .await;
            if let Err(FetchError::ConnectionFailed { .. }) = &retry_result {
                let failed_seq = *last_dial_seq.lock().unwrap_or_else(|e| e.into_inner());
                endpoint
                    .pool()
                    .evict_and_close(node_id, ALPN, b"iroh-http fetch retry failed", failed_seq)
                    .await;
            }
            return retry_result;
        }

        Err(first_err)
    };

    match cfg.timeout {
        Some(t) => match tokio::time::timeout(t, retry_seq).await {
            Ok(r) => r,
            Err(_) => {
                // Timed out across the whole sequence: evict the connection the
                // in-flight attempt was using so the next request starts clean,
                // then surface the bound (#336). Scoped to that connection (P2).
                let failed_seq = *last_dial_seq.lock().unwrap_or_else(|e| e.into_inner());
                endpoint
                    .pool()
                    .evict_and_close(node_id, ALPN, b"iroh-http fetch timed out", failed_seq)
                    .await;
                Err(FetchError::Timeout)
            }
        },
        None => retry_seq.await,
    }
}

/// One connect→open_bi→handshake→send attempt. Returns the result together
/// with whether the connection was **reused** from the pool (see
/// [`crate::http::transport::pool::ConnectionPool::get_or_connect`]), which the
/// caller uses to decide whether a connection failure is worth a single retry.
///
/// Records the [`PooledConnection::dial_seq`](crate::http::transport::pool::PooledConnection::dial_seq)
/// of the connection actually used into `last_dial_seq` (or `None` if no
/// connection was obtained) so the caller can scope eviction to the exact
/// connection that failed (P2/L1).
async fn attempt_send(
    endpoint: &IrohEndpoint,
    addr: &iroh::EndpointAddr,
    node_id: iroh::PublicKey,
    req: hyper::Request<Body>,
    cfg: &StackConfig,
    last_dial_seq: &std::sync::Mutex<Option<u64>>,
) -> (Result<hyper::Response<Body>, FetchError>, bool) {
    let ep_raw = endpoint.raw().clone();
    let addr_clone = addr.clone();
    let max_header_size = endpoint.max_header_size();

    let (pooled, reused) = match endpoint
        .pool()
        .get_or_connect(node_id, ALPN, || async move {
            ep_raw
                .connect(addr_clone, ALPN)
                .await
                .map_err(|e| format!("connect: {e}"))
        })
        .await
    {
        Ok(v) => v,
        Err(e) => {
            // No connection was obtained — clear the identity so a subsequent
            // eviction (e.g. on timeout) is a no-op rather than closing an
            // unrelated cached connection.
            *last_dial_seq.lock().unwrap_or_else(|e| e.into_inner()) = None;
            return (
                Err(FetchError::ConnectionFailed {
                    detail: e,
                    source: None,
                }),
                // A failed connect means nothing was reused.
                false,
            );
        }
    };

    // Record the connection this attempt is about to use so eviction is scoped
    // to it and never closes a fresh replacement dialed by a concurrent request.
    *last_dial_seq.lock().unwrap_or_else(|e| e.into_inner()) = Some(pooled.dial_seq);
    let conn = pooled.conn.clone();

    let (send, recv) = match conn.open_bi().await {
        Ok(pair) => pair,
        Err(e) => {
            return (
                Err(FetchError::ConnectionFailed {
                    detail: format!("open_bi: {e}"),
                    source: None,
                }),
                reused,
            )
        }
    };
    let io = TokioIo::new(IrohStream::new(send, recv));

    let (sender, conn_task) = match hyper::client::conn::http1::Builder::new()
        // hyper requires max_buf_size >= 8192; clamp upward so small
        // max_header_size values don't panic. Header-size enforcement
        // happens at the byte-count check in `ffi::fetch` after the
        // response is returned (deterministic, framing-independent).
        .max_buf_size(max_header_size.max(8192))
        .max_headers(128)
        .handshake::<_, Body>(io)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return (
                Err(FetchError::ConnectionFailed {
                    detail: format!("hyper handshake: {e}"),
                    source: Some(e),
                }),
                reused,
            )
        }
    };

    // Drive the connection state machine in the background.
    tokio::spawn(conn_task);

    // Dispatch through the shared client stack (Slice B / #184).
    use tower::ServiceExt;
    let svc = crate::http::server::stack::build_client_stack(sender, cfg);
    let result = svc
        .oneshot(req)
        .await
        .map_err(|e| FetchError::ConnectionFailed {
            detail: format!("send_request: {e}"),
            source: Some(e),
        });
    (result, reused)
}

// `extract_path` and the hyper-body→channel pumps moved to `ffi/fetch.rs`
// and `ffi/pumps.rs` respectively (Slice D, #186). They were FFI plumbing
// in shape and use; keeping them in `mod http` was the last reason this
// module had to import from `crate::ffi`.
