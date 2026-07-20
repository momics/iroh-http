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

    /// Preserve the concise lifecycle calls below while supplying the
    /// WebView-aware command's app context.
    async fn close_endpoint(handle: u64, force: Option<bool>) -> Result<(), String> {
        crate::commands::close_endpoint(mock_handle(), handle, force).await
    }

    /// Create a real endpoint in loopback/offline mode for testing.
    async fn make_test_endpoint() -> u64 {
        let result = create_endpoint(
            mock_handle(),
            Some(CreateEndpointArgs {
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
            }),
        )
        .await
        .expect("create_endpoint should succeed");

        assert!(!result.node_id.is_empty(), "node_id should be non-empty");
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
        close_endpoint(handle, None)
            .await
            .expect("close should succeed");

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
        assert!(
            msg.contains("INVALID_HANDLE"),
            "error should mention INVALID_HANDLE: {msg}"
        );
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
    async fn test_discovery_info() {
        let handle = make_test_endpoint().await;
        let info = discovery_info(handle);
        assert!(info.is_ok(), "discovery_info should succeed");
        let info = info.unwrap();
        assert!(
            !info.node_id.is_empty(),
            "discovery_info nodeId should be non-empty"
        );
        // directAddress/relayUrl may be null on a loopback-only test host; the
        // command must still resolve without error.
        close_endpoint(handle, None).await.ok();
    }

    #[tokio::test]
    async fn test_discovery_info_invalid_handle() {
        let err = discovery_info(99999);
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
        assert!(
            result.is_ok(),
            "cancel_request should not error on unknown handle"
        );
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
        // Create an endpoint with a caller-supplied key so we own the secret
        // directly — the endpoint never exports it (keys stay native-held).
        let sk_bytes = B64
            .decode(generate_secret_key().expect("generate key"))
            .expect("valid base64");
        let sk_b64 = B64.encode(&sk_bytes);

        let info = create_endpoint(
            mock_handle(),
            Some(CreateEndpointArgs {
                key: Some(sk_b64.clone()),
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
            }),
        )
        .await
        .expect("create_endpoint");

        // Secret key is the one we supplied at creation (32 bytes).
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
        close_endpoint(handle, Some(true))
            .await
            .expect("force close should succeed");
        // Note: we do NOT assert ping(handle) fails here because the global
        // slab reuses freed indices — a concurrent test may have already
        // inserted a new endpoint at the same slot.
    }
}

/// Platform dependency contracts that cannot be exercised by the host test
/// target but are required by the plugin's declared mobile minimum versions.
mod platform_dependency_contract {
    #[test]
    fn android_api21_does_not_link_if_addrs_getifaddrs() {
        let manifest: toml::Value = include_str!("../Cargo.toml")
            .parse()
            .expect("Tauri plugin Cargo.toml must parse");
        let targets = manifest
            .get("target")
            .and_then(toml::Value::as_table)
            .expect("Cargo.toml must contain target dependencies");

        for (selector, config) in targets {
            let dependencies = config.get("dependencies").and_then(toml::Value::as_table);
            if selector.contains("target_os = \"android\"")
                && dependencies.is_some_and(|deps| deps.contains_key("if-addrs"))
            {
                panic!(
                    "Android minSdk 21–23 cannot link if-addrs 0.15: it calls getifaddrs, which Android introduced in API 24 ({selector})"
                );
            }
        }
    }
}

