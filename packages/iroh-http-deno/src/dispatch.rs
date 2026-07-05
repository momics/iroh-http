//! JSON-over-FFI dispatch.  Translates `(method, payload)` pairs into calls on
//! `iroh-http-core` and returns a JSON-encoded `{"ok": T} | {"err": string}`.
//!
//! ## Why this file is large
//!
//! Deno FFI (`Deno.dlopen`) exposes a single C-ABI symbol (`iroh_http_call`).
//! Every bridge method arrives as a UTF-8 method name + JSON payload, and the
//! dispatch table below routes each to the appropriate `iroh-http-core` call.
//! Unlike Node.js (napi-rs macros) or Tauri (typed commands), Deno has no
//! code-generation layer that can auto-produce bindings — the match arms,
//! JSON deserialization, and response serialization are all hand-maintained.
//!
//! The file is organised into logical sections (endpoint lifecycle, streaming,
//! fetch/serve, keys, mDNS, sessions).  Each section is a thin shim that
//! deserializes JSON, calls core, and re-serializes the result.  Endpoint
//! slab management is centralised in `iroh_http_core::registry` (A-ISS-041).
//!
//! If a generated binding approach becomes viable for Deno FFI, this file
//! should be replaced.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use bytes::Bytes;
use iroh_http_core::{
    endpoint::{IrohEndpoint, NodeOptions},
    parse_direct_addrs, registry, respond, ConnectionEvent, DiscoveryOptions, NetworkingOptions,
    PoolOptions, RequestPayload, StreamingOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::serve_registry;
#[cfg(feature = "discovery")]
use iroh_http_discovery as _;
#[cfg(feature = "discovery")]
use slab::Slab;
#[cfg(feature = "discovery")]
use std::sync::Arc;
#[cfg(feature = "discovery")]
use std::sync::{Mutex, OnceLock};
#[cfg(feature = "discovery")]
use tokio::sync::Mutex as TokioMutex;

struct PathSub {
    rx: tokio::sync::Mutex<
        tokio::sync::mpsc::UnboundedReceiver<iroh_http_core::endpoint::PathInfo>,
    >,
    notify: tokio::sync::Notify,
}

type PathRxMap = dashmap::DashMap<(u64, String), std::sync::Arc<PathSub>>;

fn path_change_rxs() -> &'static PathRxMap {
    static PATH_CHANGE_RXS: std::sync::OnceLock<PathRxMap> = std::sync::OnceLock::new();
    PATH_CHANGE_RXS.get_or_init(dashmap::DashMap::new)
}

// ── Endpoint helpers ─────────────────────────────────────────────────────────

fn get_endpoint(handle: u64) -> Option<IrohEndpoint> {
    registry::get_endpoint(handle)
}

fn remove_endpoint(handle: u64) -> Option<IrohEndpoint> {
    registry::remove_endpoint(handle)
}

fn insert_endpoint(ep: IrohEndpoint) -> u64 {
    registry::insert_endpoint(ep)
}

use iroh_http_adapter::{
    core_error_to_json, format_error_json, safe_f64_to_u64, safe_f64_to_usize,
    send_undeliverable_rejection, validate_header_rows, AdapterInputError, MAX_BODY_BYTES,
    MAX_TIMEOUT_MS,
};

// ── Helper ────────────────────────────────────────────────────────────────────

fn ok(v: impl Serialize) -> Value {
    json!({ "ok": v })
}

fn err(s: impl std::fmt::Display) -> Value {
    json!({ "err": format_error_json("UNKNOWN", s) })
}

fn err_code(code: &str, s: impl std::fmt::Display) -> Value {
    json!({ "err": format_error_json(code, s) })
}

fn err_core(e: iroh_http_core::CoreError) -> Value {
    json!({ "err": core_error_to_json(&e) })
}

fn err_adapter(e: AdapterInputError) -> Value {
    err_code(e.code(), e.message())
}

fn deliver_request_to_queue(handle: u64, ep: &IrohEndpoint, payload: RequestPayload) {
    // #122: Look up the CURRENT queue from the registry instead of a captured
    // Arc.  When serve is restarted (stopServe → serveStart), old QUIC
    // connections may still be alive (connection pool reuse). Requests on
    // those old connections must be delivered to the NEW queue, not the old
    // (removed) one.
    let req_handle = payload.req_handle;
    let q = match serve_registry::get(handle) {
        Some(q) => q,
        None => {
            tracing::warn!("iroh-http-deno: serve registry miss — responding 503");
            let _ = send_undeliverable_rejection(ep.handles(), req_handle, "deno registry miss");
            return;
        }
    };
    // #149: Reject immediately if this queue belongs to a stopped serve cycle.
    // The queue stays in the registry between stopServe and the next serveStart
    // so the request never hangs until the tower timeout.
    if *q.shutdown_rx.borrow() {
        tracing::warn!("iroh-http-deno: serve shutdown — responding 503");
        let _ = send_undeliverable_rejection(ep.handles(), req_handle, "deno serve shutdown");
        return;
    }
    let headers: Vec<Vec<String>> = payload
        .headers
        .into_iter()
        .map(|(k, v)| vec![k, v])
        .collect();
    let event = serde_json::json!({
        "reqHandle":         payload.req_handle,
        "reqBodyHandle":     payload.req_body_handle,
        "resBodyHandle":     payload.res_body_handle,
        "isBidi":            payload.is_bidi,
        "method":            payload.method,
        "url":               payload.url,
        "headers":           headers,
        "remoteNodeId":      payload.remote_node_id,
    });
    if q.tx.try_send(event).is_err() {
        tracing::warn!("iroh-http-deno: serve queue full — dropping request with 503");
        let _ = send_undeliverable_rejection(ep.handles(), req_handle, "deno serve queue full");
    }
}

/// Extract and look up the endpoint from a JSON payload's `endpointHandle`.
fn require_endpoint(p: &Value) -> Result<IrohEndpoint, Value> {
    let handle = p["endpointHandle"]
        .as_u64()
        .ok_or_else(|| err("missing endpointHandle"))?;
    get_endpoint(handle).ok_or_else(|| {
        err_code(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {handle})"),
        )
    })
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

