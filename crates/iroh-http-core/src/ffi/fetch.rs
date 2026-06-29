//! FFI-shaped `fetch` — flat-string wrapper around the pure-Rust
//! [`crate::http::client::fetch_request`].
//!
//! Slice D (#186): translates the JS adapter calling convention into a
//! typed [`hyper::Request<Body>`] / [`iroh::EndpointAddr`] pair, hands
//! them to [`crate::http::client::fetch_request`], and re-packages the
//! response as a [`FfiResponse`] with a slotmap body handle. Maps
//! [`crate::http::client::FetchError`] variants onto [`crate::CoreError`]
//! codes for the FFI boundary.
// Legitimate FFI wiring — uses the disallowed types intentionally.
#![allow(clippy::disallowed_types)]

use std::time::Duration;

use http::{HeaderName, HeaderValue, Method};

use crate::{
    ffi::{handles::BodyReader, pumps::pump_hyper_body_to_channel_limited},
    http::client::{fetch_request, FetchError},
    parse_node_addr, Body, CoreError, FfiResponse, IrohEndpoint,
};

/// FFI-shaped fetch — re-exported as `iroh_http_core::fetch` for FFI
/// binary compatibility (Slice D acceptance #5). Composition mirrors the
/// pre-Slice-D function exactly; the moving parts that became pure Rust
/// live in [`crate::http::client::fetch_request`].
#[allow(clippy::too_many_arguments)]
pub async fn fetch(
    endpoint: &IrohEndpoint,
    remote_node_id: &str,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    req_body_reader: Option<BodyReader>,
    fetch_token: Option<u64>,
    direct_addrs: Option<&[std::net::SocketAddr]>,
    timeout: Option<Duration>,
    decompress: bool,
    max_response_body_bytes: Option<usize>,
) -> Result<FfiResponse, CoreError> {
    // Reject standard web schemes.
    {
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") {
            let scheme_end = lower
                .find("://")
                .map(|i| i.saturating_add(3))
                .unwrap_or(lower.len());
            return Err(CoreError::invalid_input(format!(
                "iroh-http URLs must use the \"httpi://\" scheme, not \"{}\". \
                 Example: httpi://nodeId/path",
                &url[..scheme_end]
            )));
        }
    }

    // Validate method and headers at the FFI boundary.
    let http_method = Method::from_bytes(method.as_bytes())
        .map_err(|_| CoreError::invalid_input(format!("invalid HTTP method {:?}", method)))?;
    for (name, value) in headers {
        HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| CoreError::invalid_input(format!("invalid header name {:?}", name)))?;
        HeaderValue::from_str(value).map_err(|_| {
            CoreError::invalid_input(format!("invalid header value for {:?}", name))
        })?;
    }

    // Resolve the EndpointAddr once: bare node id, ticket, or JSON, plus
    // any caller-supplied direct addresses.
    let parsed = parse_node_addr(remote_node_id)?;
    let mut addr = iroh::EndpointAddr::new(parsed.node_id);
    for a in &parsed.direct_addrs {
        addr = addr.with_ip_addr(*a);
    }
    if let Some(addrs) = direct_addrs {
        for a in addrs {
            addr = addr.with_ip_addr(*a);
        }
    }
    let remote_str = crate::base32_encode(parsed.node_id.as_bytes());
    let path = extract_path(url);

    // Build the typed Request<Body>.
    let mut req_builder = hyper::Request::builder()
        .method(http_method)
        .uri(&path)
        .header(hyper::header::HOST, &remote_str);

    // When compression is enabled, advertise zstd-only Accept-Encoding —
    // but only if the caller has not already set Accept-Encoding. A caller
    // passing `Accept-Encoding: identity` is opting out of compression and
    // must not be overridden.
    {
        let has_accept_encoding = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"));
        if !has_accept_encoding {
            req_builder = req_builder.header("accept-encoding", "zstd");
        }
    }
    for (k, v) in headers {
        req_builder = req_builder.header(k.as_str(), v.as_str());
    }

    let req_body: Body = if let Some(reader) = req_body_reader {
        Body::new(reader)
    } else {
        Body::empty()
    };
    let req = req_builder
        .body(req_body)
        .map_err(|e| CoreError::internal(format!("build request: {e}")))?;

    // Self-request (ADR-015): a node fetching its own node id. iroh's
    // transport forbids self-dial ("Connecting to ourself is not supported"),
    // so route the request in-process to this node's own serve service instead
    // of attempting a QUIC connection.
    if remote_str == endpoint.node_id() {
        return self_fetch(
            endpoint,
            req,
            &remote_str,
            &path,
            fetch_token,
            timeout,
            max_response_body_bytes,
        )
        .await;
    }

    let cancel_notify = fetch_token.and_then(|t| endpoint.handles().get_fetch_cancel_notify(t));

    // Wire FFI-supplied knobs into the shared stack config.
    // - `timeout` bounds time-to-response-head inside `fetch_request`.
    // - `decompress` toggles the tower-http Decompression layer.
    // Per-frame body-read timeout and the response-body byte limit are
    // enforced below by `pump_hyper_body_to_channel_limited`; cancellation
    // is handled by the `tokio::select!` on the cancel-token notifier.
    let cfg = crate::http::server::stack::StackConfig {
        timeout,
        decompression: decompress,
        ..crate::http::server::stack::StackConfig::default()
    };

    let fetch_fut = fetch_request(endpoint, &addr, req, &cfg);
    let resp = match cancel_notify {
        Some(notify) => tokio::select! {
            _ = notify.notified() => {
                if let Some(t) = fetch_token {
                    endpoint.handles().remove_fetch_token(t);
                }
                return Err(CoreError::cancelled());
            }
            r = fetch_fut => r,
        },
        None => fetch_fut.await,
    };

    // Always clean up the cancellation token, even on error.
    if let Some(t) = fetch_token {
        endpoint.handles().remove_fetch_token(t);
    }

    let resp = resp.map_err(fetch_error_to_core)?;

    package_response(endpoint, resp, &remote_str, &path, max_response_body_bytes).await
}

