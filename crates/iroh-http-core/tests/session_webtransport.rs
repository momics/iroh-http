//! WebTransport session tests — datagrams, unidirectional streams, close info.

use bytes::Bytes;
use iroh_http_core::{IrohEndpoint, NetworkingOptions, NodeOptions, Session};

/// Create a pair of locally-connected endpoints (relay disabled).
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

fn node_id(ep: &IrohEndpoint) -> String {
    ep.node_id().to_string()
}

fn direct_addrs(ep: &IrohEndpoint) -> Vec<std::net::SocketAddr> {
    ep.raw().addr().ip_addrs().cloned().collect()
}

// -- Unidirectional streams ---------------------------------------------------

#[tokio::test]
async fn session_uni_stream_send_recv() {
    let (a_ep, b_ep) = make_pair().await;
    let b_id = node_id(&b_ep);
    let b_addrs = direct_addrs(&b_ep);

    // B accepts a session and reads an incoming uni stream.
    let b_ep_spawn = b_ep.clone();
    let b_handle = tokio::spawn(async move {
        let session_b = Session::accept(b_ep_spawn.clone()).await.unwrap().unwrap();
        let read_handle = session_b.next_uni_stream().await.unwrap().unwrap();

        let mut data = Vec::new();
        while let Some(chunk) = b_ep_spawn.handles().next_chunk(read_handle).await.unwrap() {
            data.extend_from_slice(&chunk);
        }

        (session_b, data)
    });

    // A opens a uni stream and writes data.
    let session_a = Session::connect(a_ep.clone(), &b_id, Some(&b_addrs))
        .await
        .unwrap();
    let write_handle = session_a.create_uni_stream().await.unwrap();

    a_ep.handles()
        .send_chunk(write_handle, Bytes::from_static(b"uni-hello"))
        .await
        .unwrap();
    a_ep.handles().finish_body(write_handle).unwrap();

    let (session_b, data) = b_handle.await.unwrap();
    assert_eq!(data, b"uni-hello");

    session_b.close(0, "").ok();
    session_a.close(0, "").ok();
}

#[tokio::test]
async fn session_multiple_uni_streams() {
    let (a_ep, b_ep) = make_pair().await;
    let b_id = node_id(&b_ep);
    let b_addrs = direct_addrs(&b_ep);

    let b_ep_spawn = b_ep.clone();
    let b_handle = tokio::spawn(async move {
        let session_b = Session::accept(b_ep_spawn.clone()).await.unwrap().unwrap();

        let mut messages = Vec::new();
        for _ in 0..3 {
            let read_handle = session_b.next_uni_stream().await.unwrap().unwrap();
            let mut data = Vec::new();
            while let Some(chunk) = b_ep_spawn.handles().next_chunk(read_handle).await.unwrap() {
                data.extend_from_slice(&chunk);
            }
            messages.push(data);
        }

        (session_b, messages)
    });

    let session_a = Session::connect(a_ep.clone(), &b_id, Some(&b_addrs))
        .await
        .unwrap();

    for i in 0..3u8 {
        let write_handle = session_a.create_uni_stream().await.unwrap();
        let msg = format!("msg-{i}");
        a_ep.handles()
            .send_chunk(write_handle, Bytes::from(msg.into_bytes()))
            .await
            .unwrap();
        a_ep.handles().finish_body(write_handle).unwrap();
    }

    let (session_b, messages) = b_handle.await.unwrap();
    // Order might vary, so just check we got all 3.
    assert_eq!(messages.len(), 3);
    let mut sorted: Vec<String> = messages
        .iter()
        .map(|m| String::from_utf8(m.clone()).unwrap())
        .collect();
    sorted.sort();
    assert_eq!(sorted, vec!["msg-0", "msg-1", "msg-2"]);

    session_b.close(0, "").ok();
    session_a.close(0, "").ok();
}

// -- Datagrams ----------------------------------------------------------------

#[tokio::test]
async fn session_datagram_round_trip() {
    let (a_ep, b_ep) = make_pair().await;
    let b_id = node_id(&b_ep);
    let b_addrs = direct_addrs(&b_ep);

    let b_ep_spawn = b_ep.clone();
    let b_handle = tokio::spawn(async move {
        let session_b = Session::accept(b_ep_spawn).await.unwrap().unwrap();

        // Receive a datagram.
        let data = session_b.recv_datagram().await.unwrap().unwrap();

        // Send one back.
        session_b.send_datagram(b"pong").unwrap();

        (session_b, data)
    });

    let session_a = Session::connect(a_ep.clone(), &b_id, Some(&b_addrs))
        .await
        .unwrap();

    // Check max datagram size is available.
    let max_size = session_a.max_datagram_size().unwrap();
    assert!(max_size.is_some(), "datagrams should be supported");
    assert!(max_size.unwrap() > 0);

    // Send a datagram.
    session_a.send_datagram(b"ping").unwrap();

    // Receive the reply.
    let reply = session_a.recv_datagram().await.unwrap().unwrap();
    assert_eq!(reply, b"pong");

    let (session_b, received) = b_handle.await.unwrap();
    assert_eq!(received, b"ping");

    session_b.close(0, "").ok();
    session_a.close(0, "").ok();
}

// -- Close info ---------------------------------------------------------------

#[tokio::test]
async fn session_close_with_code_and_reason() {
    let (a_ep, b_ep) = make_pair().await;
    let b_id = node_id(&b_ep);
    let b_addrs = direct_addrs(&b_ep);

    let b_ep_spawn = b_ep.clone();
    let b_handle = tokio::spawn(async move {
        let session_b = Session::accept(b_ep_spawn).await.unwrap().unwrap();

        // Wait for session to be closed by A.
        session_b.closed().await.unwrap()
    });

    let session_a = Session::connect(a_ep.clone(), &b_id, Some(&b_addrs))
        .await
        .unwrap();

    // Close with a specific code and reason.
    session_a.close(42, "done").unwrap();

    let info = b_handle.await.unwrap();
    assert_eq!(info.close_code, 42);
    assert_eq!(info.reason, "done");
}

// -- Idempotent close ---------------------------------------------------------

#[tokio::test]
async fn session_double_close_is_idempotent() {
    let (a_ep, b_ep) = make_pair().await;
    let b_id = node_id(&b_ep);
    let b_addrs = direct_addrs(&b_ep);

    let b_ep_spawn = b_ep.clone();
    let b_handle = tokio::spawn(async move {
        let session_b = Session::accept(b_ep_spawn).await.unwrap().unwrap();
        session_b.closed().await.unwrap();
    });

    let session_a = Session::connect(a_ep.clone(), &b_id, Some(&b_addrs))
        .await
        .unwrap();

    // Closing twice must not error — `close` is safe to call repeatedly.
    session_a.close(0, "first").unwrap();
    session_a.close(0, "second").unwrap();

    // closed() is also idempotent: the first call removes the registry entry,
    // the second returns a default CloseInfo instead of erroring.
    let _first = session_a.closed().await.unwrap();
    let second = session_a.closed().await.unwrap();
    assert_eq!(second.close_code, 0);
    assert_eq!(second.reason, "");

    b_handle.await.unwrap();
}