/// Expands one arm of the dispatch table.
///
/// - `async` — calls `handler(p).await`
/// - `sync`  — calls `handler(p)`
/// - `sync0` — calls `handler()` (no payload argument; e.g. `generateSecretKey`)
macro_rules! dispatch_arm {
    (async, $handler:path, $p:expr) => {
        $handler($p).await
    };
    (sync,  $handler:path, $p:expr) => {
        $handler($p)
    };
    (sync0, $handler:path, $_p:expr) => {
        $handler()
    };
}

/// Generates the dispatch `match` from a compact method registry.  Adding a
/// new method is one line: `async "methodName" => handler_fn`.
macro_rules! dispatch_table {
    ($method:expr, $p:expr; $( $kind:ident $name:literal => $handler:path ),+ $(,)?) => {
        match $method {
            $( $name => dispatch_arm!($kind, $handler, $p), )+
            other => err(format!("unknown method: {other}")),
        }
    };
}

/// Entry point called from `lib.rs`.  Parses the JSON payload and routes to the
/// appropriate handler.
pub async fn dispatch(method: &str, payload: &[u8]) -> Value {
    let p: Value = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => return err(format!("invalid JSON payload: {e}")),
    };

    dispatch_table!(method, p;
        // ── Endpoint lifecycle ───────────────────────────────────────────────
        async "createEndpoint"          => create_endpoint,
        async "closeEndpoint"           => close_endpoint,
        sync  "nodeAddr"                => node_addr_dispatch,
        sync  "nodeTicket"              => node_ticket_dispatch,
        sync  "homeRelay"               => home_relay_dispatch,
        async "peerInfo"                => peer_info_dispatch,
        async "peerStats"               => peer_stats_dispatch,
        sync  "endpointStats"           => endpoint_stats_dispatch,
        // ── Handle allocation ────────────────────────────────────────────────
        sync  "allocBodyWriter"         => alloc_body_writer_dispatch,
        sync  "allocFetchToken"         => alloc_fetch_token_dispatch,
        sync  "cancelInFlight"          => cancel_in_flight_dispatch,
        // ── Streaming (JSON fallbacks — hot path uses raw FFI symbols) ───────
        async "nextChunk"               => next_chunk_dispatch,
        async "sendChunk"               => send_chunk_dispatch,
        sync  "finishBody"              => finish_body_dispatch,
        sync  "cancelRequest"           => cancel_request_dispatch,
        // ── HTTP ─────────────────────────────────────────────────────────────
        async "rawFetch"                => raw_fetch,
        // ── Serve loop ───────────────────────────────────────────────────────
        async "serveStart"              => serve_start,
        async "stopServe"               => stop_serve,
        async "waitEndpointClosed"      => wait_endpoint_closed,
        async "nextRequest"             => next_request,
        async "nextConnectionEvent"     => next_connection_event,
        sync  "respond"                 => respond_dispatch,
        // ── Crypto ───────────────────────────────────────────────────────────
        sync  "secretKeySign"           => secret_key_sign_dispatch,
        sync  "publicKeyVerify"         => public_key_verify_dispatch,
        sync0 "generateSecretKey"       => generate_secret_key_dispatch,
        // ── mDNS ─────────────────────────────────────────────────────────────
        async "mdnsBrowse"              => mdns_browse_dispatch,
        async "mdnsNextEvent"           => mdns_next_event_dispatch,
        sync  "mdnsBrowseClose"         => mdns_browse_close_dispatch,
        sync  "mdnsAdvertise"           => mdns_advertise_dispatch,
        sync  "mdnsAdvertiseClose"      => mdns_advertise_close_dispatch,
        // ── Sessions ─────────────────────────────────────────────────────────
        async "sessionAccept"           => session_accept_dispatch,
        async "sessionConnect"          => session_connect_dispatch,
        async "sessionCreateBidiStream" => session_create_bidi_stream_dispatch,
        async "sessionNextBidiStream"   => session_next_bidi_stream_dispatch,
        sync  "sessionClose"            => session_close_dispatch,
        async "sessionClosed"           => session_closed_dispatch,
        async "sessionCreateUniStream"  => session_create_uni_stream_dispatch,
        async "sessionNextUniStream"    => session_next_uni_stream_dispatch,
        sync  "sessionSendDatagram"     => session_send_datagram_dispatch,
        async "sessionRecvDatagram"     => session_recv_datagram_dispatch,
        sync  "sessionMaxDatagramSize"  => session_max_datagram_size_dispatch,
        // ── Transport events ──────────────────────────────────────────────────
        async "startTransportEvents"    => start_transport_events_dispatch,
        async "nextTransportEvent"      => next_transport_event_dispatch,
        async "nextPathChange"          => next_path_change_dispatch,
        sync  "unsubscribePathChanges"  => unsubscribe_path_changes_dispatch,
    )
}

// ── Endpoint ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateEndpointPayload {
    key: Option<String>,
    idle_timeout: Option<u64>,
    relay_mode: Option<String>,
    relays: Option<Vec<String>>,
    bind_addrs: Option<Vec<String>>,
    dns_discovery: Option<String>,
    dns_discovery_enabled: Option<bool>,
    channel_capacity: Option<usize>,
    max_chunk_size_bytes: Option<usize>,
    handle_ttl: Option<u64>,
    sweep_interval: Option<u64>,
    max_pooled_connections: Option<usize>,
    pool_idle_timeout_ms: Option<u64>,
    disable_networking: Option<bool>,
    proxy_url: Option<String>,
    proxy_from_env: Option<bool>,
    keylog: Option<bool>,
    compression_level: Option<i32>,
    compression_min_body_bytes: Option<usize>,
    max_header_bytes: Option<usize>,
}

