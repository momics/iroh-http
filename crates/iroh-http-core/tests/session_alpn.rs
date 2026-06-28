//! Session ALPN enforcement.
//!
//! [`Session::accept`] must reject connections that negotiated a non-duplex
//! ALPN (e.g. plain HTTP). Surfacing such a connection as a raw session would
//! be protocol confusion: a peer that handshook for HTTP must not be handed a
//! bidirectional session. This is a regression guard for the ALPN check in
//! `ffi::session::Session::accept`.

use iroh_http_core::{ErrorCode, IrohEndpoint, NetworkingOptions, NodeOptions, Session, ALPN};

/// Two locally-connected endpoints (relay disabled, loopback only). Both
/// advertise the duplex and plain-HTTP ALPNs by default.
async fn make_pair() -> (IrohEndpoint, IrohEndpoint) {
    let opts = || NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let a = IrohEndpoint::bind(opts()).await.unwrap();
    let b = IrohEndpoint::bind(opts()).await.unwrap();
    (a, b)
}

#[tokio::test]
async fn accept_rejects_non_duplex_alpn() {
    let (client_ep, server_ep) = make_pair().await;

    let server_pk = server_ep.raw().id();
    let addrs: Vec<std::net::SocketAddr> = server_ep.raw().addr().ip_addrs().cloned().collect();
    let mut addr = iroh::EndpointAddr::new(server_pk);
    for a in &addrs {
        addr = addr.with_ip_addr(*a);
    }

    // Server accepts the next incoming connection and must reject it.
    let server_handle = tokio::spawn(async move { Session::accept(server_ep).await });

    // Client connects with the plain HTTP ALPN (advertised by the server),
    // NOT the duplex session ALPN.
    let conn = client_ep
        .raw()
        .connect(addr, ALPN)
        .await
        .expect("handshake negotiates the HTTP ALPN");
    assert_eq!(conn.alpn(), ALPN, "client negotiated the HTTP ALPN");

    let err = server_handle
        .await
        .unwrap()
        .err()
        .expect("accept must reject a non-duplex ALPN");
    // Assert the machine-readable code first; the message is an extra guard.
    assert_eq!(err.code, ErrorCode::ConnectionFailed);
    assert!(
        err.message.contains("unexpected ALPN"),
        "unexpected error: {}",
        err.message,
    );

    // Hold the client connection until after the assertions.
    drop(conn);
}
