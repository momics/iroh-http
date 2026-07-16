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
/// transport errors — they all surface as `ConnectionFailed`. Failures yielded
/// by the caller's request body are identified separately as
/// [`FetchError::RequestBodyFailed`].
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
    /// The caller-supplied request body yielded an error while Hyper was
    /// streaming it. This is a local request-production failure, not evidence
    /// that the pooled transport is unhealthy.
    RequestBodyFailed {
        detail: String,
        source: hyper::Error,
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
            FetchError::RequestBodyFailed { detail, .. } => {
                write!(f, "request body failed: {detail}")
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
            FetchError::RequestBodyFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Marker attached only to errors yielded by the caller-supplied request body.
///
/// Hyper's broad `is_user()` category also includes dispatch-channel failures
/// caused by a dead connection task, so it cannot distinguish body production
/// from transport death. This private marker survives in Hyper's source chain
/// and gives retry/eviction logic an exact origin instead.
#[derive(Debug)]
struct CallerRequestBodyError {
    source: crate::BoxError,
}

impl std::fmt::Display for CallerRequestBodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "caller request body failed: {}", self.source)
    }
}

impl std::error::Error for CallerRequestBodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Tag caller-body failures and preserve non-DATA frames from an exact-zero
/// DATA body that has not reached end-of-stream.
///
/// Hyper treats an exact-zero size hint as a framing decision and may not poll
/// the body at all. That is correct for a body already at end-of-stream, but a
/// still-live body can legally yield trailers or an error frame. Masking only
/// that hint as unknown makes Hyper poll those frames without buffering or
/// otherwise changing the request body.
struct MarkCallerRequestBody {
    body: Body,
    mask_zero_data_hint: bool,
}

impl http_body::Body for MarkCallerRequestBody {
    type Data = bytes::Bytes;
    type Error = CallerRequestBodyError;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match std::pin::Pin::new(&mut this.body).poll_frame(cx) {
            std::task::Poll::Ready(Some(Err(source))) => {
                std::task::Poll::Ready(Some(Err(CallerRequestBodyError { source })))
            }
            std::task::Poll::Ready(Some(Ok(frame))) => std::task::Poll::Ready(Some(Ok(frame))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        if self.mask_zero_data_hint {
            http_body::SizeHint::default()
        } else {
            self.body.size_hint()
        }
    }
}

/// Return the original caller-body failure detail only when the private origin
/// marker is present in Hyper's error source chain.
fn caller_request_body_error_detail(error: &hyper::Error) -> Option<String> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(source) = current {
        if let Some(body_error) = source.downcast_ref::<CallerRequestBodyError>() {
            return Some(body_error.source.to_string());
        }
        current = source.source();
    }
    None
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
/// failures map to [`FetchError::ConnectionFailed`]. A caller-supplied request
/// body error maps to [`FetchError::RequestBodyFailed`] and retains the Hyper
/// and original body-error source chain.
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
    //
    // F18: attempt 1 retains the original request parts and real body. The body
    // gets only a transparent error-origin marker (and, for a still-live
    // exact-zero DATA body, an unknown size hint so Hyper polls trailers/errors).
    // Only the transparent retry, gated on `retry_safe`, is rebuilt from the
    // cloned method/uri/version/headers with a fresh `Body::empty()`.
    let (parts, body) = req.into_parts();
    let body_exact_size = http_body::Body::size_hint(&body).exact();
    let body_at_end = http_body::Body::is_end_stream(&body);
    // `SizeHint::exact(0)` constrains DATA bytes only: a body can still yield
    // trailers or an error frame. `is_end_stream()` is the public guarantee
    // that no frames remain, which is what makes replacing it with a fresh
    // `Body::empty()` semantically safe on retry.
    let body_resendable = body_at_end && body_exact_size == Some(0);
    let body = if body_at_end {
        body
    } else {
        Body::new(MarkCallerRequestBody {
            body,
            mask_zero_data_hint: body_exact_size == Some(0),
        })
    };
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
    // Only used to rebuild a transparent retry — never for attempt 1.
    let build_retry = |body: Body| -> hyper::Request<Body> {
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
    // Reassemble the untouched first request without cloning the body.
    let first_req = hyper::Request::from_parts(parts, body);

    // Whole-fetch timeout budget: bounds attempt + retry together so a single
    // fetch never exceeds `cfg.timeout` even when it redials once (#336).
    let retry_seq = async {
        // ── Attempt 1: on the pooled (possibly reused) connection. ───────────
        let (result, reused) =
            attempt_send(endpoint, addr, node_id, first_req, cfg, &last_dial_seq).await;

        let first_err = match result {
            Ok(resp) => return Ok(resp),
            Err(e) => e,
        };

        // Only connection-level failures are retryable: a stale pooled
        // connection the peer closed (serve-loop replace, idle close) surfaces
        // as `ConnectionFailed`, and no response head was received so resending
        // is safe. A `Timeout` means the request was likely accepted but slow —
        // resending would double the work, so it is not retried here.
        // Caller-body failures are classified separately via an exact marker
        // in Hyper's source chain. Do not use `hyper::Error::is_user()` here:
        // that broad category includes dispatch-channel loss when the
        // connection task dies, which is precisely a stale transport that
        // should be evicted and retried when the request is replay-safe.
        let retryable = matches!(&first_err, FetchError::ConnectionFailed { .. });

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
                build_retry(Body::empty()),
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
                // Timed out across the whole sequence. F6: invalidate the pool
                // entry so the next request redials, but do NOT close the shared
                // QUIC connection — it may still be multiplexing other in-flight
                // requests on independent bi-streams, and closing it would reset
                // them (cascading this local timeout into unrelated failures).
                // This request's own stream is reset when the cancelled
                // `retry_seq` future drops its send/recv handles. Scoped to the
                // connection the in-flight attempt used (P2).
                let failed_seq = *last_dial_seq.lock().unwrap_or_else(|e| e.into_inner());
                endpoint.pool().evict_only(node_id, ALPN, failed_seq).await;
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
    let result = svc.oneshot(req).await.map_err(|error| {
        if let Some(detail) = caller_request_body_error_detail(&error) {
            FetchError::RequestBodyFailed {
                detail,
                source: error,
            }
        } else {
            FetchError::ConnectionFailed {
                detail: format!("send_request: {error}"),
                source: Some(error),
            }
        }
    });
    (result, reused)
}

// `extract_path` and the hyper-body→channel pumps moved to `ffi/fetch.rs`
// and `ffi/pumps.rs` respectively (Slice D, #186). They were FFI plumbing
// in shape and use; keeping them in `mod http` was the last reason this
// module had to import from `crate::ffi`.