async fn create_endpoint(p: Value) -> Value {
    let args: CreateEndpointPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };

    let key: Option<[u8; 32]> = match args.key {
        None => None,
        Some(k) => {
            let decoded = match B64.decode(&k) {
                Ok(d) => d,
                Err(_) => return err("secret key: invalid base64"),
            };
            match <[u8; 32]>::try_from(decoded) {
                Ok(arr) => Some(arr),
                Err(v) => {
                    return err(format!(
                        "secret key must be exactly 32 bytes, got {}",
                        v.len()
                    ))
                }
            }
        }
    };

    let opts = NodeOptions {
        key,
        networking: NetworkingOptions {
            relay_mode: args.relay_mode,
            relays: args.relays.unwrap_or_default(),
            bind_addrs: args.bind_addrs.unwrap_or_default(),
            idle_timeout_ms: args.idle_timeout,
            proxy_url: args.proxy_url,
            proxy_from_env: args.proxy_from_env.unwrap_or(false),
            disabled: args.disable_networking.unwrap_or(false),
        },
        discovery: DiscoveryOptions {
            dns_server: args.dns_discovery,
            enabled: args.dns_discovery_enabled.unwrap_or(true),
        },
        pool: PoolOptions {
            max_connections: args.max_pooled_connections,
            idle_timeout_ms: args.pool_idle_timeout_ms,
        },
        streaming: StreamingOptions {
            channel_capacity: args.channel_capacity,
            max_chunk_size_bytes: args.max_chunk_size_bytes,
            drain_timeout_ms: None,
            handle_ttl_ms: args.handle_ttl,
            sweep_interval_ms: args.sweep_interval,
        },
        capabilities: Vec::new(),
        keylog: args.keylog.unwrap_or(false),
        max_header_size: args.max_header_bytes,
        max_response_body_bytes: None,
        compression: if args.compression_min_body_bytes.is_some()
            || args.compression_level.is_some()
        {
            // ISS-020: validate compression level range before cast.
            if let Some(level) = args.compression_level {
                if level < 0 {
                    return err(format!(
                        "compressionLevel must be non-negative, got {level}"
                    ));
                }
            }
            Some(iroh_http_core::CompressionOptions {
                min_body_bytes: args
                    .compression_min_body_bytes
                    .unwrap_or(iroh_http_core::CompressionOptions::DEFAULT_MIN_BODY_BYTES),
                level: args.compression_level.map(|v| v as u32),
            })
        } else {
            None
        },
    };
    match IrohEndpoint::bind(opts).await {
        Err(e) => err_core(e),
        Ok(ep) => {
            let node_id = ep.node_id().to_string();
            let handle = insert_endpoint(ep);
            ok(json!({ "endpointHandle": handle, "nodeId": node_id }))
        }
    }
}

async fn close_endpoint(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    let force = p["force"].as_bool().unwrap_or(false);
    serve_registry::remove(handle);
    match get_endpoint(handle) {
        None => err_code(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {handle})"),
        ),
        Some(ep) => {
            if force {
                ep.close_force().await;
            } else {
                ep.close().await;
            }
            // Remove from registry only after close completes.
            remove_endpoint(handle);
            ok(json!({}))
        }
    }
}

// ── Address introspection ─────────────────────────────────────────────────────

fn node_addr_dispatch(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    match get_endpoint(handle) {
        None => err_code(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {handle})"),
        ),
        Some(ep) => {
            let info = ep.node_addr();
            ok(json!({ "id": info.id, "addrs": info.addrs }))
        }
    }
}

fn node_ticket_dispatch(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    match get_endpoint(handle) {
        None => err_code(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {handle})"),
        ),
        Some(ep) => match iroh_http_core::node_ticket(&ep) {
            Ok(ticket) => ok(ticket),
            Err(e) => err_core(e),
        },
    }
}

fn home_relay_dispatch(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    match get_endpoint(handle) {
        None => err_code(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {handle})"),
        ),
        Some(ep) => ok(ep.home_relay()),
    }
}

async fn peer_info_dispatch(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    let node_id = match p["nodeId"].as_str() {
        Some(s) => s,
        None => return err("missing nodeId"),
    };
    match get_endpoint(handle) {
        None => err_code(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {handle})"),
        ),
        Some(ep) => ok(ep
            .peer_info(node_id)
            .await
            .map(|info| json!({ "id": info.id, "addrs": info.addrs }))),
    }
}

async fn peer_stats_dispatch(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    let node_id = match p["nodeId"].as_str() {
        Some(s) => s,
        None => return err("missing nodeId"),
    };
    match get_endpoint(handle) {
        None => err_code(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {handle})"),
        ),
        Some(ep) => ok(ep.peer_stats(node_id).await),
    }
}

fn endpoint_stats_dispatch(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    match get_endpoint(handle) {
        None => err_code(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {handle})"),
        ),
        Some(ep) => ok(ep.endpoint_stats()),
    }
}

// ── Body writer allocation ────────────────────────────────────────────────────

fn alloc_body_writer_dispatch(p: Value) -> Value {
    let ep = match require_endpoint(&p) {
        Ok(ep) => ep,
        Err(e) => return e,
    };
    let (handle, reader) = match ep.handles().alloc_body_writer() {
        Ok(v) => v,
        Err(e) => return err_core(e),
    };
    ep.handles().store_pending_reader(handle, reader);
    ok(json!({ "handle": handle }))
}

fn alloc_fetch_token_dispatch(p: Value) -> Value {
    let ep = match require_endpoint(&p) {
        Ok(ep) => ep,
        Err(e) => return e,
    };
    match ep.handles().alloc_fetch_token() {
        Ok(token) => ok(json!({ "token": token })),
        Err(e) => err_core(e),
    }
}

fn cancel_in_flight_dispatch(p: Value) -> Value {
    let ep = match require_endpoint(&p) {
        Ok(ep) => ep,
        Err(e) => return e,
    };
    let token = match p["token"].as_u64() {
        Some(t) => t,
        None => return err("missing token"),
    };
    ep.handles().cancel_in_flight(token);
    ok(json!({}))
}

// ── Streaming bridge ──────────────────────────────────────────────────────────

async fn next_chunk_dispatch(p: Value) -> Value {
    let ep = match require_endpoint(&p) {
        Ok(ep) => ep,
        Err(e) => return e,
    };
    let handle = match p["handle"].as_u64() {
        Some(h) => h,
        None => return err("missing handle"),
    };
    match ep.handles().next_chunk(handle).await {
        Err(e) => err_core(e),
        Ok(None) => ok(json!({ "chunk": null })),
        Ok(Some(b)) => ok(json!({ "chunk": B64.encode(&b[..]) })),
    }
}