/// In-process self-request — dispatch a `fetch()` to this node's own id
/// directly to the locally-registered serve service (ADR-015).
///
/// iroh's transport refuses self-dial, so there is no QUIC connection: the
/// request is handed to the exact same [`crate::ffi::dispatcher::IrohHttpService`]
/// that remote peers reach, as an in-process `tower::Service` call. The node's
/// own id is injected as the authenticated [`crate::http::server::RemoteNodeId`]
/// — truthful, since the peer is us. Cancellation and `timeout` mirror the QUIC
/// path; the response is packaged identically via [`package_response`].
///
/// This deliberately bypasses the wire: no QUIC/TLS handshake, no compression
/// negotiation, and none of the per-connection server stack (timeouts and body
/// limits applied at the accept loop). A self-request is therefore not a
/// network-reachability check. See ADR-015 for the full semantics.
#[allow(clippy::too_many_arguments)]
async fn self_fetch(
    endpoint: &IrohEndpoint,
    mut req: hyper::Request<Body>,
    remote_str: &str,
    path: &str,
    fetch_token: Option<u64>,
    timeout: Option<Duration>,
    max_response_body_bytes: Option<usize>,
) -> Result<FfiResponse, CoreError> {
    use tower::ServiceExt;

    let svc = endpoint.local_service().ok_or_else(|| {
        CoreError::connection_failed(
            "self-request: this node has no active server to handle a request to its \
             own node id. Call serve() before fetching httpi://<your-own-node-id>/…",
        )
    })?;

    // The QUIC path receives the authenticated peer id from the per-connection
    // AddExtensionLayer; in-process we inject it directly so the serve handler
    // still sees a truthful `Peer-Id` (our own id).
    req.extensions_mut()
        .insert(crate::http::server::RemoteNodeId(std::sync::Arc::new(
            remote_str.to_string(),
        )));

    // `IrohHttpService` is `Infallible`; dispatch and await the response head.
    let dispatch = async move {
        match svc.oneshot(req).await {
            Ok(resp) => resp,
            Err(never) => match never {},
        }
    };

    let timed = async {
        match timeout {
            Some(t) => tokio::time::timeout(t, dispatch)
                .await
                .map_err(|_| CoreError::timeout("request timed out")),
            None => Ok(dispatch.await),
        }
    };

    let cancel_notify = fetch_token.and_then(|t| endpoint.handles().get_fetch_cancel_notify(t));
    let result: Result<hyper::Response<Body>, CoreError> = match cancel_notify {
        Some(notify) => tokio::select! {
            _ = notify.notified() => Err(CoreError::cancelled()),
            r = timed => r,
        },
        None => timed.await,
    };

    // Remove the cancel token on every exit — success, timeout, or cancel —
    // before propagating, so a timed-out self-fetch cannot leak a handle.
    if let Some(t) = fetch_token {
        endpoint.handles().remove_fetch_token(t);
    }
    let resp = result?;

    package_response(endpoint, resp, remote_str, path, max_response_body_bytes).await
}

/// Translate the typed [`FetchError`] surface into the flat
/// [`CoreError`] the FFI boundary expects.
fn fetch_error_to_core(e: FetchError) -> CoreError {
    match e {
        FetchError::ConnectionFailed { detail, .. } => CoreError::connection_failed(detail),
        FetchError::HeaderTooLarge { detail } => CoreError::header_too_large(detail),
        FetchError::BodyTooLarge => CoreError::body_too_large("response body too large"),
        FetchError::Timeout => CoreError::timeout("request timed out"),
        FetchError::Cancelled => CoreError::cancelled(),
        FetchError::Internal(msg) => CoreError::internal(msg),
    }
}

