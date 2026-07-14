//! Tauri commands for the iroh-http plugin.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use bytes::Bytes;
use iroh_http_core::{
    endpoint::NodeOptions, parse_direct_addrs, respond, ConnectionEvent, DiscoveryOptions,
    NetworkingOptions, PoolOptions, RequestPayload, StreamingOptions,
};
use serde::{Deserialize, Serialize};
use tauri::{command, ipc::Channel, ipc::Response};

use tauri::Manager;

use crate::state;

use iroh_http_adapter::{
    coerce_endpoint_options, coerce_fetch_options, coerce_serve_options, core_error_to_json,
    deliver_request, format_error_json, validate_header_rows, DeliverableRequest,
    RawEndpointOptions, RawFetchOptions, RawServeOptions, RequestTransport, Undeliverable,
};

// ── Endpoint ──────────────────────────────────────────────────────────────────

/// Options for creating an Iroh endpoint from the Tauri frontend.
///
/// All fields are optional — omit for sensible defaults.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEndpointArgs {
    pub key: Option<String>,
    pub idle_timeout: Option<f64>,
    pub relay_mode: Option<String>,
    pub relays: Option<Vec<String>>,
    pub bind_addrs: Option<Vec<String>>,
    pub dns_discovery: Option<String>,
    pub dns_discovery_enabled: Option<bool>,
    pub channel_capacity: Option<usize>,
    pub max_chunk_size_bytes: Option<usize>,
    pub handle_ttl: Option<f64>,
    pub sweep_interval: Option<f64>,
    pub disable_networking: Option<bool>,
    pub proxy_url: Option<String>,
    pub proxy_from_env: Option<bool>,
    pub keylog: Option<bool>,
    pub compression_min_body_bytes: Option<usize>,
    pub compression_level: Option<i32>,
    pub max_header_bytes: Option<f64>,
    /// TAURI-002: pool-tuning options previously ignored.
    pub max_pooled_connections: Option<usize>,
    pub pool_idle_timeout_ms: Option<f64>,
}

/// Info returned after a successful endpoint bind.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointInfoPayload {
    pub endpoint_handle: u64,
    pub node_id: String,
}

/// Whether endpoint creation needs the mobile platform DNS bridge.
///
/// DNS discovery can be disabled while relay or proxy hostnames still need
/// resolution, so only the stronger fully-offline mode skips the native query.
#[cfg(any(mobile, test))]
fn should_query_mobile_dns(opts: &NodeOptions) -> bool {
    !opts.networking.disabled
}

/// Bind an Iroh endpoint and return a handle + identity info.
#[command]
pub async fn create_endpoint<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    args: Option<CreateEndpointArgs>,
) -> Result<EndpointInfoPayload, String> {
    let opts = args
        .map(|a| -> Result<NodeOptions, String> {
            let endpoint = coerce_endpoint_options(RawEndpointOptions {
                idle_timeout_ms: a.idle_timeout,
                pool_idle_timeout_ms: a.pool_idle_timeout_ms,
                handle_ttl_ms: a.handle_ttl,
                sweep_interval_ms: a.sweep_interval,
                max_header_bytes: a.max_header_bytes,
                compression_level: a.compression_level,
            })
            .map_err(|e| e.to_json())?;
            Ok(NodeOptions {
                key: match a.key {
                    Some(k) => {
                        let decoded = B64
                            .decode(&k)
                            .map_err(|_| "secret key: invalid base64".to_string())?;
                        let arr: [u8; 32] = decoded.try_into().map_err(|v: Vec<u8>| {
                            format!("secret key must be exactly 32 bytes, got {}", v.len())
                        })?;
                        Some(arr)
                    }
                    None => None,
                },
                networking: NetworkingOptions {
                    relay_mode: a.relay_mode,
                    relays: a.relays.unwrap_or_default(),
                    bind_addrs: a.bind_addrs.unwrap_or_default(),
                    idle_timeout_ms: endpoint.idle_timeout_ms,
                    proxy_url: a.proxy_url,
                    proxy_from_env: a.proxy_from_env.unwrap_or(false),
                    disabled: a.disable_networking.unwrap_or(false),
                },
                discovery: DiscoveryOptions::new(
                    a.dns_discovery,
                    a.dns_discovery_enabled.unwrap_or(true),
                ),
                pool: PoolOptions {
                    max_connections: a.max_pooled_connections,
                    idle_timeout_ms: endpoint.pool_idle_timeout_ms,
                },
                streaming: StreamingOptions {
                    channel_capacity: a.channel_capacity,
                    max_chunk_size_bytes: a.max_chunk_size_bytes,
                    drain_timeout_ms: None,
                    handle_ttl_ms: endpoint.handle_ttl_ms,
                    sweep_interval_ms: endpoint.sweep_interval_ms,
                },
                capabilities: Vec::new(),
                keylog: a.keylog.unwrap_or(false),
                max_header_size: endpoint.max_header_bytes,
                max_response_body_bytes: None,
                compression: if a.compression_min_body_bytes.is_some()
                    || a.compression_level.is_some()
                {
                    Some(iroh_http_core::CompressionOptions {
                        min_body_bytes: a
                            .compression_min_body_bytes
                            .unwrap_or(iroh_http_core::CompressionOptions::DEFAULT_MIN_BODY_BYTES),
                        level: endpoint.compression_level,
                    })
                } else {
                    None
                },
            })
        })
        .transpose()?
        .unwrap_or_default();

    // On mobile, iroh's default DNS resolver cannot read the system nameservers
    // (Android has no `/etc/resolv.conf` and needs a JNI-initialised
    // `ndk_context`), so relay/pkarr/DNS-discovery lookups time out. Query the
    // platform's active nameservers natively and hand them to the endpoint.
    // An empty result falls back to iroh's default resolver (works on iOS).
    // A native error is fatal: on Android the default path is exactly the
    // unavailable resolver this bridge exists to replace, and silently taking
    // it would create a node whose DNS requests only time out.
    #[cfg(mobile)]
    let opts = {
        let mut opts = opts;
        if should_query_mobile_dns(&opts) {
            if let Some(mdns) = app.try_state::<crate::mobile_mdns::MobileMdns<R>>() {
                let servers = mdns.get_dns_servers().map_err(|e| {
                    format_error_json("REFUSED", format!("failed to query platform DNS: {e}"))
                })?;
                if !servers.is_empty() {
                    opts.discovery.dns_nameservers = servers;
                }
            }
        }
        opts
    };

    let ep = iroh_http_core::endpoint::IrohEndpoint::bind(opts)
        .await
        .map_err(|e| core_error_to_json(&e))?;

    // #310: on mobile, register the in-process AddressLookup on this endpoint so
    // iroh's dialer resolves natively-discovered peers by node-id (desktop gets
    // this from iroh-http-discovery's MdnsAddressLookup). Same `add()` call
    // desktop makes. Missing state (scheme/discovery not configured) is a no-op.
    #[cfg(mobile)]
    if let Some(lookup) = app.try_state::<crate::mobile_address_lookup::MobileAddressLookup>() {
        if let Ok(services) = ep.raw().address_lookup() {
            services.add(lookup.inner().clone());
        }
    }

    let node_id = ep.node_id().to_string();
    let (handle, replaced) = state::replace_endpoint_for_node_id(node_id.clone(), ep);
    if let Some((old_handle, old_ep)) = replaced {
        tracing::warn!(
            node_id = %node_id,
            old_handle,
            new_handle = handle,
            "iroh-http-tauri: replacing existing endpoint for node id"
        );
        old_ep.close_force().await;

        // F11: a WebView reload / #336 foreground rebuild orphans every
        // discovery session the previous JS context started — its close
        // commands can no longer run, so a stale advertisement would keep
        // publishing the now-dead QUIC port and browse listeners would
        // accumulate. Retire them all; the new context restarts what it needs.
        retire_orphaned_discovery_sessions::<R>(&app);

        // #336: the endpoint was rebuilt (foreground recovery / webview
        // reload). Addresses discovered before the previous endpoint died may
        // now be stale, so drop them and let a fresh browse repopulate the
        // lookup instead of dialing dead paths.
        #[cfg(mobile)]
        if let Some(lookup) = app.try_state::<crate::mobile_address_lookup::MobileAddressLookup>() {
            lookup.clear();
        }
    }

    // Auto-bind the httpi:// scheme handler to the first endpoint created.
    // `try_state` returns None when `with_scheme()` was not set, so this
    // is a no-op in apps that don't use the scheme handler.
    if let Some(scheme_state) = app.try_state::<state::SchemeState>() {
        scheme_state.bind_if_unbound(handle);
    }

    Ok(EndpointInfoPayload {
        endpoint_handle: handle,
        node_id,
    })
}

/// Close an Iroh endpoint.
///
/// If `force` is `true`, aborts immediately.  Otherwise drains in-flight requests.
/// The endpoint is removed from the registry **after** closing to avoid
/// INVALID_HANDLE errors from background tasks during drain.
#[command]
pub async fn close_endpoint(endpoint_handle: u64, force: Option<bool>) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {endpoint_handle})"),
        )
    })?;
    if force.unwrap_or(false) {
        ep.close_force().await;
    } else {
        ep.close().await;
    }
    state::remove_endpoint(endpoint_handle);
    Ok(())
}

// ── Health probe (mobile lifecycle) ──────────────────────────────────────────

/// Transport health probe used by mobile foreground recovery.
///
/// Returns `Ok(true)` only when the underlying QUIC transport is still usable,
/// `Ok(false)` when the handle exists but the transport is dead (e.g. iOS
/// invalidated the socket during suspension), and `Err(INVALID_HANDLE)` when
/// the endpoint is gone entirely.
///
/// This replaces the previous handle-existence check: a live registry handle
/// is not evidence that remote peers can still reach this node (#336).
#[command]
pub async fn ping(endpoint_handle: u64) -> Result<bool, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("endpoint not found: {endpoint_handle}"),
        )
    })?;
    Ok(ep.transport_alive())
}

// ── Address introspection ─────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeAddrPayload {
    pub id: String,
    pub addrs: Vec<String>,
}

/// Discovery info payload: node id + dialable direct address + relay URL.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryInfoPayload {
    pub node_id: String,
    pub direct_address: Option<String>,
    pub direct_addresses: Vec<String>,
    pub relay_url: Option<String>,
}

/// Full node address: node ID + relay URL(s) + direct socket addresses.
#[command]
pub fn node_addr(endpoint_handle: u64) -> Result<NodeAddrPayload, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let info = ep.node_addr();
    Ok(NodeAddrPayload {
        id: info.id,
        addrs: info.addrs,
    })
}