async fn send_chunk_dispatch(p: Value) -> Value {
    let ep = match require_endpoint(&p) {
        Ok(ep) => ep,
        Err(e) => return e,
    };
    let handle = match p["handle"].as_u64() {
        Some(h) => h,
        None => return err("missing handle"),
    };
    let b64: String = match serde_json::from_value(p["chunk"].clone()) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let bytes = match B64.decode(&b64) {
        Ok(b) => Bytes::from(b),
        Err(e) => return err(format!("base64 decode: {e}")),
    };
    match ep.handles().send_chunk(handle, bytes).await {
        Ok(()) => ok(json!({})),
        Err(e) => err_core(e),
    }
}

fn finish_body_dispatch(p: Value) -> Value {
    let ep = match require_endpoint(&p) {
        Ok(ep) => ep,
        Err(e) => return e,
    };
    let handle = match p["handle"].as_u64() {
        Some(h) => h,
        None => return err("missing handle"),
    };
    match ep.handles().finish_body(handle) {
        Ok(()) => ok(json!({})),
        Err(e) => err_core(e),
    }
}

fn cancel_request_dispatch(p: Value) -> Value {
    let ep = match require_endpoint(&p) {
        Ok(ep) => ep,
        Err(e) => return e,
    };
    let handle = match p["handle"].as_u64() {
        Some(h) => h,
        None => return err("missing handle"),
    };
    ep.handles().cancel_reader(handle);
    ok(json!({}))
}

// ── rawFetch ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFetchPayload {
    endpoint_handle: u64,
    node_id: String,
    url: String,
    method: String,
    headers: Vec<Vec<String>>,
    req_body_handle: Option<u64>,
    fetch_token: Option<u64>,
    direct_addrs: Option<Vec<String>>,
    #[serde(alias = "timeout_ms")]
    timeout_ms: Option<f64>,
    decompress: Option<bool>,
    #[serde(alias = "max_response_body_bytes")]
    max_response_body_bytes: Option<f64>,
}

async fn raw_fetch(p: Value) -> Value {
    let args: RawFetchPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    raw_fetch_impl(args).await
}

/// Warm fetch hot path (#286): parse the original request slice straight into
/// `RawFetchPayload`, skipping the `Value` round trip the generic dispatch
/// table performs (`from_slice` → `Value` → `to_vec` → `from_slice` → `Value`
/// → `from_value`). Callers get the same `err` envelope on malformed JSON as
/// `dispatch` would return.
pub async fn raw_fetch_from_slice(payload: &[u8]) -> Value {
    let args: RawFetchPayload = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => return err(format!("invalid JSON payload: {e}")),
    };
    raw_fetch_impl(args).await
}

async fn raw_fetch_impl(args: RawFetchPayload) -> Value {
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let pairs = match validate_header_rows(args.headers) {
        Ok(pairs) => pairs,
        Err(e) => return err_adapter(e),
    };
    let reader = args
        .req_body_handle
        .and_then(|h| ep.handles().claim_pending_reader(h));
    let addrs = match parse_direct_addrs(&args.direct_addrs) {
        Ok(a) => a,
        Err(e) => return err(e),
    };

    let timeout = match args
        .timeout_ms
        .map(|ms| safe_f64_to_u64(ms, "timeoutMs", MAX_TIMEOUT_MS))
        .transpose()
    {
        Ok(timeout) => timeout.map(std::time::Duration::from_millis),
        Err(e) => return err_adapter(e),
    };
    let max_response_body_bytes = match args
        .max_response_body_bytes
        .map(|b| safe_f64_to_usize(b, "maxResponseBodyBytes", MAX_BODY_BYTES))
        .transpose()
    {
        Ok(bytes) => bytes,
        Err(e) => return err_adapter(e),
    };

    // #126: If the caller didn't supply a fetch token, allocate one
    // internally so the JS side doesn't need a separate FFI round-trip.
    let fetch_token = match args.fetch_token {
        Some(t) => Some(t),
        None => ep.handles().alloc_fetch_token().ok(),
    };

    match iroh_http_core::fetch(
        &ep,
        &args.node_id,
        &args.url,
        &args.method,
        &pairs,
        reader,
        fetch_token,
        addrs.as_deref(),
        timeout,
        args.decompress.unwrap_or(true),
        max_response_body_bytes,
    )
    .await
    {
        Err(e) => err_core(e),
        Ok(res) => {
            let headers: Vec<Vec<String>> =
                res.headers.into_iter().map(|(k, v)| vec![k, v]).collect();
            ok(json!({
                "status": res.status,
                "headers": headers,
                "bodyHandle": res.body_handle,
                "url": res.url,
            }))
        }
    }
}

// ── serve ─────────────────────────────────────────────────────────────────────

async fn serve_start(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    let ep = match get_endpoint(handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {handle})"),
            )
        }
    };

    // Parse optional serve options from payload.
    let serve_opts = iroh_http_core::ServeOptions {
        max_concurrency: p["maxConcurrency"].as_u64().map(|v| v as usize),
        max_connections_per_peer: p["maxConnectionsPerPeer"].as_u64().map(|v| v as usize),
        request_timeout_ms: p["requestTimeout"].as_u64(),
        max_request_body_wire_bytes: p["maxRequestBodyWireBytes"].as_u64().map(|v| v as usize),
        max_request_body_decoded_bytes: p["maxRequestBodyDecodedBytes"]
            .as_u64()
            .map(|v| v as usize),
        max_total_connections: p["maxTotalConnections"].as_u64().map(|v| v as usize),
        max_serve_errors: p["maxServeErrors"].as_u64().map(|v| v as usize),
        drain_timeout_ms: p["drainTimeout"].as_u64(),
        load_shed: p["loadShed"].as_bool(),
        decompression: p["decompress"].as_bool(),
    };

    let queue = serve_registry::register(handle);

    let ep_clone = ep.clone();
    let conn_tx = queue.conn_tx.clone();
    let conn_event_fn: Option<std::sync::Arc<dyn Fn(ConnectionEvent) + Send + Sync>> =
        Some(std::sync::Arc::new(move |ev: ConnectionEvent| {
            let _ = conn_tx.try_send(serde_json::json!({
                "peerId": ev.peer_id,
                "connected": ev.connected,
            }));
        }));
    let serve_handle = iroh_http_core::ffi_serve_with_callback(
        ep.clone(),
        serve_opts,
        move |payload: RequestPayload| {
            deliver_request_to_queue(handle, &ep_clone, payload);
        },
        conn_event_fn,
    );
    ep.set_serve_handle(serve_handle);

    ok(json!({}))
}

