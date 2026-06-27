//! Rust-side unit tests for the iroh-http Tauri plugin.
//!
//! Uses Tauri's mock runtime to exercise command handlers without a real
//! webview.  Tests cover endpoint lifecycle, body handle management, the
//! binary prefix-byte protocol for chunks, and Ed25519 key operations.

mod command_tests {
    use crate::commands::*;
    use crate::state;
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use tauri::ipc::{InvokeResponseBody, IpcResponse};

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Extract raw bytes from a `tauri::ipc::Response`.
    fn response_bytes(resp: tauri::ipc::Response) -> Vec<u8> {
        match resp.body().expect("response body") {
            InvokeResponseBody::Raw(bytes) => bytes,
            InvokeResponseBody::Json(s) => s.into_bytes(),
        }
    }

    /// Build a minimal mock Tauri app handle for tests.
    ///
    /// `AppHandle` is Arc-backed internally: the app internals stay alive as
    /// long as any handle clone exists, even after the `App` value is dropped.
    /// No `SchemeState` is managed, so `create_endpoint`'s scheme bind is a
    /// no-op in all tests below.
    fn mock_handle() -> tauri::AppHandle<tauri::test::MockRuntime> {
        tauri::test::mock_app().handle().clone()
    }

    /// Create a real endpoint in loopback/offline mode for testing.
    async fn make_test_endpoint() -> u64 {
        let result = create_endpoint(mock_handle(), Some(CreateEndpointArgs {
            key: None,
            idle_timeout: None,
            relay_mode: Some("disabled".into()),
            relays: None,
            bind_addrs: Some(vec!["127.0.0.1:0".into()]),
            dns_discovery: None,
            dns_discovery_enabled: Some(false),
            channel_capacity: None,
            max_chunk_size_bytes: None,
            handle_ttl: None,
            sweep_interval: None,
            disable_networking: Some(true),
            proxy_url: None,
            proxy_from_env: None,
            keylog: None,
            compression_min_body_bytes: None,
            compression_level: None,
            max_header_bytes: None,
            max_pooled_connections: None,
            pool_idle_timeout_ms: None,
        }))
        .await
        .expect("create_endpoint should succeed");

        assert!(!result.node_id.is_empty(), "node_id should be non-empty");
        assert_eq!(result.keypair.len(), 32, "keypair should be 32 bytes");
        result.endpoint_handle
    }

    // ── Endpoint lifecycle ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_and_close_endpoint() {
        let handle = make_test_endpoint().await;

        // Endpoint should be reachable.
        let alive = ping(handle).await;
        assert!(alive.is_ok(), "ping should succeed");
        assert!(alive.unwrap(), "ping should return true");

        // Close it.
        close_endpoint(handle, None).await.expect("close should succeed");