/// Generate a ticket string for the given endpoint.
#[command]
pub fn node_ticket(endpoint_handle: u64) -> Result<String, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    iroh_http_core::node_ticket(&ep).map_err(|e| format_error_json("INTERNAL", e.message))
}

/// Home relay URL, or null if not connected to a relay.
#[command]
pub fn home_relay(endpoint_handle: u64) -> Result<Option<String>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    Ok(ep.home_relay())
}

/// Discovery info: node id + dialable direct `ip:port` address + relay URL.
///
/// `directAddress` carries the candidate's authoritative dialable port. A real
/// local or reflexive QAD port is preserved verbatim; only a platform
/// placeholder (`:0`/`:1`) borrows a same-family bound QUIC port. Desktop
/// resolves it from the core's `dialable_direct_address` (mirrors the
/// discovery-crate helper); mobile reuses `primary_direct_addr`, which also
/// falls back to the active OS interface inventory when the endpoint enumerates
/// no usable direct address at advertise time (#346).
#[command]
#[cfg(not(mobile))]
pub fn discovery_info(endpoint_handle: u64) -> Result<DiscoveryInfoPayload, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let direct_address = ep.dialable_direct_address();
    let direct_addresses = ep.dialable_direct_addresses();
    Ok(DiscoveryInfoPayload {
        node_id: ep.node_id().to_string(),
        direct_address,
        direct_addresses,
        relay_url: ep.home_relay(),
    })
}

#[command]
#[cfg(mobile)]
pub fn discovery_info<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    endpoint_handle: u64,
) -> Result<DiscoveryInfoPayload, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let direct_addresses = primary_direct_addrs(&ep, mobile_routable_ips(&state));
    let direct_address = direct_addresses.first().cloned();
    Ok(DiscoveryInfoPayload {
        node_id: ep.node_id().to_string(),
        direct_address,
        direct_addresses,
        relay_url: ep.home_relay(),
    })
}

/// Known addresses for a remote peer, or null if unknown.
#[command]
pub async fn peer_info(
    endpoint_handle: u64,
    node_id: String,
) -> Result<Option<NodeAddrPayload>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    Ok(ep.peer_info(&node_id).await.map(|info| NodeAddrPayload {
        id: info.id,
        addrs: info.addrs,
    }))
}

/// Per-peer connection statistics with path information.
#[command]
pub async fn peer_stats(
    endpoint_handle: u64,
    node_id: String,
) -> Result<Option<iroh_http_core::PeerStats>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    Ok(ep.peer_stats(&node_id).await)
}

/// Endpoint-level observability snapshot (point-in-time).
#[command]
pub fn endpoint_stats(endpoint_handle: u64) -> Result<iroh_http_core::EndpointStats, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    Ok(ep.endpoint_stats())
}

// ── Bridge methods ────────────────────────────────────────────────────────────

/// Read the next chunk from a body reader handle (raw binary).
///
/// Returns an `ArrayBuffer` on the JS side. A `null` response signals EOF.
#[command]
pub async fn next_chunk(endpoint_handle: u64, handle: u64) -> Result<Response, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let chunk = ep
        .handles()
        .next_chunk(handle)
        .await
        .map_err(|e| core_error_to_json(&e))?;
    match chunk {
        Some(bytes) => {
            let mut buf = Vec::with_capacity(bytes.len().saturating_add(1));
            buf.push(1u8);
            buf.extend_from_slice(&bytes);
            Ok(Response::new(buf))
        }
        None => Ok(Response::new(vec![0u8])),
    }
}

/// Non-blocking body read fast path (raw binary).
///
/// Returns:
/// - `Ok(ArrayBuffer)` — data was available synchronously.
/// - `Ok(null)` — EOF, handle cleaned up.
/// - `Err(_)` — channel empty or lock contended, caller should fall back to async `nextChunk`.
#[command]
pub fn try_next_chunk(endpoint_handle: u64, handle: u64) -> Result<Response, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let chunk = ep
        .handles()
        .try_next_chunk(handle)
        .map_err(|e| core_error_to_json(&e))?;
    match chunk {
        Some(bytes) => {
            let mut buf = Vec::with_capacity(bytes.len().saturating_add(1));
            buf.push(1u8);
            buf.extend_from_slice(&bytes);
            Ok(Response::new(buf))
        }
        None => Ok(Response::new(vec![0u8])),
    }
}

/// Push a binary chunk into a body writer handle.
///
/// Accepts raw binary data via `tauri::ipc::Request`. The endpoint handle and
/// body handle are passed as IPC headers (`iroh-ep` and `iroh-handle`).
#[command]
pub async fn send_chunk(request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let headers = request.headers();
    let endpoint_handle: u64 = headers
        .get("iroh-ep")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format_error_json("INVALID_INPUT", "missing or invalid iroh-ep header"))?;
    let handle: u64 = headers
        .get("iroh-handle")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| {
            format_error_json("INVALID_INPUT", "missing or invalid iroh-handle header")
        })?;

    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;

    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(data) => Bytes::from(data.clone()),
        tauri::ipc::InvokeBody::Json(val) => {
            // Fallback: accept a base64-encoded string for backward compat.
            if let Some(b64) = val.as_str() {
                let decoded = B64.decode(b64).map_err(|e| {
                    format_error_json("INVALID_INPUT", format!("base64 decode: {e}"))
                })?;
                Bytes::from(decoded)
            } else {
                return Err(format_error_json(
                    "INVALID_INPUT",
                    "expected raw binary body or base64 string",
                ));
            }
        }
    };

    ep.handles()
        .send_chunk(handle, bytes)
        .await
        .map_err(|e| core_error_to_json(&e))
}

/// Signal end-of-body for a writer handle.
#[command]
pub fn finish_body(endpoint_handle: u64, handle: u64) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    ep.handles()
        .finish_body(handle)
        .map_err(|e| core_error_to_json(&e))
}

/// Cancel a body reader, signalling EOF.
#[command]
pub fn cancel_request(endpoint_handle: u64, handle: u64) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    ep.handles().cancel_reader(handle);
    Ok(())
}

/// Allocate a body writer handle for streaming request bodies.
#[command]
pub fn create_body_writer(endpoint_handle: u64) -> Result<u64, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let (handle, reader) = ep
        .handles()
        .alloc_body_writer()
        .map_err(|e| core_error_to_json(&e))?;
    ep.handles().store_pending_reader(handle, reader);
    Ok(handle)
}

/// Allocate a cancellation token for an upcoming fetch call.
#[command]
pub fn create_fetch_token(endpoint_handle: u64) -> Result<u64, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    ep.handles()
        .alloc_fetch_token()
        .map_err(|e| core_error_to_json(&e))
}

/// Cancel an in-flight fetch by its token.
#[command]
pub fn cancel_in_flight(endpoint_handle: u64, token: u64) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    ep.handles().cancel_in_flight(token);
    Ok(())
}

// ── rawFetch ──────────────────────────────────────────────────────────────────

/// Arguments for `rawFetch` — send an HTTP request to a remote peer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFetchArgs {
    pub endpoint_handle: u64,
    pub node_id: String,
    pub url: String,
    pub method: String,
    pub headers: Vec<Vec<String>>,
    pub req_body_handle: Option<u64>,
    pub fetch_token: Option<u64>,
    pub direct_addrs: Option<Vec<String>>,
    pub timeout_ms: Option<f64>,
    pub decompress: Option<bool>,
    pub max_response_body_bytes: Option<f64>,
}

/// Response payload returned by `rawFetch`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfiResponsePayload {
    pub status: u16,
    pub headers: Vec<Vec<String>>,
    pub body_handle: u64,
    pub url: String,
}

/// Send an HTTP request to a remote Iroh peer.
#[command]
pub async fn fetch(args: RawFetchArgs) -> Result<FfiResponsePayload, String> {
    let ep = state::get_endpoint(args.endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {}", args.endpoint_handle),
        )
    })?;

    let opts = coerce_fetch_options(RawFetchOptions {
        node_id: args.node_id,
        url: args.url,
        method: args.method,
        direct_addrs: args.direct_addrs,
        headers: args.headers,
        timeout_ms: args.timeout_ms,
        max_response_body_bytes: args.max_response_body_bytes,
    })
    .map_err(|e| e.to_json())?;

    let req_body_reader = args
        .req_body_handle
        .and_then(|h| ep.handles().claim_pending_reader(h));

    let addrs = parse_direct_addrs(&opts.direct_addrs)?;
    let timeout = opts.timeout_ms.map(std::time::Duration::from_millis);
    let max_response_body_bytes = opts.max_response_body_bytes;
    let res = iroh_http_core::fetch(
        &ep,
        &opts.node_id,
        &opts.url,
        &opts.method,
        &opts.headers,
        req_body_reader,
        args.fetch_token,
        addrs.as_deref(),
        timeout,
        args.decompress.unwrap_or(true),
        max_response_body_bytes,
    )
    .await
    .map_err(|e| core_error_to_json(&e))?;

    let resp_headers: Vec<Vec<String>> = res.headers.into_iter().map(|(k, v)| vec![k, v]).collect();

    Ok(FfiResponsePayload {
        status: res.status,
        headers: resp_headers,
        body_handle: res.body_handle,
        url: res.url,
    })
}

// ── serve ─────────────────────────────────────────────────────────────────────

/// Serialised request payload pushed through the Tauri Channel.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServeEventPayload {
    pub req_handle: u64,
    pub req_body_handle: u64,
    pub res_body_handle: u64,
    pub is_bidi: bool,
    pub method: String,
    pub url: String,
    pub headers: Vec<Vec<String>>,
    pub remote_node_id: String,
}

/// Tauri request transport: hands each request to the frontend over an IPC
/// [`Channel`]. A `send` failure means the frontend will never see the request,
/// which [`deliver_request`] turns into the fail-closed 503.
struct TauriTransport {
    channel: Channel<ServeEventPayload>,
}

impl RequestTransport for TauriTransport {
    fn deliver(&self, req: &DeliverableRequest) -> Result<(), Undeliverable> {
        let event = ServeEventPayload {
            req_handle: req.req_handle,
            req_body_handle: req.req_body_handle,
            res_body_handle: req.res_body_handle,
            is_bidi: req.is_bidi,
            method: req.method.clone(),
            url: req.url.clone(),
            headers: req.headers.clone(),
            remote_node_id: req.remote_node_id.clone(),
        };
        self.channel.send(event).map_err(|e| {
            tracing::warn!("iroh-http-tauri: channel send error: {e}; responding 503");
            Undeliverable::new(format!("tauri channel send failed: {e}"))
        })
    }
}