async fn stop_serve(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    let ep = match get_endpoint(handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {handle})"),
            )
        }
    };
    ep.stop_serve();
    // Signal shutdown immediately so the JS polling loop stops dequeuing.
    // The watch channel persists its value — any in-flight or future
    // `try_next_request` call sees shutdown and returns -1.
    serve_registry::signal_shutdown(handle);
    // Drain any queued-but-undelivered requests and respond 503 so the
    // core handler tasks wake up and decrement in-flight immediately,
    // instead of stalling until the 60s tower timeout fires.
    if let Some(queue) = serve_registry::get(handle) {
        if let Ok(mut rx) = queue.rx.try_lock() {
            while let Ok(payload) = rx.try_recv() {
                if let Some(h) = payload.get("reqHandle").and_then(|v| v.as_u64()) {
                    let _ = send_undeliverable_rejection(ep.handles(), h, "deno stopServe drain");
                }
            }
        }
    }
    // The registry entry is intentionally kept alive here.  Removing it
    // would cause a race: lingering connection-handler tasks spawned by
    // `serve_with_events` can still fire `on_request` after the serve
    // loop exits (they are detached tokio::spawn tasks).  If the entry
    // were absent, `serve_registry::get` would return `None` → 503,
    // even when a new serve cycle is about to register a replacement
    // queue.  Keeping the shutdown-flagged entry lets the `on_request`
    // callback reject promptly without a registry miss.  The entry is
    // replaced by the next `serve_start` or removed by `close_endpoint`.
    //
    // (Supersedes DENO-002 — the JS polling loop now stops via the
    // shutdown watch channel, not channel disconnection.)
    //
    // #122: wait for the previous serve loop to fully terminate before returning.
    // Without this, a subsequent `serve_start` on the same endpoint would
    // overwrite `serve_handle` while the old loop is still draining; new
    // incoming requests would be routed through the old `on_request` closure
    // (capturing the old queue's `tx`) and never delivered to the new
    // `nextRequest` poller — they would just time out at 60s.
    ep.wait_serve_stop().await;
    ok(json!({}))
}

async fn wait_endpoint_closed(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    let ep = match get_endpoint(handle) {
        Some(e) => e,
        None => return ok(json!({})), // already removed — treat as closed
    };
    ep.wait_closed().await;
    ok(json!({}))
}

async fn next_request(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    let queue = match serve_registry::get(handle) {
        Some(q) => q,
        // Queue was already removed (stopServe completed) — signal end-of-stream.
        None => return ok(Value::Null),
    };
    // Clone the receiver so we can watch for shutdown without moving it.
    // `wait_for(|v| *v)` completes immediately if shutdown was already triggered
    // (watch persists its last value), or waits until it becomes true — both paths
    // unblock any pending recv() call (issue-12 fix).
    let mut shutdown_rx = queue.shutdown_rx.clone();
    let item = tokio::select! {
        biased;
        _ = shutdown_rx.wait_for(|v| *v) => None,
        item = async { queue.rx.lock().await.recv().await } => item,
    };
    ok(item)
}

/// Poll the next peer connection event (connect or disconnect) for an endpoint.
///
/// Returns `{"ok": {"peerId": "...", "connected": bool}}` on success,
/// or `{"ok": null}` when the serve loop has stopped and no more events will arrive.
async fn next_connection_event(p: Value) -> Value {
    let handle = match p["endpointHandle"].as_u64() {
        Some(h) => h,
        None => return err("missing endpointHandle"),
    };
    let queue = match serve_registry::get(handle) {
        Some(q) => q,
        None => return ok(Value::Null),
    };
    let mut shutdown_rx = queue.shutdown_rx.clone();
    let item = tokio::select! {
        biased;
        _ = shutdown_rx.wait_for(|v| *v) => None,
        item = async { queue.conn_rx.lock().await.recv().await } => item,
    };
    ok(item)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RespondPayload {
    #[allow(dead_code)]
    endpoint_handle: u64,
    req_handle: u64,
    status: u16,
    headers: Vec<Vec<String>>,
}

fn respond_dispatch(p: Value) -> Value {
    let ep = match require_endpoint(&p) {
        Ok(ep) => ep,
        Err(e) => return e,
    };
    let args: RespondPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let headers = match validate_header_rows(args.headers) {
        Ok(headers) => headers,
        Err(e) => return err_adapter(e),
    };
    match respond(ep.handles(), args.req_handle, args.status, headers) {
        Ok(()) => ok(json!({})),
        Err(e) => err_core(e),
    }
}

// ── Key operations ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignPayload {
    secret_key: String,
    data: String,
}

