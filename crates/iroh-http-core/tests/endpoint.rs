use iroh_http_core::{IrohEndpoint, NetworkingOptions, NodeOptions, ServeOptions};

// -- Endpoint basics ----------------------------------------------------------

#[tokio::test]
async fn endpoint_node_id_is_stable() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts).await.unwrap();
    let id1 = ep.node_id().to_string();
    let id2 = ep.node_id().to_string();
    assert_eq!(id1, id2);
    assert!(!id1.is_empty());
}

#[tokio::test]
async fn endpoint_deterministic_key() {
    let key = [42u8; 32];
    let opts1 = NodeOptions {
        key: Some(key),
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let opts2 = NodeOptions {
        key: Some(key),
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let ep1 = IrohEndpoint::bind(opts1).await.unwrap();
    let ep2 = IrohEndpoint::bind(opts2).await.unwrap();
    assert_eq!(ep1.node_id(), ep2.node_id());
}

#[tokio::test]
async fn endpoint_secret_key_round_trip() {
    // Any 32 bytes are a valid Ed25519 seed; a fixed key lets us assert that
    // rebinding with the same key yields a deterministic node ID.
    let key_bytes = [7u8; 32];

    let opts = NodeOptions {
        key: Some(key_bytes),
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts).await.unwrap();

    // Rebinding with the same key should produce the same node ID
    let opts2 = NodeOptions {
        key: Some(key_bytes),
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let ep2 = IrohEndpoint::bind(opts2).await.unwrap();
    assert_eq!(ep.node_id(), ep2.node_id());
}

#[tokio::test]
async fn endpoint_bound_sockets_non_empty() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts).await.unwrap();
    let sockets = ep.bound_sockets();
    assert!(!sockets.is_empty(), "bound_sockets should not be empty");
}

// Regression: #346 — every direct (non-relay) address in a bound endpoint's
// advertised `node_addr()` must carry a real, non-zero port. On iOS the derived
// address arrived with port 0, so peers rejected it with
// `invalid socket address syntax`. The port is reconciled from `bound_sockets`.
#[tokio::test]
async fn node_addr_direct_addresses_carry_nonzero_port() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts).await.unwrap();
    let info = ep.node_addr();
    for a in &info.addrs {
        // Relay URLs are not socket addresses — skip them.
        if a.contains("://") {
            continue;
        }
        let sock: std::net::SocketAddr = a
            .parse()
            .unwrap_or_else(|e| panic!("advertised direct address {a:?} must parse: {e}"));
        assert_ne!(
            sock.port(),
            0,
            "advertised direct address {a:?} must carry a non-zero port"
        );
    }
}

#[tokio::test]
async fn endpoint_close() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts).await.unwrap();
    ep.close().await;
    // After close, connecting should fail
}

// ── Transport liveness vs. handle liveness (#336) ─────────────────────────────

/// A freshly bound endpoint reports its transport as usable; once closed it
/// must not. This is the primitive the mobile foreground probe needs — a live
/// registry handle is not evidence that the underlying transport still works.
#[tokio::test]
async fn transport_alive_reflects_close() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts).await.unwrap();
    assert!(
        ep.transport_alive(),
        "a freshly bound endpoint must report a usable transport"
    );

    ep.close().await;
    assert!(
        !ep.transport_alive(),
        "a closed endpoint must not report its transport as usable"
    );
}

/// Regression guard for #336: the endpoint *handle* can still resolve from the
/// registry after the transport is force-closed. Foreground recovery must key
/// off transport liveness, not handle existence — otherwise a half-live iOS
/// node looks healthy and desktop peers hang until a long timeout.
#[tokio::test]
async fn handle_liveness_is_not_transport_liveness() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts).await.unwrap();
    let handle = iroh_http_core::insert_endpoint(ep.clone());

    ep.close_force().await;

    // The handle still exists in the registry ...
    let resolved = iroh_http_core::get_endpoint(handle)
        .expect("handle should still resolve after force-close");
    // ... but the transport behind it is no longer usable.
    assert!(
        !resolved.transport_alive(),
        "transport must report dead even though the handle still resolves"
    );

    iroh_http_core::remove_endpoint(handle);
}

#[tokio::test]
async fn serve_options_defaults() {
    let opts = ServeOptions::default();
    assert_eq!(opts.max_concurrency, None);
    assert_eq!(opts.drain_timeout_ms, None);
}