/// Server-side configuration passed at `serve()` time.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServeArgs {
    pub endpoint_handle: u64,
    pub max_concurrency: Option<usize>,
    pub max_connections_per_peer: Option<usize>,
    pub request_timeout: Option<f64>,
    pub max_request_body_wire_bytes: Option<f64>,
    pub max_request_body_decoded_bytes: Option<f64>,
    pub max_total_connections: Option<f64>,
    pub drain_timeout: Option<f64>,
    pub load_shed: Option<bool>,
    pub decompress: Option<bool>,
}

/// Start the serve accept loop, streaming incoming requests via a Tauri Channel.
#[command]
pub async fn serve(
    args: ServeArgs,
    channel: Channel<ServeEventPayload>,
    conn_channel: Channel<ConnectionEvent>,
) -> Result<(), String> {
    let ep = state::get_endpoint(args.endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {}", args.endpoint_handle),
        )
    })?;

    let serve = coerce_serve_options(RawServeOptions {
        request_timeout_ms: args.request_timeout,
        max_request_body_wire_bytes: args.max_request_body_wire_bytes,
        max_request_body_decoded_bytes: args.max_request_body_decoded_bytes,
        max_total_connections: args.max_total_connections,
        drain_timeout_ms: args.drain_timeout,
    })
    .map_err(|e| e.to_json())?;
    let serve_opts = iroh_http_core::ServeOptions {
        max_concurrency: args.max_concurrency,
        max_connections_per_peer: args.max_connections_per_peer,
        request_timeout_ms: serve.request_timeout_ms,
        max_request_body_wire_bytes: serve.max_request_body_wire_bytes,
        max_request_body_decoded_bytes: serve.max_request_body_decoded_bytes,
        max_total_connections: serve.max_total_connections,
        drain_timeout_ms: serve.drain_timeout_ms,
        load_shed: args.load_shed,
        decompression: args.decompress,
    };

    let conn_event_fn: Option<std::sync::Arc<dyn Fn(ConnectionEvent) + Send + Sync>> = {
        let arc: std::sync::Arc<dyn Fn(ConnectionEvent) + Send + Sync> =
            std::sync::Arc::new(move |ev: ConnectionEvent| {
                if let Err(e) = conn_channel.send(ev) {
                    tracing::warn!("iroh-http-tauri: conn_channel send error: {e}");
                }
            });
        Some(arc)
    };

    // Clone ep so the request-dispatch closure can close orphaned requests on
    // channel failure without capturing the outer ep.
    let ep_for_closure = ep.clone();
    let transport = TauriTransport { channel };

    let handle = iroh_http_core::ffi_serve_with_callback(
        ep.clone(),
        serve_opts,
        move |payload: RequestPayload| {
            deliver_request(ep_for_closure.handles(), &transport, payload);
        },
        conn_event_fn,
    );
    ep.set_serve_handle(handle);

    Ok(())
}

/// Stop the serve loop for the given endpoint (graceful shutdown).
#[command]
pub fn stop_serve(endpoint_handle: u64) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    ep.stop_serve();
    Ok(())
}

/// Wait until the serve loop has fully exited (all requests drained).
///
/// Resolves immediately if serve was never started. Use after `stop_serve` to
/// confirm the loop has actually terminated before safe-to-free teardown.
#[command]
pub async fn wait_serve_stop(endpoint_handle: u64) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    ep.wait_serve_stop().await;
    Ok(())
}

/// Wait until this endpoint has been fully closed — either because `close_endpoint()`
/// was called or because the QUIC stack shut down natively.
#[command]
pub async fn wait_endpoint_closed(endpoint_handle: u64) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    ep.wait_closed().await;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondArgs {
    pub endpoint_handle: u64,
    pub req_handle: u64,
    pub status: u16,
    pub headers: Vec<Vec<String>>,
}

/// Send the response head for a pending request.
///
/// Called from the Tauri frontend after computing the response status and headers.
#[command]
pub fn respond_to_request(args: RespondArgs) -> Result<(), String> {
    let ep = state::get_endpoint(args.endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {}", args.endpoint_handle),
        )
    })?;
    let headers = validate_header_rows(args.headers).map_err(|e| e.to_json())?;
    respond(ep.handles(), args.req_handle, args.status, headers).map_err(|e| core_error_to_json(&e))
}
// ── Session ───────────────────────────────────────────────────────────────────

/// Arguments for establishing a session.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConnectArgs {
    pub endpoint_handle: u64,
    pub node_id: String,
    pub direct_addrs: Option<Vec<String>>,
}

/// Establish a session (QUIC connection) to a remote peer.
#[command]
pub async fn session_connect(args: SessionConnectArgs) -> Result<u64, String> {
    let ep = state::get_endpoint(args.endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {}", args.endpoint_handle),
        )
    })?;

    // TAURI-003: fail fast on invalid address strings rather than silently discarding them.
    let addrs: Option<Vec<std::net::SocketAddr>> = match args.direct_addrs.as_ref() {
        None => None,
        Some(v) => {
            let mut parsed = Vec::with_capacity(v.len());
            for s in v {
                match s.parse::<std::net::SocketAddr>() {
                    Ok(a) => parsed.push(a),
                    Err(_) => {
                        return Err(format_error_json(
                            "INVALID_INPUT",
                            format!("invalid socket address {:?}", s),
                        ))
                    }
                }
            }
            Some(parsed)
        }
    };

    iroh_http_core::Session::connect(ep, &args.node_id, addrs.as_deref())
        .await
        .map(|s| s.handle())
        .map_err(|e| core_error_to_json(&e))
}

/// Handles for a bidirectional stream on a session.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBidiStreamPayload {
    pub read_handle: u64,
    pub write_handle: u64,
}

/// Accept the next incoming session (QUIC connection) from a remote peer.
///
/// Blocks until a peer connects or the endpoint shuts down. Returns
/// `{ sessionHandle, nodeId }` on success, or `null` when the endpoint is closed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAcceptedPayload {
    pub session_handle: u64,
    pub node_id: String,
}

#[command]
pub async fn session_accept(
    endpoint_handle: u64,
) -> Result<Option<SessionAcceptedPayload>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = match iroh_http_core::Session::accept(ep)
        .await
        .map_err(|e| core_error_to_json(&e))?
    {
        Some(s) => s,
        None => return Ok(None),
    };
    let node_id = session
        .remote_id()
        .map(|pk| iroh_http_core::base32_encode(pk.as_bytes()))
        .unwrap_or_default();
    Ok(Some(SessionAcceptedPayload {
        session_handle: session.handle(),
        node_id,
    }))
}

/// Open a new bidirectional stream on an existing session.
#[command]
pub async fn session_create_bidi_stream(
    endpoint_handle: u64,
    session_handle: u64,
) -> Result<SessionBidiStreamPayload, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    let duplex = session
        .create_bidi_stream()
        .await
        .map_err(|e| core_error_to_json(&e))?;
    Ok(SessionBidiStreamPayload {
        read_handle: duplex.read_handle,
        write_handle: duplex.write_handle,
    })
}

/// Accept the next incoming bidirectional stream on a session.
/// Returns null when the session is closed.
#[command]
pub async fn session_next_bidi_stream(
    endpoint_handle: u64,
    session_handle: u64,
) -> Result<Option<SessionBidiStreamPayload>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    let result = session
        .next_bidi_stream()
        .await
        .map_err(|e| core_error_to_json(&e))?;
    Ok(result.map(|d| SessionBidiStreamPayload {
        read_handle: d.read_handle,
        write_handle: d.write_handle,
    }))
}

/// Close a session with optional close code and reason.
#[command]
pub async fn session_close(
    endpoint_handle: u64,
    session_handle: u64,
    close_code: Option<u64>,
    reason: Option<String>,
) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    session
        .close(close_code.unwrap_or(0), reason.as_deref().unwrap_or(""))
        .map_err(|e| core_error_to_json(&e))
}

/// Wait for a session to close. Returns { closeCode, reason }.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseInfoPayload {
    pub close_code: u64,
    pub reason: String,
}

#[command]
pub async fn session_closed(
    endpoint_handle: u64,
    session_handle: u64,
) -> Result<CloseInfoPayload, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    let info = session.closed().await.map_err(|e| core_error_to_json(&e))?;
    Ok(CloseInfoPayload {
        close_code: info.close_code,
        reason: info.reason,
    })
}

/// Open a new unidirectional (send-only) stream on a session.
/// Returns a write handle.
#[command]
pub async fn session_create_uni_stream(
    endpoint_handle: u64,
    session_handle: u64,
) -> Result<u64, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    session
        .create_uni_stream()
        .await
        .map_err(|e| core_error_to_json(&e))
}

/// Accept the next incoming unidirectional stream on a session.
/// Returns a read handle, or null when the session is closed.
#[command]
pub async fn session_next_uni_stream(
    endpoint_handle: u64,
    session_handle: u64,
) -> Result<Option<u64>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    session
        .next_uni_stream()
        .await
        .map_err(|e| core_error_to_json(&e))
}

/// Send a datagram on a session. Data is base64-encoded.
#[command]
pub async fn session_send_datagram(
    endpoint_handle: u64,
    session_handle: u64,
    data: String,
) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let bytes = B64
        .decode(&data)
        .map_err(|e| format!("base64 decode: {e}"))?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    session
        .send_datagram(&bytes)
        .map_err(|e| core_error_to_json(&e))
}

/// Receive the next datagram on a session. Returns base64, or null when closed.
#[command]
pub async fn session_recv_datagram(
    endpoint_handle: u64,
    session_handle: u64,
) -> Result<Option<String>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    let result = session
        .recv_datagram()
        .await
        .map_err(|e| core_error_to_json(&e))?;
    Ok(result.map(|d| B64.encode(&d)))
}

/// Get the maximum datagram payload size for a session.
#[command]
pub fn session_max_datagram_size(
    endpoint_handle: u64,
    session_handle: u64,
) -> Result<Option<u32>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_core::Session::from_handle(ep, session_handle);
    let result = session
        .max_datagram_size()
        .map_err(|e| core_error_to_json(&e))?;
    Ok(result.map(|s| s as u32))
}

// ── Key operations ────────────────────────────────────────────────────────────

/// Sign arbitrary bytes with a 32-byte Ed25519 secret key (base64-encoded).
/// Returns a 64-byte signature as base64.
#[command]
pub fn secret_key_sign(secret_key: String, data: String) -> Result<String, String> {
    let key_bytes: [u8; 32] = B64
        .decode(&secret_key)
        .map_err(|e| format!("base64 decode key: {e}"))?
        .try_into()
        .map_err(|_| "secret key must be 32 bytes".to_string())?;
    let data_bytes = B64
        .decode(&data)
        .map_err(|e| format!("base64 decode data: {e}"))?;
    let sig =
        iroh_http_core::secret_key_sign(&key_bytes, &data_bytes).map_err(|e| e.to_string())?;
    Ok(B64.encode(sig))
}