/// FFI string-parity contract tests for the mobile discovery boundary.
///
/// The mobile mDNS/DNS-SD commands cross a Rust ↔ Swift/Kotlin boundary that is
/// matched **by string at runtime**: the Rust plugin calls
/// `run_mobile_plugin_async("browse_start", …)`, which dispatches to the native
/// handler declared as `@objc public func browse_start` (Swift) and
/// `@Command fun browse_start` (Kotlin). Each side compiles independently,
/// so a rename or a dropped handler on one side is a *silent* contract drift
/// that only surfaces as an `unknown method` runtime error during manual
/// on-device testing (exactly the failure PR #330 risked introducing).
///
/// `mobile_mdns.rs` is `#[cfg(mobile)]`, so it is not compiled on the host and
/// its symbols cannot be reflected on here. Instead these tests scan the three
/// source files as text and assert the command-string sets are identical in
/// both directions — a deterministic, millisecond check that runs in the
/// existing host-side `cargo test` and would have caught the #330 rename with
/// certainty. See issue #333.
mod ffi_contract {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    /// Absolute path to a repo file, resolved from this crate's manifest dir so
    /// the test is independent of the process working directory.
    fn crate_file(rel: &str) -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push(rel);
        p
    }

    fn read(rel: &str) -> String {
        let path = crate_file(rel);
        fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
    }

    /// Extract the identifier that follows `marker` in `haystack`, i.e. the run
    /// of `[A-Za-z0-9_]` characters starting at the first non-identifier byte
    /// after each occurrence of `marker`. Used to pull the command name out of
    /// `func NAME(`, `run_mobile_plugin_async("NAME"`, etc.
    fn names_after(haystack: &str, marker: &str) -> BTreeSet<String> {
        let bytes = haystack.as_bytes();
        let mut out = BTreeSet::new();
        let mut search_from = 0;
        while let Some(rel) = haystack[search_from..].find(marker) {
            let mut i = search_from + rel + marker.len();
            // Skip separators (whitespace, quotes, generics like `::<()>`)
            // between the marker and the identifier.
            while i < bytes.len() && !is_ident(bytes[i]) {
                i += 1;
            }
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            if i > start {
                out.insert(haystack[start..i].to_string());
            }
            search_from = start.max(search_from + rel + marker.len());
        }
        out
    }

    fn is_ident(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    /// Command strings the Rust plugin invokes via Tauri's synchronous or
    /// asynchronous mobile bridge.
    ///
    /// Matches `run_mobile_plugin[_async]` optionally followed by a turbofish
    /// (`::<()>`) and `(`, then the first string-literal argument.
    fn rust_commands() -> BTreeSet<String> {
        let src = read("src/mobile_mdns.rs");
        let mut out = BTreeSet::new();
        // Scan the longer marker first. The shorter marker also occurs inside
        // `_async`, but its first-argument guard intentionally rejects that
        // duplicate because `_async` contains identifier characters.
        for marker in ["run_mobile_plugin_async", "run_mobile_plugin"] {
            for (idx, _) in src.match_indices(marker) {
                // Find the opening quote of the first argument, skipping an optional
                // turbofish and the opening paren / whitespace / newlines.
                let after = &src[idx + marker.len()..];
                let Some(q) = after.find('"') else { continue };
                // Guard: the string literal must be the *first* argument, i.e. only
                // separators / turbofish / `(` may appear before the quote.
                if after[..q]
                    .chars()
                    .any(|c| c.is_ascii_alphanumeric() && c != '(')
                {
                    // Defensive: an alnum before the quote would mean we matched a
                    // different construct. `::<()>` contains no alnum, `(` is fine.
                    continue;
                }
                let name: String = after[q + 1..].chars().take_while(|&c| c != '"').collect();
                if !name.is_empty() {
                    out.insert(name);
                }
            }
        }
        out
    }

    /// Handlers exposed by the Swift plugin: `@objc public func NAME(`.
    fn swift_commands() -> BTreeSet<String> {
        let src = read("ios/Sources/IrohHttpPlugin.swift");
        names_after(&src, "@objc public func")
    }

    /// Handlers exposed by the Kotlin plugin: `@Command` on the line(s)
    /// preceding a `fun NAME(`. `@Command` and `fun` may be separated by
    /// whitespace / newlines (annotation on its own line).
    fn kotlin_commands() -> BTreeSet<String> {
        let src = read("android/src/main/java/com/iroh/http/IrohHttpPlugin.kt");
        let bytes = src.as_bytes();
        let mut out = BTreeSet::new();
        for (idx, _) in src.match_indices("@Command") {
            let rest = &src[idx + "@Command".len()..];
            let Some(fpos) = rest.find("fun ") else {
                continue;
            };
            // Only accept `fun` that belongs to this annotation: reject if
            // another `@Command` appears between the annotation and the `fun`.
            if rest[..fpos].contains("@Command") {
                continue;
            }
            let mut i = idx + "@Command".len() + fpos + "fun ".len();
            while i < bytes.len() && !is_ident(bytes[i]) {
                i += 1;
            }
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            if i > start {
                out.insert(src[start..i].to_string());
            }
        }
        out
    }

    /// Human-readable diff between two command sets.
    fn diff(a: &BTreeSet<String>, b: &BTreeSet<String>) -> String {
        let only_a: Vec<_> = a.difference(b).cloned().collect();
        let only_b: Vec<_> = b.difference(a).cloned().collect();
        format!("only in first: {only_a:?}; only in second: {only_b:?}")
    }

    #[test]
    fn rust_and_swift_commands_match() {
        let rust = rust_commands();
        let swift = swift_commands();
        assert!(
            !rust.is_empty(),
            "no run_mobile_plugin[_async] command strings found in src/mobile_mdns.rs \
             — the scanner is broken, not the contract"
        );
        assert!(
            !swift.is_empty(),
            "no @objc handlers found in ios/Sources/IrohHttpPlugin.swift \
             — the scanner is broken, not the contract"
        );
        assert_eq!(
            rust,
            swift,
            "Rust ↔ Swift mobile command contract drift ({}). Every \
             run_mobile_plugin[_async](\"…\") string must have a matching \
             `@objc public func` in ios/Sources/IrohHttpPlugin.swift, and \
             vice-versa. See issue #333.",
            diff(&rust, &swift)
        );
    }

    #[test]
    fn rust_and_kotlin_commands_match() {
        let rust = rust_commands();
        let kotlin = kotlin_commands();
        assert!(
            !rust.is_empty(),
            "no run_mobile_plugin[_async] command strings found in src/mobile_mdns.rs \
             — the scanner is broken, not the contract"
        );
        assert!(
            !kotlin.is_empty(),
            "no @Command handlers found in IrohHttpPlugin.kt \
             — the scanner is broken, not the contract"
        );
        assert_eq!(
            rust,
            kotlin,
            "Rust ↔ Kotlin mobile command contract drift ({}). Every \
             run_mobile_plugin[_async](\"…\") string must have a matching \
             `@Command fun` in android/.../IrohHttpPlugin.kt, and vice-versa. \
             See issue #333.",
            diff(&rust, &kotlin)
        );
    }

    #[test]
    fn swift_and_kotlin_commands_match() {
        // Transitively implied by the two tests above, but asserted directly so
        // a native-only drift (Swift vs Kotlin) reports against the right pair.
        let swift = swift_commands();
        let kotlin = kotlin_commands();
        assert_eq!(
            swift,
            kotlin,
            "Swift ↔ Kotlin mobile command contract drift ({}). The two native \
             plugins must expose the same handler set. See issue #333.",
            diff(&swift, &kotlin)
        );
    }
}
