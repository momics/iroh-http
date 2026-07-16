//! Slice D of #182 (issue #186) — acceptance criterion #1: pure-Rust
//! [`iroh_http_core::fetch_request`] round-trips a [`hyper::Request<Body>`]
//! through [`iroh_http_core::serve`] and returns
//! [`hyper::Response<Body>`] with a typed [`iroh_http_core::FetchError`].
//!
//! No `u64` body handles, no `BodyReader`, no `FfiResponse`. Just `tower`
//! and `hyper`. This is the structural proof that the client side is now
//! shaped exactly like the server side.

mod common;

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Frame, SizeHint};
use http_body_util::BodyExt;
use iroh_http_core::{
    fetch_request, serve, Body, FetchError, RemoteNodeId, ServeOptions, StackConfig,
};
use tower::Service;

#[derive(Clone)]
struct EchoPeerService;

impl Service<hyper::Request<Body>> for EchoPeerService {
    type Response = hyper::Response<Body>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<Body>) -> Self::Future {
        let peer = req
            .extensions()
            .get::<RemoteNodeId>()
            .map(|r| (*r.0).clone())
            .unwrap_or_default();
        let path = req.uri().path().to_string();
        Box::pin(async move {
            let body = format!("path={path} peer={peer}");
            Ok(hyper::Response::builder()
                .status(200)
                .header("content-type", "text/plain")
                .body(Body::full(Bytes::from(body)))
                .expect("static response args are valid"))
        })
    }
}

#[tokio::test]
async fn pure_rust_fetch_round_trips_typed_request_response() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_pk = server_ep.raw().id();
    let server_id_str = server_ep.node_id().to_string();
    let addrs = common::server_addrs(&server_ep);
    let client_id = common::node_id(&client_ep);

    let _handle = serve(server_ep.clone(), ServeOptions::default(), EchoPeerService);

    // Build the EndpointAddr by hand — this is the typed contract the
    // pure-Rust API takes. No flat strings, no tickets.
    let mut addr = iroh::EndpointAddr::new(server_pk);
    for a in &addrs {
        addr = addr.with_ip_addr(*a);
    }

    let req = hyper::Request::builder()
        .method("GET")
        .uri("/hello-typed")
        .header(hyper::header::HOST, &server_id_str)
        .body(Body::empty())
        .expect("valid request");

    let cfg = StackConfig::default();
    let resp = fetch_request(&client_ep, &addr, req, &cfg)
        .await
        .expect("typed fetch ok");

    assert_eq!(resp.status().as_u16(), 200);

    // Drain the typed body — no slotmap, no handle store.
    let collected = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let body = String::from_utf8(collected.to_vec()).expect("utf8 body");
    assert!(body.contains("path=/hello-typed"), "body: {body}");
    assert!(body.contains(&format!("peer={client_id}")), "body: {body}");
}

// ── F9: negative tests for FetchError variants ────────────────────────

#[derive(Clone)]
struct SlowPeerService;

impl Service<hyper::Request<Body>> for SlowPeerService {
    type Response = hyper::Response<Body>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: hyper::Request<Body>) -> Self::Future {
        Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            Ok(hyper::Response::builder()
                .status(200)
                .body(Body::empty())
                .expect("valid response"))
        })
    }
}

/// `cfg.timeout` elapses before the slow peer responds → typed `Timeout`.
#[tokio::test]
async fn pure_rust_fetch_timeout_returns_typed_error() {
    use iroh_http_core::FetchError;

    let (server_ep, client_ep) = common::make_pair().await;
    let server_pk = server_ep.raw().id();
    let addrs = common::server_addrs(&server_ep);

    let _handle = serve(server_ep.clone(), ServeOptions::default(), SlowPeerService);

    let mut addr = iroh::EndpointAddr::new(server_pk);
    for a in &addrs {
        addr = addr.with_ip_addr(*a);
    }

    let req = hyper::Request::builder()
        .method("GET")
        .uri("/slow")
        .body(Body::empty())
        .expect("valid request");

    let cfg = StackConfig::default().with_timeout(Some(std::time::Duration::from_millis(100)));
    let err = fetch_request(&client_ep, &addr, req, &cfg)
        .await
        .err()
        .expect("expected timeout");
    assert!(
        matches!(err, FetchError::Timeout),
        "expected FetchError::Timeout, got {err:?}"
    );
}

