#![allow(clippy::disallowed_types)] // test file — RequestPayload is valid here
//! HTTP accept-loop guards.
//!
//! Regression tests for two defences in the HTTP serve loop that had no
//! dedicated end-to-end coverage:
//!
//!  * `accept.rs` rejects a connection that negotiated a non-HTTP ALPN
//!    (protocol-confusion guard), and
//!  * `pipeline.rs` bounds the request-head read so a peer cannot hold a
//!    bistream open by withholding header bytes (slowloris guard).

mod common;

use std::time::Duration;

use iroh_http_core::{ffi_serve, IrohEndpoint, RequestPayload, ServeOptions, ALPN, ALPN_DUPLEX};

/// Build an `EndpointAddr` for the server from its public key + direct addrs.
fn server_addr(server_ep: &IrohEndpoint) -> iroh::EndpointAddr {
    let mut addr = iroh::EndpointAddr::new(server_ep.raw().id());
    for a in common::server_addrs(server_ep) {
        addr = addr.with_ip_addr(a);
    }
    addr
}

/// The HTTP serve loop must reject a connection that negotiated the duplex
/// session ALPN instead of the HTTP ALPN — handing it to the HTTP service
/// would be protocol confusion.
#[tokio::test]
async fn serve_loop_rejects_non_http_alpn() {
    let (server_ep, client_ep) = common::make_pair().await;
    let addr = server_addr(&server_ep);

    let _serve = ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        |_p: RequestPayload| {},
    );

    // Connect with the duplex ALPN (advertised by the server), not HTTP.
    let conn = client_ep
        .raw()
        .connect(addr, ALPN_DUPLEX)
        .await
        .expect("duplex ALPN negotiates — the server advertises it");
    assert_eq!(conn.alpn(), ALPN_DUPLEX);

    // The HTTP accept loop must close it promptly with "unexpected ALPN".
    let err = tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .expect("server should close the non-HTTP connection, not hang");
    match err {
        iroh::endpoint::ConnectionError::ApplicationClosed(info) => {
            assert_eq!(&info.reason[..], b"unexpected ALPN");
        }
        other => panic!("expected an application close, got: {other:?}"),
    }
}

/// A peer that opens a bistream but never sends a complete request head must
/// be timed out: the server closes the stream rather than waiting forever.
#[tokio::test]
async fn serve_loop_times_out_slow_request_head() {
    let (server_ep, client_ep) = common::make_pair().await;
    let addr = server_addr(&server_ep);

    // Short head-read budget (reuses the request timeout).
    let opts = ServeOptions {
        request_timeout_ms: Some(200),
        ..Default::default()
    };
    let _serve = ffi_serve(server_ep.clone(), opts, |_p: RequestPayload| {});

    let conn = client_ep
        .raw()
        .connect(addr, ALPN)
        .await
        .expect("HTTP ALPN negotiates");
    let (mut send, mut recv) = conn.open_bi().await.expect("open bistream");

    // Send a partial request head and stop: the server's accept_bi yields (so
    // hyper begins reading the head) but the head never terminates. Without
    // the head-read timeout the server would block forever and this read would
    // hang past the outer budget; with it, the server resets/closes the stream
    // and the read resolves well within the window. `send` is kept alive so
    // the stream stays open (no finish/reset from our side).
    send.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("write partial head");
    let read = tokio::time::timeout(Duration::from_secs(5), recv.read_to_end(1024)).await;
    assert!(
        read.is_ok(),
        "server must time out a slow request head instead of hanging",
    );
    drop(send);
}