fn secret_key_sign_dispatch(p: Value) -> Value {
    let args: SignPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let key_bytes: [u8; 32] = match B64.decode(&args.secret_key) {
        Ok(v) => match v.try_into() {
            Ok(a) => a,
            Err(_) => return err("secret key must be 32 bytes"),
        },
        Err(e) => return err(format!("base64 decode key: {e}")),
    };
    let data_bytes = match B64.decode(&args.data) {
        Ok(v) => v,
        Err(e) => return err(format!("base64 decode data: {e}")),
    };
    let sig = match iroh_http_core::secret_key_sign(&key_bytes, &data_bytes) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    ok(json!(B64.encode(sig)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifyPayload {
    public_key: String,
    data: String,
    signature: String,
}

fn public_key_verify_dispatch(p: Value) -> Value {
    let args: VerifyPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let key_bytes: [u8; 32] = match B64.decode(&args.public_key) {
        Ok(v) => match v.try_into() {
            Ok(a) => a,
            Err(_) => return err("public key must be 32 bytes"),
        },
        Err(e) => return err(format!("base64 decode key: {e}")),
    };
    let data_bytes = match B64.decode(&args.data) {
        Ok(v) => v,
        Err(e) => return err(format!("base64 decode data: {e}")),
    };
    let sig_bytes: [u8; 64] = match B64.decode(&args.signature) {
        Ok(v) => match v.try_into() {
            Ok(a) => a,
            Err(_) => return err("signature must be 64 bytes"),
        },
        Err(e) => return err(format!("base64 decode sig: {e}")),
    };
    ok(json!(iroh_http_core::public_key_verify(
        &key_bytes,
        &data_bytes,
        &sig_bytes
    )))
}

fn generate_secret_key_dispatch() -> Value {
    let key = match iroh_http_core::generate_secret_key() {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    ok(json!(B64.encode(key)))
}

// ── mDNS browse / advertise ──────────────────────────────────────────────────

#[cfg(feature = "discovery")]
type BrowseHandle = Arc<TokioMutex<iroh_http_discovery::BrowseSession>>;

#[cfg(feature = "discovery")]
fn browse_slab() -> &'static Mutex<Slab<BrowseHandle>> {
    static S: OnceLock<Mutex<Slab<BrowseHandle>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Slab::new()))
}

#[cfg(feature = "discovery")]
fn advertise_slab() -> &'static Mutex<Slab<iroh_http_discovery::AdvertiseSession>> {
    static S: OnceLock<Mutex<Slab<iroh_http_discovery::AdvertiseSession>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Slab::new()))
}

async fn mdns_browse_dispatch(_p: Value) -> Value {
    #[cfg(feature = "discovery")]
    {
        let handle = match _p["endpointHandle"].as_u64() {
            Some(h) => h,
            None => return err("missing endpointHandle"),
        };
        let service_name = match _p["serviceName"].as_str() {
            Some(s) => s,
            None => return err("missing serviceName"),
        };
        let ep = match get_endpoint(handle) {
            Some(ep) => ep,
            None => {
                return err_code(
                    "INVALID_HANDLE",
                    format!("node closed or not found (handle {handle})"),
                )
            }
        };
        match iroh_http_discovery::start_browse(ep.raw(), service_name).await {
            Err(e) => err_code("REFUSED", e),
            Ok(session) => {
                let h = browse_slab()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(Arc::new(TokioMutex::new(session))) as u32;
                ok(json!(h))
            }
        }
    }
    #[cfg(not(feature = "discovery"))]
    err("discovery feature not enabled in this build")
}

async fn mdns_next_event_dispatch(_p: Value) -> Value {
    #[cfg(feature = "discovery")]
    {
        let handle = match _p["browseHandle"].as_u64() {
            Some(h) => h,
            None => return err("missing browseHandle"),
        };
        let session = match browse_slab()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle as usize)
            .cloned()
        {
            Some(s) => s,
            None => return err(format!("invalid browse handle: {handle}")),
        };
        let event = session.lock().await.next_event().await;
        match event {
            None => ok(json!(null)),
            Some(ev) => ok(json!({
                "isActive": ev.is_active,
                "nodeId": ev.node_id,
                "addrs": ev.addrs,
            })),
        }
    }
    #[cfg(not(feature = "discovery"))]
    err("discovery feature not enabled in this build")
}

fn mdns_browse_close_dispatch(_p: Value) -> Value {
    #[cfg(feature = "discovery")]
    {
        let handle = match _p["browseHandle"].as_u64() {
            Some(h) => h,
            None => return err("missing browseHandle"),
        };
        let mut slab = browse_slab().lock().unwrap_or_else(|e| e.into_inner());
        if slab.contains(handle as usize) {
            slab.remove(handle as usize);
        }
    }
    ok(json!({}))
}

fn mdns_advertise_dispatch(_p: Value) -> Value {
    #[cfg(feature = "discovery")]
    {
        let handle = match _p["endpointHandle"].as_u64() {
            Some(h) => h,
            None => return err("missing endpointHandle"),
        };
        let service_name = match _p["serviceName"].as_str() {
            Some(s) => s,
            None => return err("missing serviceName"),
        };
        let ep = match get_endpoint(handle) {
            Some(ep) => ep,
            None => {
                return err_code(
                    "INVALID_HANDLE",
                    format!("node closed or not found (handle {handle})"),
                )
            }
        };
        match iroh_http_discovery::start_advertise(ep.raw(), service_name) {
            Err(e) => err_code("REFUSED", e),
            Ok(session) => {
                let h = advertise_slab()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(session) as u32;
                ok(json!(h))
            }
        }
    }
    #[cfg(not(feature = "discovery"))]
    err("discovery feature not enabled in this build")
}

fn mdns_advertise_close_dispatch(_p: Value) -> Value {
    #[cfg(feature = "discovery")]
    {
        let handle = match _p["advertiseHandle"].as_u64() {
            Some(h) => h,
            None => return err("missing advertiseHandle"),
        };
        let mut slab = advertise_slab().lock().unwrap_or_else(|e| e.into_inner());
        if slab.contains(handle as usize) {
            slab.remove(handle as usize);
        }
    }
    ok(json!({}))
}

// ── Session ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionAcceptPayload {
    endpoint_handle: u64,
}