struct ZeroDataErrorBody {
    server_started: tokio::sync::oneshot::Receiver<()>,
    emitted: bool,
}

impl http_body::Body for ZeroDataErrorBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.emitted {
            return Poll::Ready(None);
        }
        match std::future::Future::poll(Pin::new(&mut this.server_started), cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(_) => {
                this.emitted = true;
                Poll::Ready(Some(Err(std::io::Error::other("request body failed"))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        false
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(0)
    }
}

/// A body can promise zero DATA bytes while still yielding a trailer or error
/// frame. Such a body is not replayable as `Body::empty()`: doing so turns a
/// local body-production failure into a successful, semantically different
/// request on the retry connection.
#[tokio::test]
async fn zero_data_body_error_surfaces_and_is_not_replayed() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_calls = Arc::new(AtomicUsize::new(0));
    let completed_bodies = Arc::new(AtomicUsize::new(0));
    let (server_started_tx, server_started_rx) = tokio::sync::oneshot::channel();
    let server_started_tx = Arc::new(Mutex::new(Some(server_started_tx)));
    let service_calls = Arc::clone(&server_calls);
    let service_completed_bodies = Arc::clone(&completed_bodies);
    let service_server_started = Arc::clone(&server_started_tx);
    let _handle = serve(
        server_ep.clone(),
        ServeOptions::default(),
        tower::service_fn(move |req: hyper::Request<Body>| {
            let service_calls = Arc::clone(&service_calls);
            let service_completed_bodies = Arc::clone(&service_completed_bodies);
            let service_server_started = Arc::clone(&service_server_started);
            async move {
                let is_failed_path = req.uri().path() == "/must-fail";
                if is_failed_path {
                    service_calls.fetch_add(1, Ordering::SeqCst);
                    if let Some(tx) = service_server_started
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take()
                    {
                        let _ = tx.send(());
                    }
                }
                let body_completed = req.into_body().collect().await.is_ok();
                if is_failed_path && body_completed {
                    service_completed_bodies.fetch_add(1, Ordering::SeqCst);
                }
                Ok::<_, Infallible>(hyper::Response::new(Body::empty()))
            }
        }),
    );

    let mut addr = iroh::EndpointAddr::new(server_ep.raw().id());
    for socket in common::server_addrs(&server_ep) {
        addr = addr.with_ip_addr(socket);
    }
    let cfg = StackConfig::default();

    // Warm the pool so the failing request is eligible for the transparent
    // reused-connection retry path.
    let warm = hyper::Request::builder()
        .method("GET")
        .uri("/warm")
        .body(Body::empty())
        .expect("valid warm-up request");
    fetch_request(&client_ep, &addr, warm, &cfg)
        .await
        .expect("warm-up fetch")
        .into_body()
        .collect()
        .await
        .expect("drain warm-up response");

    let request = hyper::Request::builder()
        .method("PUT")
        .uri("/must-fail")
        .body(Body::new(ZeroDataErrorBody {
            server_started: server_started_rx,
            emitted: false,
        }))
        .expect("valid request");
    let error = match fetch_request(&client_ep, &addr, request, &cfg).await {
        Err(error) => error,
        Ok(_) => panic!("the request body error must reach the caller"),
    };
    assert!(
        matches!(&error, FetchError::RequestBodyFailed { .. }),
        "a caller request-body failure needs its non-transport variant: {error:?}",
    );
    let mut chain = Vec::new();
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(source) = current {
        chain.push(source.to_string());
        current = source.source();
    }
    assert!(
        chain
            .iter()
            .any(|item| item.contains("request body failed")),
        "the injected request-body error must survive in the source chain: {chain:?}",
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while server_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the server should observe the single failed attempt");
    assert_eq!(
        server_calls.load(Ordering::SeqCst),
        1,
        "the failed request must reach the server exactly once, never replay",
    );
    assert_eq!(
        completed_bodies.load(Ordering::SeqCst),
        0,
        "the failed body must never complete as a substituted empty body",
    );
}

/// Connecting to a node id with no reachable direct addrs surfaces the
/// transport failure as `ConnectionFailed`, never as a stringly-typed
/// `Internal` (regression guard for the substring-classifier removal).
#[tokio::test]
async fn pure_rust_fetch_unreachable_addr_returns_connection_failed() {
    use iroh_http_core::FetchError;

    let (_server_ep, client_ep) = common::make_pair().await;

    // Random node id with a bogus, definitely-unreachable address. We need a
    // PublicKey we don't have a connection to, so generate a throwaway key.
    let bogus_secret = iroh::SecretKey::generate();
    let bogus_pk = bogus_secret.public();

    let mut addr = iroh::EndpointAddr::new(bogus_pk);
    addr = addr.with_ip_addr("127.0.0.1:1".parse().expect("valid socket addr"));

    let req = hyper::Request::builder()
        .method("GET")
        .uri("/nope")
        .body(Body::empty())
        .expect("valid request");

    // Short timeout so the test does not block on dial retries.
    let cfg = StackConfig::default().with_timeout(Some(std::time::Duration::from_secs(2)));
    let err = fetch_request(&client_ep, &addr, req, &cfg)
        .await
        .err()
        .expect("expected connection failure");
    assert!(
        matches!(
            err,
            FetchError::ConnectionFailed { .. } | FetchError::Timeout
        ),
        "expected FetchError::ConnectionFailed (or Timeout if dial slow), got {err:?}"
    );
}

// ── Slice E (#187) acceptance #4: large-body streaming roundtrip ───────

#[derive(Clone)]
struct LargeEchoService;

impl Service<hyper::Request<Body>> for LargeEchoService {
    type Response = hyper::Response<Body>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<Body>) -> Self::Future {
        Box::pin(async move {
            // Drain the request body, then send it back. Echo proves the
            // body flowed through hyper → channel → service → channel →
            // hyper without dropping bytes.
            let collected = req
                .into_body()
                .collect()
                .await
                .expect("collect req body")
                .to_bytes();
            Ok(hyper::Response::builder()
                .status(200)
                .header("content-type", "application/octet-stream")
                .body(Body::full(collected))
                .expect("static response args are valid"))
        })
    }
}

/// Streams a 10 MiB body request → response and asserts byte-exact
/// roundtrip. This verifies *correctness* end-to-end (request body flows
/// in via `BodyReader: http_body::Body`; response body flows out via the
/// channel pump in `ffi::pumps`) but not bounded memory — both ends use
/// `Body::full` and `BodyExt::collect`, so the full 10 MiB is materialised
/// in memory by design. A real backpressure / peak-memory test would need
/// a slow-yielding stream body and an explicit memory probe; tracked
/// separately.
#[tokio::test]
async fn pure_rust_fetch_round_trips_10mib_body() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_pk = server_ep.raw().id();
    let server_id_str = server_ep.node_id().to_string();
    let addrs = common::server_addrs(&server_ep);

    let _handle = serve(server_ep.clone(), ServeOptions::default(), LargeEchoService);

    let mut addr = iroh::EndpointAddr::new(server_pk);
    for a in &addrs {
        addr = addr.with_ip_addr(*a);
    }

    // 10 MiB — bigger than the channel buffer so the test exercises
    // backpressure rather than fitting in a single chunk.
    let payload: Bytes = Bytes::from(vec![0xAB; 10 * 1024 * 1024]);

    let req = hyper::Request::builder()
        .method("POST")
        .uri("/echo")
        .header(hyper::header::HOST, &server_id_str)
        .header(hyper::header::CONTENT_LENGTH, payload.len())
        .body(Body::full(payload.clone()))
        .expect("valid request");

    // Generous outer timeout to avoid hanging CI on a real bug.
    let cfg = StackConfig::default().with_timeout(Some(std::time::Duration::from_secs(60)));
    let resp = fetch_request(&client_ep, &addr, req, &cfg)
        .await
        .expect("typed fetch ok");
    assert_eq!(resp.status().as_u16(), 200);

    let collected = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    assert_eq!(collected.len(), payload.len(), "echo length mismatch");
    assert_eq!(collected, payload, "echo content mismatch");
}