// ── Edge-case tests (TEST-004) ────────────────────────────────────────────────

/// Registry: get_endpoint after remove_endpoint returns None without panic.
#[tokio::test]
async fn registry_get_after_remove_returns_none() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts).await.unwrap();
    let handle = iroh_http_core::insert_endpoint(ep);

    let got = iroh_http_core::get_endpoint(handle);
    assert!(got.is_some());

    let removed = iroh_http_core::remove_endpoint(handle);
    assert!(removed.is_some());

    // Second remove returns None.
    let removed_again = iroh_http_core::remove_endpoint(handle);
    assert!(removed_again.is_none());

    // Get after remove returns None.
    let got_after = iroh_http_core::get_endpoint(handle);
    assert!(got_after.is_none());
}

/// Registry: get_endpoint with a bogus handle returns None without panic.
#[tokio::test]
async fn registry_bogus_handle_returns_none() {
    let got = iroh_http_core::get_endpoint(999_999);
    assert!(got.is_none());
}

/// sweep_interval_ms: a short TTL + fast sweep interval causes leaked handles
/// to be evicted promptly.  sweep_now() lets tests trigger immediate sweeps
/// without waiting for the background tick.
#[tokio::test]
async fn sweep_interval_ms_evicts_handles() {
    // 50 ms TTL; background sweep at 100 ms (validates interval wiring).
    let ep = iroh_http_core::IrohEndpoint::bind(iroh_http_core::NodeOptions {
        networking: iroh_http_core::NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        streaming: iroh_http_core::StreamingOptions {
            handle_ttl_ms: Some(50),
            sweep_interval_ms: Some(100),
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap();

    // Allocate a writer handle — immediately "leak" it (don't finish or cancel).
    let (writer_handle, _reader) = ep.handles().alloc_body_writer().unwrap();

    // The handle should exist right now.
    assert!(
        ep.handles().finish_body(writer_handle).is_ok(),
        "writer handle should be valid immediately after alloc"
    );

    // Allocate another and truly leak it by not calling finish_body.
    let (leaked_handle, _reader2) = ep.handles().alloc_body_writer().unwrap();

    // Wait > TTL so the handle's creation timestamp is past the TTL window.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    // Trigger an immediate sweep rather than waiting for the background tick.
    ep.sweep_now();

    // The leaked handle should now be gone — finish_body returns an error.
    let evicted = ep.handles().finish_body(leaked_handle).is_err();
    assert!(
        evicted,
        "leaked handle was not evicted by sweep_now() after TTL expired"
    );
}

// ── ALPN capability validation ────────────────────────────────────────────────

#[tokio::test]
async fn bind_rejects_unknown_alpn_capability() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        capabilities: vec!["totally-bogus-alpn".into()],
        ..Default::default()
    };
    let result = IrohEndpoint::bind(opts).await;
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("bind should reject unknown ALPN capability"),
    };
    assert!(
        err.message.contains("unknown ALPN capability"),
        "error message should mention the invalid value, got: {err}"
    );
}

#[tokio::test]
async fn bind_accepts_known_alpn_capabilities() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        capabilities: vec![
            iroh_http_core::ALPN_STR.to_string(),
            iroh_http_core::ALPN_DUPLEX_STR.to_string(),
        ],
        ..Default::default()
    };
    let ep = IrohEndpoint::bind(opts)
        .await
        .expect("valid ALPNs should be accepted");
    ep.close().await;
}

// ── Compression level validation ──────────────────────────────────────────────

#[tokio::test]
async fn bind_rejects_out_of_range_compression_level() {
    let opts = NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        compression: Some(iroh_http_core::CompressionOptions {
            min_body_bytes: 512,
            level: Some(99),
        }),
        ..Default::default()
    };
    let result = IrohEndpoint::bind(opts).await;
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("bind should reject compression level 99"),
    };
    assert!(
        err.message.contains("compression level"),
        "error should mention compression level, got: {err}"
    );
}

#[test]
fn compression_options_default_min_body_bytes_is_1_kib() {
    // Regression guard for #167: default must agree with the documented
    // value in docs/features/compression.md (1 KiB).
    let opts = iroh_http_core::CompressionOptions::default();
    assert_eq!(opts.min_body_bytes, 1024);
    assert_eq!(opts.level, None);
    assert_eq!(
        iroh_http_core::CompressionOptions::DEFAULT_MIN_BODY_BYTES,
        1024
    );
}