/// Verify a 64-byte Ed25519 signature against a 32-byte public key.
/// All inputs base64-encoded.  Returns `true` on success.
#[command]
pub fn public_key_verify(
    public_key: String,
    data: String,
    signature: String,
) -> Result<bool, String> {
    let key_bytes: [u8; 32] = B64
        .decode(&public_key)
        .map_err(|e| format!("base64 decode key: {e}"))?
        .try_into()
        .map_err(|_| "public key must be 32 bytes".to_string())?;
    let data_bytes = B64
        .decode(&data)
        .map_err(|e| format!("base64 decode data: {e}"))?;
    let sig_bytes: [u8; 64] = B64
        .decode(&signature)
        .map_err(|e| format!("base64 decode sig: {e}"))?
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    Ok(iroh_http_core::public_key_verify(
        &key_bytes,
        &data_bytes,
        &sig_bytes,
    ))
}

/// Generate a fresh Ed25519 secret key. Returns 32 raw bytes as base64.
#[command]
pub fn generate_secret_key() -> Result<String, String> {
    let key = iroh_http_core::generate_secret_key().map_err(|e| e.to_string())?;
    Ok(B64.encode(key))
}

// ── mDNS browse / advertise ──────────────────────────────────────────────────

use std::sync::{Mutex, OnceLock};

#[cfg(all(feature = "discovery", not(mobile)))]
use std::sync::Arc;
#[cfg(all(feature = "discovery", not(mobile)))]
use tokio::sync::Mutex as TokioMutex;

#[cfg(all(feature = "discovery", not(mobile)))]
type BrowseHandle = Arc<TokioMutex<iroh_http_discovery::BrowseSession>>;

/// ISS-017: shared mobile mDNS event buffer, accessible from both
/// `browse_peers_next` and `browse_peers_close` to clear stale events.
#[cfg(mobile)]
fn mobile_mdns_buffer() -> &'static Mutex<
    std::collections::HashMap<
        u64,
        std::collections::VecDeque<crate::mobile_mdns::MobileDiscoveryEvent>,
    >,
> {
    static BUFFER: OnceLock<
        Mutex<
            std::collections::HashMap<
                u64,
                std::collections::VecDeque<crate::mobile_mdns::MobileDiscoveryEvent>,
            >,
        >,
    > = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Set of browse handles that are still open. The native mDNS poll
/// (`browse_peers_poll`) is non-blocking and, once a session is stopped, resolves
/// with an empty event list rather than an error. `browse_peers_next` long-polls
/// that layer, so it consults this set to detect closure and terminate with
/// `None` (stream finished) instead of spinning forever after
/// `browse_peers_close`.
#[cfg(mobile)]
fn mobile_active_browses() -> &'static Mutex<std::collections::HashSet<u64>> {
    static S: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

#[cfg(all(feature = "discovery", not(mobile)))]
fn browse_slab() -> &'static Mutex<slab::Slab<BrowseHandle>> {
    static S: OnceLock<Mutex<slab::Slab<BrowseHandle>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(slab::Slab::new()))
}

#[cfg(all(feature = "discovery", not(mobile)))]
fn advertise_slab() -> &'static Mutex<slab::Slab<iroh_http_discovery::AdvertiseSession>> {
    static S: OnceLock<Mutex<slab::Slab<iroh_http_discovery::AdvertiseSession>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(slab::Slab::new()))
}

/// F11: identifies a live DNS-SD discovery session so it can be retired when the
/// owning WebView context is orphaned (endpoint replacement / reload).
#[cfg(any(mobile, feature = "discovery"))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiscoveryKind {
    /// `browse_peers_start` — peer discovery.
    BrowsePeers,
    /// `advertise_peer_start` — advertise this node.
    AdvertisePeer,
    /// `advertise_start` — generic DNS-SD advertise.
    GenericAdvertise,
    /// `browse_start` — generic DNS-SD browse.
    GenericBrowse,
}

/// F11: every discovery session started by the current WebView context.
///
/// A WebView reload or foreground endpoint rebuild (`replace_endpoint_for_node_id`)
/// orphans the JavaScript that owns these handles, so its `*_close` commands can
/// never run. Without retirement, a stale advertisement keeps publishing the
/// dead QUIC port and browse listeners accumulate. Sessions register here on
/// start, deregister on close, and are all retired on endpoint replacement.
#[cfg(any(mobile, feature = "discovery"))]
fn active_discovery_sessions() -> &'static Mutex<Vec<(DiscoveryKind, u64)>> {
    static S: OnceLock<Mutex<Vec<(DiscoveryKind, u64)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(any(mobile, feature = "discovery"))]
fn track_discovery(kind: DiscoveryKind, handle: u64) {
    active_discovery_sessions()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((kind, handle));
}

#[cfg(any(mobile, feature = "discovery"))]
fn untrack_discovery(kind: DiscoveryKind, handle: u64) {
    active_discovery_sessions()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|&(k, h)| !(k == kind && h == handle));
}

/// F11: retire every discovery session orphaned by an endpoint replacement.
///
/// Called from `create_endpoint` when an existing endpoint for the same node id
/// is replaced (WebView reload / #336 foreground rebuild). The previous JS
/// context can no longer close its advertise/browse handles, so we retire them
/// here — mirroring each `*_close` command — and the new context re-starts
/// whatever it needs.
#[cfg(all(feature = "discovery", not(mobile)))]
fn retire_orphaned_discovery_sessions<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) {
    let drained: Vec<(DiscoveryKind, u64)> = std::mem::take(
        &mut *active_discovery_sessions()
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );
    for (kind, handle) in drained {
        match kind {
            DiscoveryKind::BrowsePeers => {
                let mut slab = browse_slab().lock().unwrap_or_else(|e| e.into_inner());
                if slab.contains(handle as usize) {
                    slab.remove(handle as usize);
                }
            }
            // Peer and generic advertise share `advertise_slab`; dropping the
            // session stops the advertisement.
            DiscoveryKind::AdvertisePeer | DiscoveryKind::GenericAdvertise => {
                let mut slab = advertise_slab().lock().unwrap_or_else(|e| e.into_inner());
                if slab.contains(handle as usize) {
                    slab.remove(handle as usize);
                }
            }
            DiscoveryKind::GenericBrowse => {
                let mut slab = generic_browse_slab()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if slab.contains(handle as usize) {
                    slab.remove(handle as usize);
                }
            }
        }
    }
}

#[cfg(mobile)]
fn retire_orphaned_discovery_sessions<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let drained: Vec<(DiscoveryKind, u64)> = std::mem::take(
        &mut *active_discovery_sessions()
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );
    let Some(state) = app.try_state::<crate::mobile_mdns::MobileMdns<R>>() else {
        return;
    };
    for (kind, handle) in drained {
        match kind {
            DiscoveryKind::BrowsePeers => {
                mobile_active_browses()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&handle);
                let _ = state.browse_peers_stop(handle);
                mobile_mdns_buffer()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&handle);
            }
            DiscoveryKind::AdvertisePeer => {
                let _ = state.advertise_peer_stop(handle);
            }
            DiscoveryKind::GenericAdvertise => {
                let _ = state.advertise_stop(handle);
            }
            DiscoveryKind::GenericBrowse => {
                mobile_active_dns_sd_browses()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&handle);
                let _ = state.browse_stop(handle);
                mobile_dns_sd_buffer()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&handle);
            }
        }
    }
}

#[cfg(all(not(feature = "discovery"), not(mobile)))]
fn retire_orphaned_discovery_sessions<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) {}

/// Discovery event payload for the Tauri frontend.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerDiscoveryEventPayload {
    pub is_active: bool,
    pub node_id: String,
    pub addrs: Vec<String>,
}

/// Start a browse session: discover peers on the local network via mDNS.
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn browse_peers(endpoint_handle: u64, service_name: String) -> Result<u64, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_discovery::browse_peers(ep.raw(), &service_name)
        .await
        .map_err(|e| format_error_json("REFUSED", e))?;
    let handle = browse_slab()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(Arc::new(TokioMutex::new(session))) as u64;
    track_discovery(DiscoveryKind::BrowsePeers, handle);
    Ok(handle)
}

#[command]
#[cfg(all(not(feature = "discovery"), not(mobile)))]
pub async fn browse_peers(_endpoint_handle: u64, _service_name: String) -> Result<u64, String> {
    Err(format_error_json(
        "UNKNOWN",
        "discovery feature not enabled in this build",
    ))
}

#[command]
#[cfg(mobile)]
pub async fn browse_peers<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    _endpoint_handle: u64,
    service_name: String,
) -> Result<u64, String> {
    let browse_id = state
        .browse_peers_start(&service_name)
        .map_err(|e| format_error_json("REFUSED", e))?;
    // Mark the session active so `browse_peers_next` knows to keep long-polling
    // until `browse_peers_close` retires the handle.
    mobile_active_browses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(browse_id);
    track_discovery(DiscoveryKind::BrowsePeers, browse_id);
    Ok(browse_id)
}

/// Poll the next discovery event from a browse session.
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn browse_peers_next(
    browse_handle: u64,
) -> Result<Option<PeerDiscoveryEventPayload>, String> {
    let session = {
        browse_slab()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(browse_handle as usize)
            .cloned()
    }
    .ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid browse handle: {browse_handle}"),
        )
    })?;
    let event = session.lock().await.next_event().await;
    Ok(event.map(|ev| PeerDiscoveryEventPayload {
        is_active: ev.is_active,
        node_id: ev.node_id,
        addrs: ev.addrs,
    }))
}

#[command]
#[cfg(all(not(feature = "discovery"), not(mobile)))]
pub async fn browse_peers_next(
    _browse_handle: u64,
) -> Result<Option<PeerDiscoveryEventPayload>, String> {
    Err(format_error_json(
        "UNKNOWN",
        "discovery feature not enabled in this build",
    ))
}