/// Accept the next incoming raw QUIC session from a remote peer.
///
/// Blocks until a peer connects or the endpoint shuts down.  Returns
/// `{ sessionHandle, nodeId }` on success, or `null` when the endpoint
/// is closed.
async fn session_accept_dispatch(p: Value) -> Value {
    let args: SessionAcceptPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    match iroh_http_core::Session::accept(ep).await {
        Err(e) => err_core(e),
        Ok(None) => ok(json!(null)),
        Ok(Some(session)) => {
            // Retrieve the remote peer's public key so JS can build a PublicKey.
            let node_id = session
                .remote_id()
                .map(|pk| iroh_http_core::base32_encode(pk.as_bytes()))
                .unwrap_or_default();
            ok(json!({ "sessionHandle": session.handle(), "nodeId": node_id }))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionConnectPayload {
    endpoint_handle: u64,
    node_id: String,
    direct_addrs: Option<Vec<String>>,
}

async fn session_connect_dispatch(p: Value) -> Value {
    let args: SessionConnectPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let addrs = match parse_direct_addrs(&args.direct_addrs) {
        Ok(a) => a,
        Err(e) => return err(e),
    };
    match iroh_http_core::Session::connect(ep, &args.node_id, addrs.as_deref()).await {
        Err(e) => err_core(e),
        Ok(session) => ok(json!({ "sessionHandle": session.handle() })),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEndpointPayload {
    endpoint_handle: u64,
    session_handle: u64,
}

async fn session_create_bidi_stream_dispatch(p: Value) -> Value {
    let args: SessionEndpointPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.create_bidi_stream().await {
        Err(e) => err_core(e),
        Ok(d) => ok(json!({ "readHandle": d.read_handle, "writeHandle": d.write_handle })),
    }
}

async fn session_next_bidi_stream_dispatch(p: Value) -> Value {
    let args: SessionEndpointPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.next_bidi_stream().await {
        Err(e) => err_core(e),
        Ok(None) => ok(json!(null)),
        Ok(Some(d)) => ok(json!({ "readHandle": d.read_handle, "writeHandle": d.write_handle })),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionClosePayload {
    endpoint_handle: u64,
    session_handle: u64,
    close_code: Option<u64>,
    reason: Option<String>,
}

fn session_close_dispatch(p: Value) -> Value {
    let args: SessionClosePayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.close(
        args.close_code.unwrap_or(0),
        args.reason.as_deref().unwrap_or(""),
    ) {
        Err(e) => err_core(e),
        Ok(()) => ok(json!({})),
    }
}

async fn session_closed_dispatch(p: Value) -> Value {
    let args: SessionEndpointPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.closed().await {
        Err(e) => err_core(e),
        Ok(info) => ok(json!({ "closeCode": info.close_code, "reason": info.reason })),
    }
}

async fn session_create_uni_stream_dispatch(p: Value) -> Value {
    let args: SessionEndpointPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.create_uni_stream().await {
        Err(e) => err_core(e),
        Ok(handle) => ok(json!({ "writeHandle": handle })),
    }
}

async fn session_next_uni_stream_dispatch(p: Value) -> Value {
    let args: SessionEndpointPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.next_uni_stream().await {
        Err(e) => err_core(e),
        Ok(None) => ok(json!(null)),
        Ok(Some(handle)) => ok(json!({ "readHandle": handle })),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionDatagramPayload {
    endpoint_handle: u64,
    session_handle: u64,
    data: String, // base64
}

fn session_send_datagram_dispatch(p: Value) -> Value {
    let args: SessionDatagramPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let data = match B64.decode(&args.data) {
        Ok(d) => d,
        Err(e) => return err(format!("base64 decode: {e}")),
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.send_datagram(&data) {
        Err(e) => err_core(e),
        Ok(()) => ok(json!({})),
    }
}

async fn session_recv_datagram_dispatch(p: Value) -> Value {
    let args: SessionEndpointPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.recv_datagram().await {
        Err(e) => err_core(e),
        Ok(None) => ok(json!(null)),
        Ok(Some(data)) => ok(json!({ "data": B64.encode(&data) })),
    }
}

fn session_max_datagram_size_dispatch(p: Value) -> Value {
    let args: SessionEndpointPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(args.endpoint_handle) {
        Some(e) => e,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {})", args.endpoint_handle),
            )
        }
    };
    let session = iroh_http_core::Session::from_handle(ep, args.session_handle);
    match session.max_datagram_size() {
        Err(e) => err_core(e),
        Ok(size) => ok(json!({ "maxDatagramSize": size })),
    }
}

// ── Transport events ──────────────────────────────────────────────────────────

type TransportEventRxMap = dashmap::DashMap<
    u64,
    std::sync::Arc<
        tokio::sync::Mutex<tokio::sync::mpsc::Receiver<iroh_http_core::events::TransportEvent>>,
    >,
>;
static TRANSPORT_EVENT_RXS: std::sync::OnceLock<TransportEventRxMap> = std::sync::OnceLock::new();

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartTransportEventsPayload {
    endpoint_handle: u64,
}

async fn start_transport_events_dispatch(p: Value) -> Value {
    let payload: StartTransportEventsPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(payload.endpoint_handle) {
        Some(ep) => ep,
        // Endpoint already closed — nothing to subscribe to.
        // Return false so the JS polling loop exits cleanly.
        None => return ok(serde_json::Value::Bool(false)),
    };
    let rx = match ep.subscribe_events() {
        Some(rx) => rx,
        None => {
            return err_code(
                "ALREADY_STARTED",
                "transport event receiver already taken; startTransportEvents called twice",
            )
        }
    };
    let rxs = TRANSPORT_EVENT_RXS.get_or_init(dashmap::DashMap::new);
    rxs.insert(
        payload.endpoint_handle,
        std::sync::Arc::new(tokio::sync::Mutex::new(rx)),
    );
    ok(serde_json::Value::Null)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NextTransportEventPayload {
    endpoint_handle: u64,
}

async fn next_transport_event_dispatch(p: Value) -> Value {
    let payload: NextTransportEventPayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let rxs = TRANSPORT_EVENT_RXS.get_or_init(dashmap::DashMap::new);
    let rx_arc = match rxs.get(&payload.endpoint_handle) {
        Some(r) => r.value().clone(),
        None => return ok(serde_json::Value::Null),
    };
    // DashMap ref dropped above; now safe to await
    let mut rx = rx_arc.lock().await;
    match rx.recv().await {
        None => {
            drop(rx);
            rxs.remove(&payload.endpoint_handle);
            ok(serde_json::Value::Null)
        }
        Some(event) => match serde_json::to_value(&event) {
            Ok(v) => ok(v),
            Err(e) => err(e),
        },
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NextPathChangePayload {
    endpoint_handle: u64,
    node_id: String,
}

async fn next_path_change_dispatch(p: Value) -> Value {
    let rxs = path_change_rxs();

    let payload: NextPathChangePayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(payload.endpoint_handle) {
        Some(ep) => ep,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!(
                    "node closed or not found (handle {})",
                    payload.endpoint_handle
                ),
            )
        }
    };

    let key = (payload.endpoint_handle, payload.node_id.clone());
    let rx_arc = rxs
        .entry(key.clone())
        .or_insert_with(|| {
            let rx = ep.subscribe_path_changes(&payload.node_id);
            std::sync::Arc::new(PathSub {
                rx: tokio::sync::Mutex::new(rx),
                notify: tokio::sync::Notify::new(),
            })
        })
        .clone();

    let mut rx = rx_arc.rx.lock().await;
    match tokio::select! {
        path = rx.recv() => path,
        () = rx_arc.notify.notified() => None,
    } {
        None => {
            drop(rx);
            rxs.remove_if(&key, |_, existing| {
                std::sync::Arc::ptr_eq(existing, &rx_arc)
            });
            ok(serde_json::Value::Null)
        }
        Some(path) => match serde_json::to_value(&path) {
            Ok(v) => ok(v),
            Err(e) => err(e),
        },
    }
}

fn unsubscribe_path_changes_dispatch(p: Value) -> Value {
    let payload: NextPathChangePayload = match serde_json::from_value(p) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ep = match get_endpoint(payload.endpoint_handle) {
        Some(ep) => ep,
        None => {
            return err_code(
                "INVALID_HANDLE",
                format!(
                    "node closed or not found (handle {})",
                    payload.endpoint_handle
                ),
            )
        }
    };

    if let Some((_, sub)) =
        path_change_rxs().remove(&(payload.endpoint_handle, payload.node_id.clone()))
    {
        sub.notify.notify_waiters();
    }
    ep.unsubscribe_path_changes(&payload.node_id);
    ok(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_http_core::{NodeOptions, ResponseHeadEntry};
    use tokio::sync::oneshot;

    async fn test_endpoint() -> (u64, IrohEndpoint) {
        let mut opts = NodeOptions::default();
        opts.networking.disabled = true;
        opts.networking.bind_addrs = vec!["127.0.0.1:0".to_string()];
        let ep = IrohEndpoint::bind(opts).await.unwrap();
        let handle = insert_endpoint(ep.clone());
        (handle, ep)
    }

    fn pending_payload(
        ep: &IrohEndpoint,
    ) -> (RequestPayload, oneshot::Receiver<ResponseHeadEntry>) {
        let (tx, rx) = oneshot::channel();
        let req_handle = ep.handles().allocate_req_handle(tx).unwrap();
        (
            RequestPayload {
                req_handle,
                req_body_handle: 0,
                res_body_handle: 0,
                method: "GET".to_string(),
                url: "httpi://test/".to_string(),
                headers: Vec::new(),
                remote_node_id: "peer".to_string(),
                is_bidi: false,
            },
            rx,
        )
    }

    fn assert_undeliverable(mut rx: oneshot::Receiver<ResponseHeadEntry>) {
        let expected = iroh_http_adapter::reject_undeliverable("test assertion");
        let head = rx
            .try_recv()
            .expect("undeliverable request should receive an immediate response head");
        assert_eq!(head.status, expected.status);
        assert_eq!(head.headers, expected.headers);
    }

    async fn cleanup(handle: u64, ep: IrohEndpoint) {
        serve_registry::remove(handle);
        let _ = remove_endpoint(handle);
        ep.close_force().await;
    }

    #[tokio::test]
    async fn deno_delivery_registry_miss_rejects_undeliverable_request() {
        let (handle, ep) = test_endpoint().await;
        let (payload, rx) = pending_payload(&ep);

        deliver_request_to_queue(handle, &ep, payload);

        assert_undeliverable(rx);
        cleanup(handle, ep).await;
    }

    #[tokio::test]
    async fn deno_delivery_shutdown_mid_request_rejects_without_queueing() {
        let (handle, ep) = test_endpoint().await;
        let queue = serve_registry::register(handle);
        serve_registry::signal_shutdown(handle);
        let (payload, rx) = pending_payload(&ep);

        deliver_request_to_queue(handle, &ep, payload);

        assert_undeliverable(rx);
        assert!(queue.rx.try_lock().unwrap().try_recv().is_err());
        cleanup(handle, ep).await;
    }

    #[tokio::test]
    async fn deno_delivery_queue_full_rejects_undeliverable_request() {
        let (handle, ep) = test_endpoint().await;
        let queue = serve_registry::register(handle);
        for i in 0..10_000 {
            if queue.tx.try_send(json!({ "reqHandle": i })).is_err() {
                break;
            }
        }
        assert!(
            queue
                .tx
                .try_send(json!({ "reqHandle": "sentinel" }))
                .is_err(),
            "test setup should fill the bounded serve queue"
        );
        let (payload, rx) = pending_payload(&ep);

        deliver_request_to_queue(handle, &ep, payload);

        assert_undeliverable(rx);
        cleanup(handle, ep).await;
    }

    #[tokio::test]
    async fn deno_stop_serve_drains_queued_requests_and_stops_polling() {
        let (handle, ep) = test_endpoint().await;
        let queue = serve_registry::register(handle);
        let (payload, rx) = pending_payload(&ep);
        queue
            .tx
            .try_send(json!({ "reqHandle": payload.req_handle }))
            .unwrap();

        let result = stop_serve(json!({ "endpointHandle": handle })).await;

        assert!(
            result.get("ok").is_some(),
            "stopServe should succeed: {result}"
        );
        assert_undeliverable(rx);
        let mut out = [0u8; 64];
        let poll =
            unsafe { crate::iroh_http_try_next_request(handle, out.as_mut_ptr(), out.len()) };
        assert_eq!(poll, -1, "polling should stop after stopServe shutdown");
        cleanup(handle, ep).await;
    }
}