        // Note: we do NOT assert ping(handle) fails here because the global
        // slab reuses freed indices — a concurrent test may have already
        // inserted a new endpoint at the same slot.
    }

    #[tokio::test]
    async fn test_create_endpoint_default_args() {
        // create_endpoint(None) should bind with defaults.
        let result = create_endpoint(mock_handle(), None).await;
        assert!(result.is_ok(), "default create_endpoint should succeed");
        let info = result.unwrap();
        assert!(!info.node_id.is_empty());
        close_endpoint(info.endpoint_handle, None).await.ok();
    }

    #[tokio::test]
    async fn test_close_invalid_handle() {
        let err = close_endpoint(99999, None).await;
        assert!(err.is_err(), "closing invalid handle should error");
        let msg = err.unwrap_err();
        assert!(msg.contains("INVALID_HANDLE"), "error should mention INVALID_HANDLE: {msg}");
    }

    #[tokio::test]
    async fn test_ping_invalid_handle() {
        let err = ping(99999).await;
        assert!(err.is_err(), "ping on invalid handle should error");
    }

    // ── Address introspection ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_node_addr() {
        let handle = make_test_endpoint().await;
        let addr = node_addr(handle);
        assert!(addr.is_ok(), "node_addr should succeed");
        let addr = addr.unwrap();
        assert!(!addr.id.is_empty(), "node_addr id should be non-empty");

        close_endpoint(handle, None).await.ok();
    }

    #[tokio::test]
    async fn test_node_addr_invalid_handle() {
        let err = node_addr(99999);
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_endpoint_stats() {
        let handle = make_test_endpoint().await;
        let stats = endpoint_stats(handle);
        assert!(stats.is_ok(), "endpoint_stats should succeed");

        close_endpoint(handle, None).await.ok();
    }

    // ── Body handle lifecycle ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_body_writer() {
        let ep_handle = make_test_endpoint().await;
        let writer = create_body_writer(ep_handle);
        assert!(writer.is_ok(), "create_body_writer should succeed");
        let writer_handle = writer.unwrap();
        assert!(writer_handle > 0, "writer handle should be non-zero");

        close_endpoint(ep_handle, None).await.ok();
    }

    #[tokio::test]
    async fn test_create_fetch_token() {
        let ep_handle = make_test_endpoint().await;
        let token = create_fetch_token(ep_handle);
        assert!(token.is_ok(), "create_fetch_token should succeed");

        // Cancel it — should not error.
        let cancel = cancel_in_flight(ep_handle, token.unwrap());
        assert!(cancel.is_ok(), "cancel_in_flight should succeed");

        close_endpoint(ep_handle, None).await.ok();
    }

    #[tokio::test]
    async fn test_body_writer_invalid_handle() {
        let err = create_body_writer(99999);
        assert!(err.is_err());
    }

    // ── next_chunk / try_next_chunk: binary prefix protocol ───────────────────

    #[tokio::test]
    async fn test_next_chunk_prefix_byte_protocol() {
        let ep_handle = make_test_endpoint().await;

        // Allocate a body channel and insert the reader into the reader registry
        // (not the pending-reader map) so next_chunk can find it.
        let ep = state::get_endpoint(ep_handle).expect("endpoint exists");
        let (writer_handle, reader) = ep.handles().alloc_body_writer().expect("alloc writer");
        let reader_handle = ep.handles().insert_reader(reader).expect("insert reader");

        // Send a chunk via the writer.
        let test_data = bytes::Bytes::from_static(b"hello world");
        ep.handles()
            .send_chunk(writer_handle, test_data.clone())
            .await
            .expect("send_chunk should succeed");

        // Read it back via the command — should have prefix byte 0x01.
        let response = next_chunk(ep_handle, reader_handle).await;
        assert!(response.is_ok(), "next_chunk should succeed");
        let body = response_bytes(response.unwrap());
        assert_eq!(body[0], 1u8, "prefix byte should be 0x01 (data follows)");
        assert_eq!(&body[1..], b"hello world", "data should match");

        // Signal EOF.
        ep.handles().finish_body(writer_handle).ok();

        // Next read should return EOF: prefix byte 0x00.
        let eof = next_chunk(ep_handle, reader_handle).await;
        assert!(eof.is_ok());
        let eof_body = response_bytes(eof.unwrap());
        assert_eq!(eof_body[0], 0u8, "prefix byte should be 0x00 (EOF)");

        close_endpoint(ep_handle, None).await.ok();
    }

    #[tokio::test]
    async fn test_try_next_chunk_eof() {
        let ep_handle = make_test_endpoint().await;
        let ep = state::get_endpoint(ep_handle).expect("endpoint exists");
        let (writer_handle, reader) = ep.handles().alloc_body_writer().expect("alloc writer");
        let reader_handle = ep.handles().insert_reader(reader).expect("insert reader");

        // Close immediately — EOF.
        ep.handles().finish_body(writer_handle).ok();

        // try_next_chunk should see EOF.
        let result = try_next_chunk(ep_handle, reader_handle);
        assert!(result.is_ok());
        let body = response_bytes(result.unwrap());
        assert_eq!(body[0], 0u8, "should be EOF (0x00)");

        close_endpoint(ep_handle, None).await.ok();
    }

    // ── finish_body / cancel_request ──────────────────────────────────────────

    #[tokio::test]
    async fn test_finish_body() {
        let ep_handle = make_test_endpoint().await;
        let writer_handle = create_body_writer(ep_handle).expect("alloc writer");

        let result = finish_body(ep_handle, writer_handle);
        assert!(result.is_ok(), "finish_body should succeed");

        close_endpoint(ep_handle, None).await.ok();
    }

    #[tokio::test]
    async fn test_cancel_request_invalid_handle() {
        let ep_handle = make_test_endpoint().await;
        // cancel_request on a non-existent handle — should not panic, just no-op.
        let result = cancel_request(ep_handle, 99999);
        assert!(result.is_ok(), "cancel_request should not error on unknown handle");
        close_endpoint(ep_handle, None).await.ok();
    }

    // ── Ed25519 key operations ────────────────────────────────────────────────

    #[test]
    fn test_generate_secret_key() {
        let result = generate_secret_key();
        assert!(result.is_ok());
        let b64 = result.unwrap();
        let decoded = B64.decode(&b64).expect("valid base64");
        assert_eq!(decoded.len(), 32, "secret key should be 32 bytes");
    }

    #[tokio::test]
    async fn test_sign_and_verify() {
        // Create an endpoint to get a real keypair and node_id (public key).
        let info = create_endpoint(mock_handle(), Some(CreateEndpointArgs {
            key: None,
            idle_timeout: None,
            relay_mode: Some("disabled".into()),
            relays: None,
            bind_addrs: Some(vec!["127.0.0.1:0".into()]),
            dns_discovery: None,
            dns_discovery_enabled: Some(false),
            channel_capacity: None,
            max_chunk_size_bytes: None,
            handle_ttl: None,
            sweep_interval: None,
            disable_networking: Some(true),
            proxy_url: None,
            proxy_from_env: None,
            keylog: None,
            compression_min_body_bytes: None,
            compression_level: None,
            max_header_bytes: None,
            max_pooled_connections: None,
            pool_idle_timeout_ms: None,
        }))
        .await
        .expect("create_endpoint");

        // Secret key from endpoint info (32 bytes).
        let sk_b64 = B64.encode(&info.keypair);
        // Public key: decode node_id from base32 (lowercase, no padding).
        let pk_bytes = base32::decode(
            base32::Alphabet::Rfc4648Lower { padding: false },
            &info.node_id,
        )
        .expect("valid base32 node_id");
        let pk_b64 = B64.encode(&pk_bytes);

        let data = B64.encode(b"test message for signing");

        // Sign.
        let sig_b64 = secret_key_sign(sk_b64, data.clone()).expect("sign should succeed");

        // Verify — should return true.
        let valid =
            public_key_verify(pk_b64.clone(), data.clone(), sig_b64.clone()).expect("verify ok");
        assert!(valid, "signature should be valid");

        // Tampered data — should return false.
        let bad_data = B64.encode(b"tampered message");
        let invalid =
            public_key_verify(pk_b64, bad_data, sig_b64).expect("verify should not error");
        assert!(!invalid, "tampered data should fail verification");

        close_endpoint(info.endpoint_handle, None).await.ok();
    }

    #[test]
    fn test_sign_invalid_key() {
        let bad_key = B64.encode(b"too short");
        let data = B64.encode(b"data");
        let err = secret_key_sign(bad_key, data);
        assert!(err.is_err(), "signing with wrong-size key should fail");
    }

    #[test]
    fn test_verify_invalid_signature() {
        let pk = B64.encode([0u8; 32]);
        let data = B64.encode(b"data");
        let bad_sig = B64.encode(b"not 64 bytes");
        let err = public_key_verify(pk, data, bad_sig);
        assert!(err.is_err(), "verify with wrong-size sig should fail");
    }

    // ── Force close ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_force_close() {
        let handle = make_test_endpoint().await;
        close_endpoint(handle, Some(true)).await.expect("force close should succeed");
        // Note: we do NOT assert ping(handle) fails here because the global
        // slab reuses freed indices — a concurrent test may have already
        // inserted a new endpoint at the same slot.
    }
}