#[command]
#[cfg(mobile)]
pub async fn browse_peers_next<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    lookup: tauri::State<'_, crate::mobile_address_lookup::MobileAddressLookup>,
    browse_handle: u64,
) -> Result<Option<PeerDiscoveryEventPayload>, String> {
    // The native NWBrowser / NsdManager layer is poll-based and non-blocking:
    // `browse_peers_poll` returns `[]` whenever nothing has been discovered *yet*.
    // The shared JS async iterator, however, treats a `null` event as
    // "stream ended" — matching the desktop `next_event().await` contract,
    // where `None` only ever means the browse session finished. Returning
    // `Ok(None)` on an empty poll would therefore make `node.browse()` stop
    // immediately on the very first call, before any peer had a chance to
    // appear (the browse UI flips straight back to "start browsing").
    //
    // Long-poll instead: keep re-polling with a short delay until an event is
    // available or the session is closed, so `None` retains its cross-platform
    // "finished" meaning. Closure is detected via `mobile_active_browses`
    // because the native poll resolves empty (not an error) after stop.
    let buffer = mobile_mdns_buffer();

    loop {
        // 1. Drain any event buffered by a previous poll first.
        {
            let mut map = buffer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(queue) = map.get_mut(&browse_handle) {
                if let Some(ev) = queue.pop_front() {
                    return Ok(Some(PeerDiscoveryEventPayload {
                        is_active: ev.kind == "discovered",
                        node_id: ev.node_id,
                        addrs: ev.addrs,
                    }));
                }
            }
        }

        // 2. Stop long-polling once the session has been closed.
        let active = mobile_active_browses()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&browse_handle);
        if !active {
            return Ok(None);
        }

        // 3. Poll the native layer for freshly discovered / expired peers.
        let mut events = state
            .browse_peers_poll(browse_handle)
            .map_err(|e| format_error_json("INVALID_HANDLE", e))?;

        // #310: feed every freshly polled event into the in-process AddressLookup so
        // iroh's dialer can resolve discovered peers by node-id, regardless of how
        // fast the JS side drains events. `discovered` upserts addrs; `expired`
        // evicts. This is what gives mobile `fetch(nodeId)` auto-dial parity.
        for ev in &events {
            lookup.apply_event(&ev.kind, &ev.node_id, &ev.addrs);
        }

        let first = events.drain(..1.min(events.len())).next();

        // Buffer remaining events.
        if !events.is_empty() {
            let mut map = buffer.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(browse_handle).or_default().extend(events);
        }

        if let Some(ev) = first {
            return Ok(Some(PeerDiscoveryEventPayload {
                is_active: ev.kind == "discovered",
                node_id: ev.node_id,
                addrs: ev.addrs,
            }));
        }

        // 4. Nothing discovered yet — wait briefly, then poll again.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Close a browse session, stopping mDNS discovery.
#[command]
#[cfg(not(mobile))]
pub fn browse_peers_close(browse_handle: u64) {
    #[cfg(feature = "discovery")]
    {
        untrack_discovery(DiscoveryKind::BrowsePeers, browse_handle);
        let mut slab = browse_slab().lock().unwrap_or_else(|e| e.into_inner());
        if slab.contains(browse_handle as usize) {
            slab.remove(browse_handle as usize);
        }
    }
}

#[command]
#[cfg(mobile)]
pub fn browse_peers_close<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    browse_handle: u64,
) {
    untrack_discovery(DiscoveryKind::BrowsePeers, browse_handle);
    // Retire the handle first so any in-flight `browse_peers_next` long-poll
    // observes the closure and returns `None` (stream finished).
    mobile_active_browses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&browse_handle);
    let _ = state.browse_peers_stop(browse_handle);
    // ISS-017: clear stale buffered events for the closed browse session.
    mobile_mdns_buffer()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&browse_handle);
}

/// Start advertising this node on the local network via mDNS.
///
/// Declared `async` so Tauri runs it inside the async (Tokio) runtime. The mDNS
/// address-lookup constructor (`advertise_peer` → `MdnsAddressLookup::build`)
/// calls `tokio::runtime::Handle::current()`, which panics with "there is no
/// reactor running" when invoked outside a runtime context. A synchronous Tauri
/// command runs on a plain worker thread with no runtime, so enabling mDNS
/// advertising aborted the process. Mirrors the Node adapter fix (#243).
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn advertise_peer(endpoint_handle: u64, service_name: String) -> Result<u64, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let session = iroh_http_discovery::advertise_peer(ep.raw(), &service_name)
        .map_err(|e| format_error_json("REFUSED", e))?;
    let handle = advertise_slab()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session) as u64;
    track_discovery(DiscoveryKind::AdvertisePeer, handle);
    Ok(handle)
}

#[command]
#[cfg(all(not(feature = "discovery"), not(mobile)))]
pub fn advertise_peer(_endpoint_handle: u64, _service_name: String) -> Result<u64, String> {
    Err(format_error_json(
        "UNKNOWN",
        "discovery feature not enabled in this build",
    ))
}

#[command]
#[cfg(mobile)]
pub fn advertise_peer<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    endpoint_handle: u64,
    service_name: String,
) -> Result<u64, String> {
    // TAURI-014: Resolve node identity so native advertise can publish TXT
    // metadata (pk, relay) that browse expects.
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let node_id = ep.node_id().to_string();
    let relay = ep.home_relay();
    // #346: publish a primary direct `ip:port` candidate so browsing peers can
    // dial this node over the LAN. A real local or reflexive port is
    // authoritative; only a `:0`/`:1` placeholder borrows a same-family bound
    // port. No placeholder reaches the dialer.
    let address = primary_direct_addr(&ep, mobile_routable_ips(&state));
    let handle = state
        .advertise_peer_start(
            &service_name,
            &node_id,
            relay.as_deref(),
            address.as_deref(),
        )
        .map_err(|e| format_error_json("REFUSED", e))?;
    track_discovery(DiscoveryKind::AdvertisePeer, handle);
    Ok(handle)
}

/// Pick the primary direct address to advertise. See #346.
///
/// Prefers a reconciled direct address that is already routable (has a real
/// port and is neither loopback nor unspecified). When the endpoint enumerates
/// no such address — the iOS failure mode, where `ip_addrs()` yields nothing
/// usable at advertise time even though the QUIC socket is bound — it falls
/// back to combining a routable IP from the active interface inventory with a
/// same-family bound QUIC port.
#[cfg(mobile)]
fn primary_direct_addr(
    ep: &iroh_http_core::IrohEndpoint,
    fallback_ips: impl IntoIterator<Item = std::net::IpAddr>,
) -> Option<String> {
    let reconciled = ep.direct_socket_addrs();
    let bound_sockets = ep.bound_sockets();
    select_primary_direct_addr(&reconciled, &bound_sockets, fallback_ips).map(|a| a.to_string())
}

/// All routable direct addresses to advertise on mobile (#348).
///
/// The plural of [`primary_direct_addr`]: every routable real-port candidate is
/// kept verbatim, while only `:0`/`:1` placeholders borrow a same-family bound
/// port. A device on both a VPN `10.x` interface and the real LAN therefore
/// advertises both so the dialing peer can race the usable paths.
#[cfg(mobile)]
fn primary_direct_addrs(
    ep: &iroh_http_core::IrohEndpoint,
    fallback_ips: impl IntoIterator<Item = std::net::IpAddr>,
) -> Vec<String> {
    let reconciled = ep.direct_socket_addrs();
    let bound_sockets = ep.bound_sockets();
    select_primary_direct_addrs(&reconciled, &bound_sockets, fallback_ips)
        .into_iter()
        .map(|a| a.to_string())
        .collect()
}

/// Discover every routable IP on an operational mobile interface.
///
/// Interface inventory is required here: asking the kernel for a source address
/// by connecting to an off-link documentation address only exercises a default
/// route, and therefore misses IPv6 ULA/local-only LANs. Keeping all usable
/// interface addresses also preserves the VPN + physical-LAN multi-path case.
#[cfg(target_os = "ios")]
fn mobile_routable_ips<R: tauri::Runtime>(
    _state: &crate::mobile_mdns::MobileMdns<R>,
) -> Vec<std::net::IpAddr> {
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => select_routable_interface_ips(
            interfaces
                .into_iter()
                .map(|interface| (interface.is_oper_up(), interface.ip())),
        ),
        Err(reason) => {
            tracing::warn!(%reason, "iroh-http: failed to enumerate mobile interface addresses");
            Vec::new()
        }
    }
}

#[cfg(target_os = "android")]
fn mobile_routable_ips<R: tauri::Runtime>(
    state: &crate::mobile_mdns::MobileMdns<R>,
) -> Vec<std::net::IpAddr> {
    match state.get_interface_addresses() {
        Ok(addresses) => parse_mobile_interface_ips(addresses),
        Err(reason) => {
            // Interface fallback supplements iroh's own candidates; failure to
            // enumerate it must not prevent endpoint use or relay fallback.
            tracing::warn!(%reason, "iroh-http: native Android interface inventory failed");
            Vec::new()
        }
    }
}