/// Extract the path portion from an `httpi://nodeId/path?query#frag` URL.
///
/// Lives next to [`fetch`] because that is the sole caller — it constructs
/// the request-target line for the outgoing HTTP/1.1 request.
pub(crate) fn extract_path(url: &str) -> String {
    // The fragment is client-local (RFC 3986 §3.5) and must never appear on
    // the wire. Drop everything from the first '#' before extracting the path,
    // as defense-in-depth in case a caller bypasses the JS-side stripping.
    let url = url.split('#').next().unwrap_or(url);
    if let Some(rest) = url.strip_prefix("httpi://") {
        if let Some(slash) = rest.find('/') {
            return rest[slash..].to_string();
        }
        return "/".to_string();
    }
    if url.starts_with('/') {
        return url.to_string();
    }
    format!("/{url}")
}

/// Validate response head, allocate body channel, and assemble [`FfiResponse`].
///
/// Lives in `ffi::fetch` because every step here exists to satisfy the
/// FFI contract: header-byte budget enforcement, RFC-9110 null-body
/// handling, slotmap body handle allocation, `Vec<(String, String)>`
/// header conversion. The pure-Rust caller would just consume
/// `resp.into_body()` directly.
async fn package_response(
    endpoint: &IrohEndpoint,
    resp: hyper::Response<Body>,
    remote_str: &str,
    path: &str,
    max_response_body_bytes: Option<usize>,
) -> Result<FfiResponse, CoreError> {
    let max_header_size = endpoint.max_header_size();
    // Per-call limit takes precedence; fall back to the endpoint-wide default.
    let max_response_body_bytes =
        max_response_body_bytes.unwrap_or_else(|| endpoint.max_response_body_bytes());
    let handles = endpoint.handles();

    let status = resp.status().as_u16();
    // ISS-011: measure header bytes using raw values before string conversion;
    // reject non-UTF8 response header values deterministically.
    let header_bytes: usize = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            k.as_str()
                .len()
                .saturating_add(v.as_bytes().len())
                .saturating_add(4) // "name: value\r\n"
        })
        .fold(16usize, |acc, x| acc.saturating_add(x)); // approximate status line
    if header_bytes > max_header_size {
        return Err(CoreError::header_too_large(format!(
            "response header size {header_bytes} exceeds limit {max_header_size}"
        )));
    }

    let mut resp_headers: Vec<(String, String)> = Vec::new();
    for (k, v) in resp.headers().iter() {
        match v.to_str() {
            Ok(s) => resp_headers.push((k.as_str().to_string(), s.to_string())),
            Err(_) => {
                return Err(CoreError::invalid_input(format!(
                    "non-UTF8 response header value for '{}'",
                    k.as_str()
                )));
            }
        }
    }

    let response_url = format!("httpi://{remote_str}{path}");

    // RFC 9110 §6.3: responses with status 204, 205, or 304 MUST NOT carry a
    // message body. Skip channel allocation entirely and return the slotmap
    // null sentinel (0) for body_handle so the JS layer can use
    // `bodyHandle === 0n` as a clean structural check without re-encoding
    // HTTP semantics in every adapter.
    if matches!(status, 204 | 205 | 304) {
        // Dropping the body signals to hyper that we are done reading.
        // For a spec-compliant server the body is already empty; this is a
        // defensive drain for misbehaving peers.
        drop(resp.into_body());
        return Ok(FfiResponse {
            status,
            headers: resp_headers,
            body_handle: 0,
            url: response_url,
        });
    }

    // Allocate channels for streaming the response body to JS.
    let mut guard = handles.insert_guard();
    let (res_writer, res_reader) = handles.make_body_channel();
    let body = resp.into_body();
    let frame_timeout = res_writer.drain_timeout;
    tokio::spawn(pump_hyper_body_to_channel_limited(
        body,
        res_writer,
        Some(max_response_body_bytes),
        frame_timeout,
        None,
    ));

    let body_handle = guard.insert_reader(res_reader)?;
    guard.commit();
    Ok(FfiResponse {
        status,
        headers: resp_headers,
        body_handle,
        url: response_url,
    })
}

#[cfg(test)]
mod tests {
    use super::extract_path;

    #[test]
    fn extracts_path_and_query() {
        assert_eq!(extract_path("httpi://node/a/b?c=1"), "/a/b?c=1");
        assert_eq!(extract_path("httpi://node/"), "/");
        assert_eq!(extract_path("httpi://node"), "/");
    }

    #[test]
    fn strips_fragment_from_request_target() {
        // Fragments are client-local (RFC 3986 §3.5) and must never reach the
        // peer, even if a caller bypasses the JS-side stripping.
        assert_eq!(extract_path("httpi://node/a?b=1#secret"), "/a?b=1");
        assert_eq!(extract_path("httpi://node/a#frag"), "/a");
        assert_eq!(extract_path("httpi://node/#frag"), "/");
        assert_eq!(extract_path("httpi://node#frag"), "/");
        assert_eq!(extract_path("/path?q=1#frag"), "/path?q=1");
    }
}
