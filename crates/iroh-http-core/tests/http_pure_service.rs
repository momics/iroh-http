//! Slice C.1 of #182 — acceptance criterion #1: the pure-Rust serve entry
//! ([`iroh_http_core::serve`] / [`iroh_http_core::serve_with_events`]) is callable from a
//! Rust integration test with a hand-rolled
//! `Service<Request<Body>, Response = Response<Body>, Error = Infallible>`.
//!
//! No callbacks, no `u64` handles, no `RequestPayload` — just `tower`.
//!
//! This is the structural proof that "the FFI is the API" has flipped to
//! "the FFI is one consumer of the API". Closes the major axis of #182:
//! a Rust application can serve HTTP-over-Iroh without ever touching the
//! handle store.

mod common;

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use iroh_http_core::{fetch, serve, Body, RemoteNodeId, ServeOptions};
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
        // Closes #177: read the authenticated peer id from the request
        // extension inserted by the per-connection AddExtensionLayer.
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
async fn pure_rust_serve_round_trips_with_peer_id_extension() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);
    let client_id = common::node_id(&client_ep);

    let _handle = serve(server_ep.clone(), ServeOptions::default(), EchoPeerService);

    let res = fetch(
        &client_ep,
        &server_id,
        "/hello-pure",
        "GET",
        &[],
        None,
        None,
        Some(&addrs),
        None,
        true,
        None, // max_response_body_bytes
    )
    .await
    .expect("fetch ok");
    assert_eq!(res.status, 200);

    // Drain the response body via the FFI handle (the client side is
    // unchanged in this slice).
    let mut body = Vec::new();
    while let Some(chunk) = client_ep
        .handles()
        .next_chunk(res.body_handle)
        .await
        .expect("chunk read ok")
    {
        body.extend_from_slice(&chunk);
    }
    let body = String::from_utf8(body).expect("utf8 body");
    assert!(body.contains("path=/hello-pure"), "body: {body}");
    assert!(body.contains(&format!("peer={client_id}")), "body: {body}");

    // Make Arc references reachable so import is not unused on doc scrape
    let _ = Arc::new(());
}

/// A pure-Rust serve cycle is owned by the endpoint just like an FFI serve
/// cycle. Callers must not need a second, adapter-specific registration call
/// before `stop_serve()` can reach the loop.
#[tokio::test]
async fn pure_rust_serve_is_stoppable_in_one_call() {
    let (server_ep, _client_ep) = common::make_pair().await;
    let handle = serve(
        server_ep.clone(),
        ServeOptions::default(),
        tower::service_fn(|_req| async {
            Ok::<_, Infallible>(hyper::Response::new(Body::empty()))
        }),
    );
    let mut done = handle.subscribe_done();

    server_ep.stop_serve();

    tokio::time::timeout(Duration::from_secs(5), done.wait_for(|value| *value))
        .await
        .expect("one stop_serve call must stop the pure-Rust serve cycle")
        .expect("serve done channel must remain open");
}