/// Select usable IPs from a platform interface inventory.
///
/// The `(is_operational, ip)` boundary keeps platform collection separate from
/// deterministic policy tests. Enumeration order is preserved and duplicates,
/// loopback, unspecified, link-local, and inactive addresses are removed.
#[cfg(any(mobile, test))]
fn select_routable_interface_ips(
    interfaces: impl IntoIterator<Item = (bool, std::net::IpAddr)>,
) -> Vec<std::net::IpAddr> {
    let mut out = Vec::new();
    for (_, ip) in interfaces
        .into_iter()
        .filter(|(is_operational, _)| *is_operational)
        .filter(|(_, ip)| is_routable_ip(ip))
    {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// Parse native mobile interface literals and apply the same routability and
/// deduplication policy as the iOS Rust-side interface inventory.
#[cfg(any(mobile, test))]
fn parse_mobile_interface_ips(
    addresses: impl IntoIterator<Item = String>,
) -> Vec<std::net::IpAddr> {
    let parsed = addresses.into_iter().filter_map(|literal| match literal.parse() {
        Ok(ip) => Some((true, ip)),
        Err(reason) => {
            tracing::warn!(%literal, %reason, "iroh-http: ignoring invalid native interface address");
            None
        }
    });
    select_routable_interface_ips(parsed)
}

/// Choose the primary direct address to advertise (#346), pure for testing.
///
/// Picks a routable reconciled address, else combines the fallback IPs with a
/// **same-family, real bound QUIC port**. A reconciled candidate with a real
/// port is authoritative and returned unchanged: reflexive QAD ports
/// intentionally differ from the local listener. Only iOS's `:0`/`:1`
/// placeholders borrow from `bound_sockets()`. The borrowed port must belong
/// to the same address family; otherwise the endpoint is not listening at the
/// synthesized address (#350 F14). Returns `None` when no candidate is usable.
#[cfg(any(mobile, test))]
fn select_primary_direct_addr<I>(
    reconciled: &[std::net::SocketAddr],
    bound_sockets: &[std::net::SocketAddr],
    fallback_ips: I,
) -> Option<std::net::SocketAddr>
where
    I: IntoIterator<Item = std::net::IpAddr>,
{
    select_primary_direct_addrs(reconciled, bound_sockets, fallback_ips)
        .into_iter()
        .next()
}

/// All routable direct addresses (#348), pure for testing.
///
/// The plural of [`select_primary_direct_addr`]: every routable real-port
/// candidate is preserved verbatim, while only `:0`/`:1` placeholders borrow a
/// same-family bound QUIC port. Results stay in enumeration order and are
/// deduplicated. Every fallback interface IP is synthesized with a same-family
/// bound port unless that exact candidate is already present; one IPv4 VPN
/// candidate therefore cannot suppress a different IPv4 LAN candidate. One bad
/// candidate never suppresses the others. This is the single policy
/// implementation used by both the singular and plural advertisement APIs.
#[cfg(any(mobile, test))]
fn select_primary_direct_addrs<I>(
    reconciled: &[std::net::SocketAddr],
    bound_sockets: &[std::net::SocketAddr],
    fallback_ips: I,
) -> Vec<std::net::SocketAddr>
where
    I: IntoIterator<Item = std::net::IpAddr>,
{
    let bound_port_for = |want_v6: bool| -> Option<u16> {
        bound_sockets
            .iter()
            .find(|s| s.is_ipv6() == want_v6 && !is_placeholder_port(s.port()))
            .map(|s| s.port())
    };
    let mut out: Vec<std::net::SocketAddr> = Vec::new();
    for a in reconciled
        .iter()
        .copied()
        .filter(|a| is_routable_ip(&a.ip()))
    {
        let candidate = if is_placeholder_port(a.port()) {
            bound_port_for(a.is_ipv6()).map(|port| std::net::SocketAddr::new(a.ip(), port))
        } else {
            Some(a)
        };
        if let Some(sa) = candidate {
            if !out.contains(&sa) {
                out.push(sa);
            }
        }
    }
    for ip in fallback_ips.into_iter().filter(is_routable_ip) {
        if let Some(port) = bound_port_for(ip.is_ipv6()) {
            let candidate = std::net::SocketAddr::new(ip, port);
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

/// Whether `port` is a non-dialable placeholder (`0` unspecified, or `1` — the
/// value iOS was observed enumerating for a local address, never a real QUIC
/// ephemeral port). Mirrors `is_placeholder_port` in `iroh-http-core`.
#[cfg(any(mobile, test))]
fn is_placeholder_port(port: u16) -> bool {
    port == 0 || port == 1
}

/// Whether `ip` is routable off-link: not loopback, unspecified, or link-local.
///
/// A link-local address (IPv4 `169.254.0.0/16`, IPv6 `fe80::/10`) is only valid
/// on its own segment, so advertising it as this node's dialable address makes
/// a browsing peer's direct dial fail (#350). `Ipv6Addr::is_unicast_link_local`
/// is still unstable, so the `fe80::/10` prefix is matched by hand.
#[cfg(any(mobile, test))]
fn is_routable_ip(ip: &std::net::IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    match ip {
        std::net::IpAddr::V4(v4) => !v4.is_link_local(),
        std::net::IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

/// Stop advertising this node on the local network.
#[command]
#[cfg(not(mobile))]
pub fn advertise_peer_close(advertise_handle: u64) {
    #[cfg(feature = "discovery")]
    {
        untrack_discovery(DiscoveryKind::AdvertisePeer, advertise_handle);
        let mut slab = advertise_slab().lock().unwrap_or_else(|e| e.into_inner());
        if slab.contains(advertise_handle as usize) {
            slab.remove(advertise_handle as usize);
        }
    }
}

#[command]
#[cfg(mobile)]
pub fn advertise_peer_close<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    advertise_handle: u64,
) {
    untrack_discovery(DiscoveryKind::AdvertisePeer, advertise_handle);
    let _ = state.advertise_peer_stop(advertise_handle);
}

// ── Generic DNS-SD ─────────────────────────────────────────────────────────────────────────

#[cfg(all(feature = "discovery", not(mobile)))]
type GenericBrowseHandle = Arc<TokioMutex<iroh_http_discovery::ServiceBrowseSession>>;

#[cfg(all(feature = "discovery", not(mobile)))]
fn generic_browse_slab() -> &'static Mutex<slab::Slab<GenericBrowseHandle>> {
    static S: OnceLock<Mutex<slab::Slab<GenericBrowseHandle>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(slab::Slab::new()))
}

/// Mobile generic DNS-SD browse buffer, mirroring the peer path's
/// `mobile_mdns_buffer`: holds records polled from the native layer that have
/// not yet been drained by `browse_next`.
#[cfg(mobile)]
#[allow(clippy::type_complexity)]
fn mobile_dns_sd_buffer() -> &'static Mutex<
    std::collections::HashMap<
        u64,
        std::collections::VecDeque<crate::mobile_mdns::MobileServiceRecord>,
    >,
> {
    static BUFFER: OnceLock<
        Mutex<
            std::collections::HashMap<
                u64,
                std::collections::VecDeque<crate::mobile_mdns::MobileServiceRecord>,
            >,
        >,
    > = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Set of generic browse handles still open, mirroring `mobile_active_browses`.
#[cfg(mobile)]
fn mobile_active_dns_sd_browses() -> &'static Mutex<std::collections::HashSet<u64>> {
    static S: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

#[cfg(mobile)]
fn service_record_from_mobile(r: crate::mobile_mdns::MobileServiceRecord) -> ServiceRecordPayload {
    ServiceRecordPayload {
        is_active: r.is_active,
        service_type: r.service_type,
        instance_name: r.instance_name,
        host: r.host,
        port: r.port,
        addrs: r.addrs,
        txt: r.txt,
    }
}

/// A generic DNS-SD service to advertise, mirroring the shared `ServiceConfig`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceConfigPayload {
    pub service_name: String,
    pub instance_name: String,
    pub port: u16,
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub txt: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub protocol: Option<String>,
}

/// A resolved DNS-SD service record, mirroring the shared `ServiceRecord`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRecordPayload {
    pub is_active: bool,
    pub service_type: String,
    pub instance_name: String,
    pub host: Option<String>,
    pub port: u16,
    pub addrs: Vec<String>,
    pub txt: std::collections::HashMap<String, String>,
}

#[cfg(all(feature = "discovery", not(mobile)))]
fn parse_protocol(p: Option<&str>) -> Result<iroh_http_discovery::Protocol, String> {
    match p.unwrap_or("udp") {
        "udp" => Ok(iroh_http_discovery::Protocol::Udp),
        "tcp" => Ok(iroh_http_discovery::Protocol::Tcp),
        other => Err(format_error_json(
            "INVALID_INPUT",
            format!("invalid protocol: {other:?}"),
        )),
    }
}

/// Advertise a generic DNS-SD service (not tied to an iroh endpoint).
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn advertise(config: ServiceConfigPayload) -> Result<u64, String> {
    let protocol = parse_protocol(config.protocol.as_deref())?;
    let addrs = config
        .addrs
        .iter()
        .map(|s| {
            s.parse::<std::net::IpAddr>()
                .map_err(|e| format_error_json("INVALID_INPUT", format!("invalid address: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let service_config = iroh_http_discovery::ServiceConfig {
        service_name: config.service_name,
        instance_name: config.instance_name,
        port: config.port,
        addrs,
        txt: config.txt.into_iter().collect(),
        protocol,
    };
    let session = iroh_http_discovery::advertise(service_config)
        .map_err(|e| format_error_json("REFUSED", e))?;
    let handle = advertise_slab()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session) as u64;
    track_discovery(DiscoveryKind::GenericAdvertise, handle);
    Ok(handle)
}

#[command]
#[cfg(all(not(feature = "discovery"), not(mobile)))]
pub async fn advertise(_config: ServiceConfigPayload) -> Result<u64, String> {
    Err(format_error_json(
        "UNKNOWN",
        "discovery feature not enabled in this build",
    ))
}

#[command]
#[cfg(mobile)]
pub fn advertise<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    config: ServiceConfigPayload,
) -> Result<u64, String> {
    let protocol = config.protocol.as_deref().unwrap_or("udp");
    let handle = state
        .advertise_start(
            &config.service_name,
            &config.instance_name,
            config.port,
            protocol,
            &config.addrs,
            &config.txt,
        )
        .map_err(|e| format_error_json("REFUSED", e))?;
    track_discovery(DiscoveryKind::GenericAdvertise, handle);
    Ok(handle)
}

/// Stop a generic DNS-SD advertisement.
#[command]
#[cfg(not(mobile))]
pub fn advertise_close(advertise_handle: u64) {
    #[cfg(feature = "discovery")]
    {
        untrack_discovery(DiscoveryKind::GenericAdvertise, advertise_handle);
        let mut slab = advertise_slab().lock().unwrap_or_else(|e| e.into_inner());
        if slab.contains(advertise_handle as usize) {
            slab.remove(advertise_handle as usize);
        }
    }
}

#[command]
#[cfg(mobile)]
pub fn advertise_close<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    advertise_handle: u64,
) {
    untrack_discovery(DiscoveryKind::GenericAdvertise, advertise_handle);
    let _ = state.advertise_stop(advertise_handle);
}

/// Browse for a generic DNS-SD service.
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn browse(service_name: String, protocol: Option<String>) -> Result<u64, String> {
    let protocol = parse_protocol(protocol.as_deref())?;
    let config = iroh_http_discovery::BrowseConfig {
        service_name,
        protocol,
    };
    let session =
        iroh_http_discovery::browse(config).map_err(|e| format_error_json("REFUSED", e))?;
    let handle = generic_browse_slab()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(Arc::new(TokioMutex::new(session))) as u64;
    track_discovery(DiscoveryKind::GenericBrowse, handle);
    Ok(handle)
}

#[command]
#[cfg(all(not(feature = "discovery"), not(mobile)))]
pub async fn browse(_service_name: String, _protocol: Option<String>) -> Result<u64, String> {
    Err(format_error_json(
        "UNKNOWN",
        "discovery feature not enabled in this build",
    ))
}

#[command]
#[cfg(mobile)]
pub fn browse<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    service_name: String,
    protocol: Option<String>,
) -> Result<u64, String> {
    let protocol = protocol.as_deref().unwrap_or("udp");
    let browse_id = state
        .browse_start(&service_name, protocol)
        .map_err(|e| format_error_json("REFUSED", e))?;
    mobile_active_dns_sd_browses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(browse_id);
    track_discovery(DiscoveryKind::GenericBrowse, browse_id);
    Ok(browse_id)
}

/// Poll the next record from a generic DNS-SD browse session.
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn browse_next(browse_handle: u64) -> Result<Option<ServiceRecordPayload>, String> {
    let session = {
        generic_browse_slab()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(browse_handle as usize)
            .cloned()
    }
    .ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid browse handle: {browse_handle}"),
        )
    })?;
    let record = session.lock().await.next_record().await;
    Ok(record.map(|r| ServiceRecordPayload {
        is_active: r.is_active,
        service_type: r.service_type,
        instance_name: r.instance_name,
        host: r.host,
        port: r.port,
        addrs: r.addrs.iter().map(|a| a.to_string()).collect(),
        txt: r.txt.into_iter().collect(),
    }))
}

