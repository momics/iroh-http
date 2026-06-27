//! FFI shaped serve + respond — the JS-callback bridge over the
//! pure-Rust [`crate::http::server::serve_with_events`] entry.
//!
//! Per epic #182 this module is the **only** place that allocates `u64`
//! handles, fires the [`RequestPayload`] callback, and rendezvous on the
//! response head from JS. Everything below is one specific implementation
//! of the generic `tower::Service<Request<Body>>` accepted by the pure-Rust
//! serve entry — handed in via [`ffi_serve_with_callback`].
//!
//! Architecture-test guarantee: this file may import from `crate::http`
// Legitimate FFI wiring — uses the disallowed types intentionally.
#![allow(clippy::disallowed_types)]
//! (the `ffi → http` direction is allowed); `crate::http::*` must NOT
//! import from here.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::StatusCode;
use tower::Service;

use crate::ffi::handles::ResponseHeadEntry;
use crate::ffi::pumps::pump_hyper_body_to_channel;
use crate::http::server::{
    serve_with_events, ConnectionEventFn, RemoteNodeId, ServeHandle, ServeOptions,
};
use crate::{Body, CoreError, IrohEndpoint, RequestPayload};

// ── Inline error responses ─────────────────────────────────────────────────

fn internal_error(detail: &'static [u8]) -> hyper::Response<Body> {
    hyper::Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::full(Bytes::from_static(detail)))
        .expect("static error response args are valid")
}

fn service_unavailable(detail: &'static [u8]) -> hyper::Response<Body> {
    hyper::Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .body(Body::full(Bytes::from_static(detail)))
        .expect("static error response args are valid")
}

// ── respond() ─────────────────────────────────────────────────────────────

/// Send the response head for a request handle previously delivered to JS
/// via the [`RequestPayload::req_handle`] callback.
///
/// Partner of the `head_rx` rendezvous in [`FfiDispatcher::dispatch`]. The
/// JS handler calls this exactly once per request to provide the status +
/// headers; subsequent body bytes flow through the response body channel
/// referenced by [`RequestPayload::res_body_handle`].
pub fn respond(
    handles: &crate::ffi::handles::HandleStore,
    req_handle: u64,
    status: u16,
    headers: Vec<(String, String)>,
) -> Result<(), CoreError> {
    StatusCode::from_u16(status)
        .map_err(|_| CoreError::invalid_input(format!("invalid HTTP status code: {status}")))?;
    for (name, value) in &headers {
        http::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            CoreError::invalid_input(format!("invalid response header name {:?}", name))
        })?;
        http::HeaderValue::from_str(value).map_err(|_| {
            CoreError::invalid_input(format!("invalid response header value for {:?}", name))
        })?;
    }

    let sender = handles
        .take_req_sender(req_handle)
        .ok_or_else(|| CoreError::invalid_handle(req_handle))?;
    sender
        .send(ResponseHeadEntry { status, headers })
        .map_err(|_| CoreError::internal("serve task dropped before respond"))
}

// ── ReqHeadGuard ──────────────────────────────────────────────────────────
//
// RAII guard that removes the per-request slab entry from the handle
// store on every dispatch exit path (success, error, panic). Lifted to
// module scope from inline-in-`dispatch` per Slice C.4 of #182 (#178).

struct ReqHeadGuard {
    endpoint: IrohEndpoint,
    req_handle: u64,
}

impl Drop for ReqHeadGuard {
    fn drop(&mut self) {
        self.endpoint.handles().take_req_sender(self.req_handle);
    }
}

// ── FfiDispatcher + IrohHttpService ───────────────────────────────────────

/// FFI-shaped tower service: allocates request/response handles, fires the
/// JS `on_request` callback, and rendezvous on the response head sent via
/// [`respond`]. Shared across every accepted connection and request via
/// `Arc`.
struct FfiDispatcher {
    on_request: Arc<dyn Fn(RequestPayload) + Send + Sync>,
    endpoint: IrohEndpoint,
    own_node_id: Arc<String>,
    max_header_size: Option<usize>,
}

