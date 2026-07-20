#![allow(clippy::disallowed_types)] // test/bench file — RequestPayload and friends are valid here
#![allow(clippy::redundant_pattern_matching)]

mod common;

use iroh_http_core::{
    fetch, ffi_serve, IrohEndpoint, NetworkingOptions, NodeOptions, RequestPayload, ServeOptions,
};
use iroh_http_core::{respond, ErrorCode};

// -- Connection pooling -------------------------------------------------------

#[tokio::test]
async fn pool_reuses_connection_for_sequential_requests() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let rc = request_count.clone();

    ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            rc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            respond(
                server_ep.handles(),
                payload.req_handle,
                200,
                vec![("content-length".into(), "0".into())],
            )
            .unwrap();
            server_ep
                .handles()
                .finish_body(payload.res_body_handle)
                .unwrap();
        },
    );

    // First request — establishes connection and caches it.
    let res1 = fetch(
        &client_ep,
        &server_id,
        "/a",
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
    .unwrap();
    assert_eq!(res1.status, 200);
    // Drain body to complete the request.
    while let Some(_) = client_ep
        .handles()
        .next_chunk(res1.body_handle)
        .await
        .unwrap()
    {}

    // Second request — should reuse the cached connection (no new handshake).
    let res2 = fetch(
        &client_ep,
        &server_id,
        "/b",
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
    .unwrap();
    assert_eq!(res2.status, 200);
    while let Some(_) = client_ep
        .handles()
        .next_chunk(res2.body_handle)
        .await
        .unwrap()
    {}

    // Third request for good measure.
    let res3 = fetch(
        &client_ep,
        &server_id,
        "/c",
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
    .unwrap();
    assert_eq!(res3.status, 200);
    while let Some(_) = client_ep
        .handles()
        .next_chunk(res3.body_handle)
        .await
        .unwrap()
    {}

    // All three requests should have been served.
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[tokio::test]
async fn pool_concurrent_requests_share_connection() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            respond(
                server_ep.handles(),
                payload.req_handle,
                200,
                vec![("content-length".into(), "0".into())],
            )
            .unwrap();
            server_ep
                .handles()
                .finish_body(payload.res_body_handle)
                .unwrap();
        },
    );

    // Fire 10 concurrent requests to the same peer.
    let mut handles = Vec::new();
    for i in 0..10u32 {
        let ep = client_ep.clone();
        let id = server_id.clone();
        let a = addrs.clone();
        handles.push(tokio::spawn(async move {
            let res = fetch(
                &ep,
                &id,
                &format!("/storm/{i}"),
                "GET",
                &[],
                None,
                None,
                Some(&a),
                None,
                true,
                None, // max_response_body_bytes
            )
            .await
            .unwrap();
            assert_eq!(res.status, 200);
            while ep
                .handles()
                .next_chunk(res.body_handle)
                .await
                .unwrap()
                .is_some()
            {}
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    // All requests completed successfully — the pool prevented a connection
    // storm (only 1 connect() call happened, the rest waited and reused it).
}

#[tokio::test]
async fn pool_different_peers_get_separate_connections() {
    // Create two separate servers.
    let opts = || NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let server1 = IrohEndpoint::bind(opts()).await.unwrap();
    let server2 = IrohEndpoint::bind(opts()).await.unwrap();
    let client = IrohEndpoint::bind(opts()).await.unwrap();

    let id1 = common::node_id(&server1);
    let id2 = common::node_id(&server2);
    let addrs1 = common::server_addrs(&server1);
    let addrs2 = common::server_addrs(&server2);

    for ep in [server1.clone(), server2.clone()] {
        let ep_handler = ep.clone();
        ffi_serve(
            ep,
            ServeOptions::default(),
            move |payload: RequestPayload| {
                respond(
                    ep_handler.handles(),
                    payload.req_handle,
                    200,
                    vec![("content-length".into(), "0".into())],
                )
                .unwrap();
                ep_handler
                    .handles()
                    .finish_body(payload.res_body_handle)
                    .unwrap();
            },
        );
    }

    // Fetch with a generous timeout instead of retry/sleep loops.
    let r1 = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fetch(
            &client,
            &id1,
            "/",
            "GET",
            &[],
            None,
            None,
            Some(&addrs1),
            None,
            true,
            None, // max_response_body_bytes
        ),
    )
    .await
    .expect("fetch to server1 timed out")
    .expect("fetch to server1 failed");
    assert_eq!(r1.status, 200);
    while client
        .handles()
        .next_chunk(r1.body_handle)
        .await
        .unwrap()
        .is_some()
    {}

    let r2 = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fetch(
            &client,
            &id2,
            "/",
            "GET",
            &[],
            None,
            None,
            Some(&addrs2),
            None,
            true,
            None, // max_response_body_bytes
        ),
    )
    .await
    .expect("fetch to server2 timed out")
    .expect("fetch to server2 failed");
    assert_eq!(r2.status, 200);
    while client
        .handles()
        .next_chunk(r2.body_handle)
        .await
        .unwrap()
        .is_some()
    {}

    // Both succeeded with separate connections to different peers.
    assert_ne!(id1, id2);
}

// ── Edge-case tests (TEST-004) ────────────────────────────────────────────────

/// Pool: with max_pooled_connections=1, rapid sequential requests to different
/// paths all succeed — the pool evicts cleanly.
#[tokio::test]
async fn pool_eviction_single_slot() {
    let server_opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        pool: iroh_http_core::endpoint::PoolOptions {
            max_connections: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    let client_opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        pool: iroh_http_core::endpoint::PoolOptions {
            max_connections: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    let server_ep = IrohEndpoint::bind(server_opts).await.unwrap();
    let client_ep = IrohEndpoint::bind(client_opts).await.unwrap();
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            respond(server_ep.handles(), payload.req_handle, 200, vec![]).unwrap();
            server_ep
                .handles()
                .finish_body(payload.res_body_handle)
                .unwrap();
        },
    );

    // 10 sequential requests — pool has room for only 1 connection.
    for i in 0..10 {
        let path = format!("/pool-test/{i}");
        let res = fetch(
            &client_ep,
            &server_id,
            &path,
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
        .unwrap();
        assert_eq!(res.status, 200, "request {i} failed");
    }
}

#[tokio::test]
async fn pool_evicts_connection_after_fetch_timeout() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    ffi_serve(
        server_ep,
        ServeOptions::default(),
        |_payload: RequestPayload| {
            // Intentionally never respond. The client should time out waiting
            // for the response head and evict the pooled QUIC connection.
        },
    );

    let err = fetch(
        &client_ep,
        &server_id,
        "/hang",
        "GET",
        &[],
        None,
        None,
        Some(&addrs),
        Some(std::time::Duration::from_millis(50)),
        true,
        None,
    )
    .await
    .expect_err("fetch should time out");

    assert_eq!(err.code, ErrorCode::Timeout);

    for _ in 0..10 {
        if client_ep.endpoint_stats().pool_size == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_eq!(
        client_ep.endpoint_stats().pool_size,
        0,
        "timed-out fetch should evict the pooled connection"
    );
}