#[command]
#[cfg(all(not(feature = "discovery"), not(mobile)))]
pub async fn browse_next(_browse_handle: u64) -> Result<Option<ServiceRecordPayload>, String> {
    Err(format_error_json(
        "UNKNOWN",
        "discovery feature not enabled in this build",
    ))
}

#[command]
#[cfg(mobile)]
pub async fn browse_next<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    browse_handle: u64,
) -> Result<Option<ServiceRecordPayload>, String> {
    // The native NsdManager / NWBrowser layer is poll-based and non-blocking
    // (`browse_poll` returns `[]` until a record appears), whereas the
    // shared async iterator treats `None` as "stream finished". Long-poll so
    // `None` keeps its cross-platform meaning — mirrors `browse_peers_next`.
    let buffer = mobile_dns_sd_buffer();
    loop {
        // 1. Drain any record buffered by a previous poll first.
        {
            let mut map = buffer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(queue) = map.get_mut(&browse_handle) {
                if let Some(rec) = queue.pop_front() {
                    return Ok(Some(service_record_from_mobile(rec)));
                }
            }
        }

        // 2. Stop long-polling once the session has been closed.
        let active = mobile_active_dns_sd_browses()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&browse_handle);
        if !active {
            return Ok(None);
        }

        // 3. Poll the native layer for freshly resolved records.
        let mut records = state
            .browse_poll(browse_handle)
            .map_err(|e| format_error_json("INVALID_HANDLE", e))?;
        let first = records.drain(..1.min(records.len())).next();
        if !records.is_empty() {
            let mut map = buffer.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(browse_handle).or_default().extend(records);
        }
        if let Some(rec) = first {
            return Ok(Some(service_record_from_mobile(rec)));
        }

        // 4. Nothing yet — wait briefly, then poll again.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Close a generic DNS-SD browse session.
#[command]
#[cfg(not(mobile))]
pub fn browse_close(browse_handle: u64) {
    #[cfg(feature = "discovery")]
    {
        untrack_discovery(DiscoveryKind::GenericBrowse, browse_handle);
        let mut slab = generic_browse_slab()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if slab.contains(browse_handle as usize) {
            slab.remove(browse_handle as usize);
        }
    }
}

#[command]
#[cfg(mobile)]
pub fn browse_close<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    browse_handle: u64,
) {
    untrack_discovery(DiscoveryKind::GenericBrowse, browse_handle);
    // Retire the handle first so any in-flight `browse_next` long-poll
    // observes the closure and returns `None` (stream finished).
    mobile_active_dns_sd_browses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&browse_handle);
    let _ = state.browse_stop(browse_handle);
    mobile_dns_sd_buffer()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&browse_handle);
}

// ── Transport events ──────────────────────────────────────────────────────────

/// Subscribe to transport-level events (pool hits/misses, path changes, sweeps).
///
/// Spawns a Tokio task that drains the endpoint's event channel and sends each
/// event to `channel`.  The task exits when the endpoint closes (all senders
/// drop) or when the frontend closes the channel.
/// Call at most once per endpoint.
#[command]
pub async fn start_transport_events(
    endpoint_handle: u64,
    channel: Channel<serde_json::Value>,
) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let rx = ep.subscribe_events().ok_or_else(|| {
        format_error_json(
            "ALREADY_STARTED",
            "transport event receiver already taken; startTransportEvents called twice",
        )
    })?;
    tokio::spawn(async move {
        let mut rx = rx;
        while let Some(ev) = rx.recv().await {
            match serde_json::to_value(&ev) {
                Ok(v) => {
                    if channel.send(v).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("iroh-http-tauri: failed to serialise transport event: {e}");
                }
            }
        }
    });
    Ok(())
}

// ── Path-change subscriptions ─────────────────────────────────────────────────

/// One live path-change subscription for a `(endpoint, peer)` pair.
///
/// Mirrors the `PathSub` used by the Node (`src/lib.rs`) and Deno
/// (`src/dispatch.rs`) adapters: it wraps the core watcher's receiver and a
/// `Notify` used to wake a blocked `next_path_change` when the frontend
/// unsubscribes.
struct PathSub {
    rx: tokio::sync::Mutex<
        tokio::sync::mpsc::UnboundedReceiver<iroh_http_core::endpoint::PathInfo>,
    >,
    notify: tokio::sync::Notify,
}

type PathRxMap =
    std::sync::Mutex<std::collections::HashMap<(u64, String), std::sync::Arc<PathSub>>>;

fn path_change_rxs() -> &'static PathRxMap {
    static PATH_CHANGE_RXS: std::sync::OnceLock<PathRxMap> = std::sync::OnceLock::new();
    PATH_CHANGE_RXS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Subscribe to path changes for a specific peer and return the next change.
///
/// Returns the next `PathInfo` as soon as the peer's active path changes, or
/// `null` when the subscription ends (endpoint closed or `unsubscribe`).
/// Call repeatedly to receive successive changes; the endpoint de-duplicates
/// watcher tasks so concurrent calls for the same peer share one Rust task.
/// Mirrors `next_path_change` in the Node and Deno adapters, wiring to
/// `IrohEndpoint::subscribe_path_changes` in `iroh-http-core`.
#[command]
pub async fn next_path_change(
    endpoint_handle: u64,
    node_id: String,
) -> Result<Option<iroh_http_core::endpoint::PathInfo>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;

    let key = (endpoint_handle, node_id.clone());
    let rx_arc = {
        let mut map = path_change_rxs()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(key.clone())
            .or_insert_with(|| {
                let rx = ep.subscribe_path_changes(&node_id);
                std::sync::Arc::new(PathSub {
                    rx: tokio::sync::Mutex::new(rx),
                    notify: tokio::sync::Notify::new(),
                })
            })
            .clone()
    };

    let mut rx = rx_arc.rx.lock().await;
    let result = tokio::select! {
        path = rx.recv() => path,
        () = rx_arc.notify.notified() => None,
    };
    match result {
        None => {
            drop(rx);
            let mut map = path_change_rxs()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if map
                .get(&key)
                .is_some_and(|existing| std::sync::Arc::ptr_eq(existing, &rx_arc))
            {
                map.remove(&key);
            }
            Ok(None)
        }
        Some(path) => Ok(Some(path)),
    }
}

/// Stop an active path-change subscription for a specific peer.
///
/// Wakes any blocked `next_path_change` call for the peer and tears down the
/// core watcher. Mirrors `unsubscribe_path_changes` in the Node and Deno
/// adapters.
#[command]
pub fn unsubscribe_path_changes(endpoint_handle: u64, node_id: String) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    if let Some(sub) = path_change_rxs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(endpoint_handle, node_id.clone()))
    {
        sub.notify.notify_waiters();
    }
    ep.unsubscribe_path_changes(&node_id);
    Ok(())
}

#[cfg(test)]
mod mobile_dns_policy_tests {
    use super::should_query_mobile_dns;
    use iroh_http_core::endpoint::{DiscoveryOptions, NodeOptions};

    #[test]
    fn offline_endpoints_skip_native_dns_but_online_endpoints_do_not() {
        let mut offline = NodeOptions::default();
        offline.networking.disabled = true;
        assert!(!should_query_mobile_dns(&offline));

        let online_without_discovery = NodeOptions {
            discovery: DiscoveryOptions::new(None, false),
            ..Default::default()
        };
        assert!(
            should_query_mobile_dns(&online_without_discovery),
            "online endpoints may still need DNS for relays or proxies"
        );
    }
}