#[derive(Clone)]
pub(crate) struct IrohHttpService {
    dispatcher: Arc<FfiDispatcher>,
}

/// ADR-014 D2 / #175: service is concrete — not generic over `B`.
/// Body normalisation happens upstream at the hyper → tower seam in
/// `crate::http::server::pipeline::serve_bistream`.
impl Service<hyper::Request<Body>> for IrohHttpService {
    type Response = hyper::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<Body>) -> Self::Future {
        let dispatcher = self.dispatcher.clone();
        // #177: the authenticated peer id arrives as a request extension
        // inserted by the per-connection AddExtensionLayer in
        // serve_with_events. Missing extension is a server-side
        // bug (the layer is unconditional) — fall back to empty string.
        let remote_node_id = req
            .extensions()
            .get::<RemoteNodeId>()
            .map(|r| r.0.clone())
            .unwrap_or_else(|| Arc::new(String::new()));
        Box::pin(async move { Ok(dispatcher.dispatch(req, remote_node_id).await) })
    }
}

impl FfiDispatcher {
    async fn dispatch(
        self: Arc<Self>,
        req: hyper::Request<Body>,
        remote_node_id: Arc<String>,
    ) -> hyper::Response<Body> {
        let handles = self.endpoint.handles();
        let own_node_id = &*self.own_node_id;
        let max_header_size = self.max_header_size;

        let method = req.method().to_string();
        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
            .to_string();

        // Log the path only, never the query string: query parameters commonly
        // carry tokens or sensitive identifiers that must not leak into logs.
        let path_only = req.uri().path();
        tracing::debug!(
            method = %method,
            path = %path_only,
            peer = %remote_node_id,
            "iroh-http: incoming request",
        );

        // Strip any client-supplied peer-id to prevent spoofing,
        // then inject the authenticated identity from the QUIC connection.
        //
        // ISS-011: Use raw byte length for header-size accounting to prevent
        // bypass via non-UTF8 values.  Reject non-UTF8 header values with 400
        // instead of silently converting them to empty strings.

        // First pass: measure header bytes using raw values (before lossy conversion).
        if let Some(limit) = max_header_size {
            let header_bytes: usize = req
                .headers()
                .iter()
                .filter(|(k, _)| !k.as_str().eq_ignore_ascii_case("peer-id"))
                .map(|(k, v)| {
                    k.as_str()
                        .len()
                        .saturating_add(v.as_bytes().len())
                        .saturating_add(4)
                }) // ": " + "\r\n"
                .fold(0usize, |acc, x| acc.saturating_add(x))
                .saturating_add("peer-id".len())
                .saturating_add(remote_node_id.len())
                .saturating_add(4)
                .saturating_add(req.uri().to_string().len())
                .saturating_add(method.len())
                .saturating_add(12); // "HTTP/1.1 \r\n\r\n" overhead
            if header_bytes > limit {
                let resp = hyper::Response::builder()
                    .status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
                    .body(Body::empty())
                    .expect("static response args are valid");
                return resp;
            }
        }

        // Build header list — reject non-UTF8 values instead of silently dropping.
        let mut req_headers: Vec<(String, String)> = Vec::new();
        for (k, v) in req.headers().iter() {
            if k.as_str().eq_ignore_ascii_case("peer-id") {
                continue;
            }
            match v.to_str() {
                Ok(s) => req_headers.push((k.as_str().to_string(), s.to_string())),
                Err(_) => {
                    let resp = hyper::Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::full(Bytes::from_static(b"non-UTF8 header value")))
                        .expect("static response args are valid");
                    return resp;
                }
            }
        }
        req_headers.push(("peer-id".to_string(), (*remote_node_id).clone()));

        let url = format!("httpi://{own_node_id}{path_and_query}");

        // ── Allocate channels ────────────────────────────────────────────────

        let mut guard = handles.insert_guard();
        let (req_body_writer, req_body_reader) = handles.make_body_channel();
        let req_body_handle = match guard.insert_reader(req_body_reader) {
            Ok(h) => h,
            Err(_) => return service_unavailable(b"server handle table full"),
        };

        let (res_body_writer, res_body_reader) = handles.make_body_channel();
        let res_body_handle = match guard.insert_writer(res_body_writer) {
            Ok(h) => h,
            Err(_) => return service_unavailable(b"server handle table full"),
        };

        let (head_tx, head_rx) = tokio::sync::oneshot::channel::<ResponseHeadEntry>();
        let req_handle = match guard.allocate_req_handle(head_tx) {
            Ok(h) => h,
            Err(_) => return service_unavailable(b"server handle table full"),
        };

        guard.commit();

        let _req_head_guard = ReqHeadGuard {
            endpoint: self.endpoint.clone(),
            req_handle,
        };

        // ── Pump request body ────────────────────────────────────────────────

        let body = req.into_body();
        tokio::spawn(pump_hyper_body_to_channel(body, req_body_writer));

        // ── Fire on_request callback ─────────────────────────────────────────

        (self.on_request)(RequestPayload {
            req_handle,
            req_body_handle,
            res_body_handle,
            method,
            url,
            headers: req_headers,
            remote_node_id: Arc::unwrap_or_clone(remote_node_id),
            is_bidi: false,
        });

        // ── Await response head from JS ──────────────────────────────────────
        let response_head = match head_rx.await {
            Ok(h) => h,
            Err(_) => return internal_error(b"JS handler dropped without responding"),
        };

        // ── Regular HTTP response ─────────────────────────────────────────────

        let mut resp_builder = hyper::Response::builder().status(response_head.status);
        for (k, v) in &response_head.headers {
            resp_builder = resp_builder.header(k.as_str(), v.as_str());
        }

        match resp_builder.body(Body::new(res_body_reader)) {
            Ok(r) => r,
            Err(_) => internal_error(b"failed to build response head from JS"),
        }
    }
}

// ── ffi_serve_with_callback ───────────────────────────────────────────────

/// FFI-shaped serve entry. Constructs an [`IrohHttpService`] around the
/// supplied callback and delegates to the pure-Rust
/// [`crate::http::server::serve_with_events`].
///
/// `on_connection_event` is called on 0→1 (first connection from a peer)
/// and 1→0 (last connection from a peer closed) count transitions.
///
/// # Security
///
/// Calling this opens a **public endpoint** on the Iroh overlay network.
/// Any peer that knows or discovers your node's public key can connect
/// and send requests. Iroh QUIC authenticates the peer's *identity*
/// cryptographically, but does not enforce *authorization*. Always
/// inspect [`RequestPayload::remote_node_id`] and reject untrusted peers.
pub fn ffi_serve_with_callback<F>(
    endpoint: IrohEndpoint,
    options: ServeOptions,
    on_request: F,
    on_connection_event: Option<ConnectionEventFn>,
) -> ServeHandle
where
    F: Fn(RequestPayload) + Send + Sync + 'static,
{
    let max_header_size = endpoint.max_header_size();
    let own_node_id = Arc::new(endpoint.node_id().to_string());
    let on_request = Arc::new(on_request) as Arc<dyn Fn(RequestPayload) + Send + Sync>;

    let dispatcher = Arc::new(FfiDispatcher {
        on_request,
        endpoint: endpoint.clone(),
        own_node_id,
        max_header_size: if max_header_size == 0 {
            None
        } else {
            Some(max_header_size)
        },
    });
    let svc = IrohHttpService { dispatcher };

    serve_with_events(endpoint, options, svc, on_connection_event)
}