#[cfg(test)]
mod primary_direct_addr_tests {
    use super::{
        parse_mobile_interface_ips, select_primary_direct_addr, select_routable_interface_ips,
    };
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn v4(a: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a[0], a[1], a[2], a[3])), port)
    }

    fn v6(seg: [u16; 8], port: u16) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(std::net::Ipv6Addr::new(
                seg[0], seg[1], seg[2], seg[3], seg[4], seg[5], seg[6], seg[7],
            )),
            port,
        )
    }

    // A single unspecified IPv4 bound socket, the common single-stack case.
    fn bound_v4(port: u16) -> Vec<SocketAddr> {
        vec![v4([0, 0, 0, 0], port)]
    }

    #[test]
    fn interface_fallback_keeps_ula_without_a_default_ipv6_route() {
        let ula = IpAddr::V6("fd12:3456:789a::7".parse().unwrap());
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227));
        let inventory = [
            (true, ula),
            (true, lan),
            (true, IpAddr::V6("fe80::7".parse().unwrap())),
            (false, IpAddr::V6("2001:db8::9".parse().unwrap())),
        ];

        assert_eq!(select_routable_interface_ips(inventory), vec![ula, lan]);
    }

    #[test]
    fn android_native_interface_literals_feed_plural_selector() {
        let vpn = IpAddr::V4(Ipv4Addr::new(10, 12, 222, 17));
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227));
        let ula = IpAddr::V6("fd12:3456:789a::7".parse().unwrap());
        let literals = vec![
            vpn.to_string(),
            lan.to_string(),
            ula.to_string(),
            "fe80::7".to_string(),
            "not-an-ip".to_string(),
            lan.to_string(),
        ];

        assert_eq!(parse_mobile_interface_ips(literals), vec![vpn, lan, ula]);
    }

    #[test]
    fn prefers_routable_reconciled_addr() {
        let reconciled = vec![v4([192, 168, 50, 227], 62546)];
        let out = select_primary_direct_addr(&reconciled, &bound_v4(62546), None);
        assert_eq!(out, Some(v4([192, 168, 50, 227], 62546)));
    }

    #[test]
    fn overrides_bogus_ip_addrs_port_with_real_bound_quic_port() {
        // #346: iOS `ip_addrs()` reports a placeholder port (`:1`), while the
        // real QUIC port lives in `bound_sockets()`. The advertised address must
        // carry the real bound port, not the placeholder.
        let reconciled = vec![v4([192, 168, 50, 227], 1)];
        let out = select_primary_direct_addr(&reconciled, &bound_v4(62546), None);
        assert_eq!(out, Some(v4([192, 168, 50, 227], 62546)));
    }

    #[test]
    fn preserves_real_reflexive_port() {
        // A QAD/reflexive candidate's public port is authoritative even when
        // it differs from the local listener port.
        let reconciled = vec![v4([192, 26, 168, 188], 40349)];
        let out = select_primary_direct_addr(&reconciled, &bound_v4(62546), None);

        assert_eq!(out, Some(v4([192, 26, 168, 188], 40349)));
    }

    #[test]
    fn keeps_reconciled_port_when_no_bound_port_available() {
        // Non-regression: without a bound port to prefer, fall back to the
        // routable reconciled addr's own (real) port rather than dropping it.
        let reconciled = vec![v4([192, 168, 50, 227], 62546)];
        let out = select_primary_direct_addr(&reconciled, &[], None);
        assert_eq!(out, Some(v4([192, 168, 50, 227], 62546)));
    }

    #[test]
    fn drops_placeholder_only_reconciled_without_bound_port() {
        // #350 F5: a reconciled addr whose only port is a placeholder (:1) and
        // no bound port to borrow must yield nothing, not a :1 advertisement.
        let reconciled = vec![v4([192, 168, 50, 227], 1)];
        let out = select_primary_direct_addr(&reconciled, &[], None);
        assert_eq!(out, None);
    }

    #[test]
    fn skips_unrepairable_placeholder_and_keeps_later_real_candidate() {
        // A bad first candidate must not suppress a later dialable one. Here
        // the IPv6 placeholder has no IPv6 listener, while the IPv4 reflexive
        // candidate is already complete and authoritative.
        let reconciled = vec![
            v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 7], 1),
            v4([192, 26, 168, 188], 40349),
        ];

        assert_eq!(
            select_primary_direct_addr(&reconciled, &bound_v4(62546), None),
            Some(v4([192, 26, 168, 188], 40349))
        );
    }

    #[test]
    fn skips_loopback_and_unspecified_reconciled() {
        let reconciled = vec![
            v4([127, 0, 0, 1], 62546),
            v4([0, 0, 0, 0], 62546),
            v4([192, 168, 50, 227], 62546),
        ];
        let out = select_primary_direct_addr(&reconciled, &bound_v4(62546), None);
        assert_eq!(out, Some(v4([192, 168, 50, 227], 62546)));
    }

    #[test]
    fn falls_back_to_local_ip_plus_bound_port_when_no_routable_reconciled() {
        // #346: iOS enumerates nothing usable (empty / port-0-only), but the
        // socket is bound and a routable local IP is discoverable.
        let reconciled: Vec<SocketAddr> = vec![];
        let fallback = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227)));
        let out = select_primary_direct_addr(&reconciled, &bound_v4(62546), fallback);
        assert_eq!(out, Some(v4([192, 168, 50, 227], 62546)));
    }

    #[test]
    fn returns_none_without_a_bound_port() {
        let reconciled: Vec<SocketAddr> = vec![];
        let fallback = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227)));
        assert_eq!(select_primary_direct_addr(&reconciled, &[], fallback), None);
    }

    #[test]
    fn never_synthesises_a_loopback_fallback() {
        let reconciled: Vec<SocketAddr> = vec![];
        let fallback = Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(
            select_primary_direct_addr(&reconciled, &bound_v4(62546), fallback),
            None
        );
    }

    // Regression: #350 F14 — the fallback IP's bound port must be same-family.
    // A dual-stack node with IPv6 listed first must not pair an IPv4 fallback IP
    // with the IPv6 socket's port.
    #[test]
    fn pairs_fallback_ip_with_same_family_bound_port() {
        let reconciled: Vec<SocketAddr> = vec![];
        let fallback = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227)));
        let bound = vec![v6([0, 0, 0, 0, 0, 0, 0, 0], 60000), v4([0, 0, 0, 0], 62546)];
        let out = select_primary_direct_addr(&reconciled, &bound, fallback);
        assert_eq!(out, Some(v4([192, 168, 50, 227], 62546)));
    }

    // Regression: #350 F14 — no same-family bound port for the routable IP →
    // publish nothing rather than a cross-family (undialable) pairing.
    #[test]
    fn returns_none_when_no_same_family_bound_port() {
        let reconciled: Vec<SocketAddr> = vec![];
        let fallback = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227)));
        let bound = vec![v6([0, 0, 0, 0, 0, 0, 0, 0], 60000)];
        assert_eq!(
            select_primary_direct_addr(&reconciled, &bound, fallback),
            None
        );
    }

    // Regression: #350 F5 — a placeholder bound port (:1) must be skipped in
    // favour of a real same-family bound socket.
    #[test]
    fn skips_placeholder_bound_port() {
        let reconciled: Vec<SocketAddr> = vec![];
        let fallback = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227)));
        let bound = vec![v4([0, 0, 0, 0], 1), v4([0, 0, 0, 0], 62546)];
        let out = select_primary_direct_addr(&reconciled, &bound, fallback);
        assert_eq!(out, Some(v4([192, 168, 50, 227], 62546)));
    }

    #[test]
    fn skips_link_local_reconciled_and_fallback() {
        // #350 review L3 — an IPv4 link-local (169.254/16) or IPv6 link-local
        // (fe80::/10) is not routable off-link; advertising it as the dialable
        // address makes the direct dial fail. Prefer a real LAN address.
        let reconciled = vec![v4([169, 254, 1, 5], 62546), v4([192, 168, 50, 227], 62546)];
        let out = select_primary_direct_addr(&reconciled, &bound_v4(62546), None);
        assert_eq!(out, Some(v4([192, 168, 50, 227], 62546)));

        // A link-local-only fallback must not be synthesised into an address.
        let link_local_fallback = Some(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 5)));
        assert_eq!(
            select_primary_direct_addr(&[], &bound_v4(62546), link_local_fallback),
            None
        );
    }

    // Regression: #348 — the device-pass iOS case. iOS enumerated a VPN `10.x`
    // interface first and the real LAN `192.168.x` second; the singular selector
    // advertised only the 10.x address, so LAN peers could not direct-dial and
    // fell back to relay. The plural selector keeps BOTH, so iroh can race the
    // paths and reach the device over the LAN.
    #[test]
    fn plural_keeps_vpn_and_lan_candidates() {
        use super::select_primary_direct_addrs;
        let reconciled = vec![v4([10, 12, 222, 17], 56604), v4([192, 168, 50, 227], 56604)];
        let out = select_primary_direct_addrs(&reconciled, &bound_v4(56604), None);
        assert_eq!(
            out,
            vec![v4([10, 12, 222, 17], 56604), v4([192, 168, 50, 227], 56604)]
        );
    }

    #[test]
    fn plural_interface_fallback_adds_lan_beside_same_family_vpn() {
        use super::select_primary_direct_addrs;
        let vpn = IpAddr::V4(Ipv4Addr::new(10, 12, 222, 17));
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227));
        let reconciled = vec![SocketAddr::new(vpn, 56604)];

        assert_eq!(
            select_primary_direct_addrs(&reconciled, &bound_v4(56604), [vpn, lan]),
            vec![SocketAddr::new(vpn, 56604), SocketAddr::new(lan, 56604)]
        );
    }

    #[test]
    fn plural_interface_fallback_keeps_all_same_family_when_reconciled_is_empty() {
        use super::select_primary_direct_addrs;
        let vpn = IpAddr::V4(Ipv4Addr::new(10, 12, 222, 17));
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227));

        assert_eq!(
            select_primary_direct_addrs(&[], &bound_v4(56604), [vpn, lan]),
            vec![SocketAddr::new(vpn, 56604), SocketAddr::new(lan, 56604)]
        );
    }

    #[test]
    fn plural_preserves_each_real_candidate_port() {
        use super::select_primary_direct_addrs;
        let reconciled = vec![
            v4([192, 168, 50, 227], 62546),
            v4([192, 26, 168, 188], 40349),
        ];

        assert_eq!(
            select_primary_direct_addrs(&reconciled, &bound_v4(62546), None),
            reconciled
        );
    }

    #[test]
    fn plural_adds_ipv6_interface_fallback_when_only_ipv4_was_enumerated() {
        use super::select_primary_direct_addrs;
        let reconciled = vec![v4([192, 168, 50, 227], 62546)];
        let fallback_v6 = IpAddr::V6(std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 42));
        let bound = vec![v4([0, 0, 0, 0], 62546), v6([0, 0, 0, 0, 0, 0, 0, 0], 60000)];

        assert_eq!(
            select_primary_direct_addrs(&reconciled, &bound, [fallback_v6]),
            vec![
                v4([192, 168, 50, 227], 62546),
                SocketAddr::new(fallback_v6, 60000),
            ]
        );
    }

    #[test]
    fn plural_adds_ipv4_interface_fallback_when_only_ipv6_was_enumerated() {
        use super::select_primary_direct_addrs;
        let reconciled = vec![v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 7], 60000)];
        let fallback_v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227));
        let bound = vec![v6([0, 0, 0, 0, 0, 0, 0, 0], 60000), v4([0, 0, 0, 0], 62546)];

        assert_eq!(
            select_primary_direct_addrs(&reconciled, &bound, [fallback_v4]),
            vec![
                v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 7], 60000),
                SocketAddr::new(fallback_v4, 62546),
            ]
        );
    }

    #[test]
    fn plural_reconciles_ports_dedups_and_drops_non_routable() {
        use super::select_primary_direct_addrs;
        // Placeholder ports (:1) borrow the real bound QUIC port; loopback and
        // link-local are dropped; the 10.x placeholder reconciles to the same
        // ip:port as the explicit 10.x entry, so the duplicate collapses.
        let reconciled = vec![
            v4([127, 0, 0, 1], 56604),
            v4([169, 254, 1, 5], 56604),
            v4([10, 12, 222, 17], 1),
            v4([192, 168, 50, 227], 56604),
            v4([10, 12, 222, 17], 56604),
        ];
        let out = select_primary_direct_addrs(&reconciled, &bound_v4(56604), None);
        assert_eq!(
            out,
            vec![v4([10, 12, 222, 17], 56604), v4([192, 168, 50, 227], 56604)]
        );
    }

    #[test]
    fn plural_falls_back_like_singular_when_nothing_routable() {
        use super::select_primary_direct_addrs;
        let fallback = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 227)));
        let out = select_primary_direct_addrs(&[], &bound_v4(62546), fallback);
        assert_eq!(out, vec![v4([192, 168, 50, 227], 62546)]);
        // No routable input and no usable fallback → empty.
        assert!(select_primary_direct_addrs(&[], &[], None).is_empty());
    }
}