/// Back-compat 3-arg FFI serve entry: equivalent to
/// [`ffi_serve_with_callback`] with `on_connection_event = None`. The Node /
/// Deno / Tauri adapters call this directly.
pub fn ffi_serve<F>(endpoint: IrohEndpoint, options: ServeOptions, on_request: F) -> ServeHandle
where
    F: Fn(RequestPayload) + Send + Sync + 'static,
{
    ffi_serve_with_callback(endpoint, options, on_request, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::handles::{HandleStore, StoreConfig};
    use crate::{NetworkingOptions, NodeOptions};
    use http::HeaderValue;
    use std::sync::Mutex;

    // ── respond(): validation + head delivery ────────────────────────────

    #[tokio::test]
    async fn respond_rejects_invalid_status() {
        let store = HandleStore::new(StoreConfig::default());
        // 9999 is outside the valid 100..=999 status range.
        let err = respond(&store, 0, 9999, vec![]).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn respond_rejects_invalid_header_name() {
        let store = HandleStore::new(StoreConfig::default());
        let err = respond(&store, 0, 200, vec![("bad name".into(), "v".into())]).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn respond_rejects_invalid_header_value() {
        let store = HandleStore::new(StoreConfig::default());
        // A newline in a header value is rejected (header injection guard).
        let err = respond(&store, 0, 200, vec![("x".into(), "bad\nvalue".into())]).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn respond_unknown_handle_errors() {
        let store = HandleStore::new(StoreConfig::default());
        // Status and headers are valid, but no such request handle exists.
        let err = respond(&store, 4242, 200, vec![("x".into(), "y".into())]).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::InvalidInput);
        assert!(err.message.contains("4242"));
    }

    #[tokio::test]
    async fn respond_delivers_head_to_receiver() {
        let store = HandleStore::new(StoreConfig::default());
        let (tx, rx) = tokio::sync::oneshot::channel::<ResponseHeadEntry>();
        let handle = store.allocate_req_handle(tx).unwrap();

        respond(&store, handle, 201, vec![("x-test".into(), "yes".into())]).unwrap();

        let head = rx.await.unwrap();
        assert_eq!(head.status, 201);
        assert_eq!(
            head.headers,
            vec![("x-test".to_string(), "yes".to_string())]
        );
        // The sender is consumed: a second respond on the same handle fails.
        let err = respond(&store, handle, 200, vec![]).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::InvalidInput);
    }

    // ── dispatch(): malformed-request rejection (ISS-011 / ISS-003) ───────

    async fn local_endpoint() -> IrohEndpoint {
        IrohEndpoint::bind(NodeOptions {
            networking: NetworkingOptions {
                disabled: true,
                bind_addrs: vec!["127.0.0.1:0".into()],
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap()
    }

    fn make_dispatcher(
        endpoint: IrohEndpoint,
        max_header_size: Option<usize>,
        sink: Arc<Mutex<Vec<RequestPayload>>>,
    ) -> Arc<FfiDispatcher> {
        let own_node_id = Arc::new(endpoint.node_id().to_string());
        Arc::new(FfiDispatcher {
            on_request: Arc::new(move |p| sink.lock().unwrap().push(p)),
            endpoint,
            own_node_id,
            max_header_size,
        })
    }

    /// ISS-011: a non-UTF8 header value must be rejected with 400, not
    /// silently coerced, and the JS callback must never fire.
    #[tokio::test]
    async fn dispatch_rejects_non_utf8_header_value() {
        let endpoint = local_endpoint().await;
        let sink = Arc::new(Mutex::new(Vec::new()));
        let dispatcher = make_dispatcher(endpoint, None, sink.clone());

        let mut req = hyper::Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();
        req.headers_mut()
            .insert("x-bad", HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap());

        let resp = dispatcher.dispatch(req, Arc::new(String::new())).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(
            sink.lock().unwrap().is_empty(),
            "callback must not fire on a rejected request",
        );
    }

    /// ISS-003: request headers exceeding `max_header_size` are rejected with
    /// 431 before any handle is allocated or the callback fires.
    #[tokio::test]
    async fn dispatch_rejects_oversized_headers() {
        let endpoint = local_endpoint().await;
        let sink = Arc::new(Mutex::new(Vec::new()));
        let dispatcher = make_dispatcher(endpoint, Some(50), sink.clone());

        let req = hyper::Request::builder()
            .method("GET")
            .uri("/")
            .header("x-big", "a".repeat(200))
            .body(Body::empty())
            .unwrap();

        let resp = dispatcher.dispatch(req, Arc::new(String::new())).await;
        assert_eq!(resp.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
        assert!(
            sink.lock().unwrap().is_empty(),
            "callback must not fire on a rejected request",
        );
    }
}
