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
                let servers = mdns.get_dns_servers().await.map_err(|e| {
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

    let node_id = ep.node_id().to_string();
    let (handle, replaced) = state::replace_endpoint_for_node_id(node_id.clone(), ep);
    discovery_ownership::activate_endpoint(handle);
    if let Some((old_handle, old_ep)) = replaced {
        tracing::warn!(
            node_id = %node_id,
            old_handle,
            new_handle = handle,
            "iroh-http-tauri: replacing existing endpoint for node id"
        );
        // Retire only sessions owned by the replaced endpoint. Other endpoints
        // and generic DNS-SD sessions in live WebViews remain untouched.
        retire_endpoint_discovery_sessions(&app, old_handle).await;
        old_ep.close_force().await;
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
pub async fn close_endpoint<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    endpoint_handle: u64,
    force: Option<bool>,
) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("node closed or not found (handle {endpoint_handle})"),
        )
    })?;
    // Stop advertisements and retract browse-owned lookup contributions before
    // the transport disappears. Retirement is endpoint-scoped and idempotent.
    retire_endpoint_discovery_sessions(&app, endpoint_handle).await;
    if force.unwrap_or(false) {
        ep.close_force().await;
    } else {
        ep.close().await;
    }
    clear_path_change_rxs(endpoint_handle);
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
/// discovery-crate helper); mobile uses the plural address selector and falls
/// back to the active OS interface inventory when the endpoint enumerates no
/// usable direct address at advertise time (#346).
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
pub async fn discovery_info<R: tauri::Runtime>(
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
    use iroh::Watcher as _;
    let endpoint_addr = ep.raw().watch_addr().get();
    let endpoint_candidates = endpoint_addr.ip_addrs().copied().collect::<Vec<_>>();
    let bound_sockets = ep.bound_sockets();
    let interface_ips = mobile_interface_ips(&state).await;
    let direct_addresses = iroh_http_discovery::peer_direct_addresses(
        &endpoint_candidates,
        &bound_sockets,
        &interface_ips,
    )
    .into_iter()
    .map(|address| address.to_string())
    .collect::<Vec<_>>();
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
    // The close operation may remove the handle before this async command is
    // first polled. An absent endpoint has already reached the requested state.
    if let Some(ep) = state::get_endpoint(endpoint_handle) {
        ep.wait_closed().await;
    }
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
                        ));
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

#[cfg(any(test, mobile, all(feature = "discovery", not(mobile))))]
use std::sync::{Mutex, OnceLock};

#[cfg(any(mobile, all(feature = "discovery", not(mobile))))]
use crate::discovery_handles::DiscoveryHandleMap;
use crate::discovery_ownership;
#[cfg(any(mobile, all(feature = "discovery", not(mobile))))]
use crate::discovery_ownership::DiscoveryKind;
#[cfg(mobile)]
use crate::discovery_ownership::TrackedDiscovery;

#[cfg(any(test, mobile, all(feature = "discovery", not(mobile))))]
use std::sync::Arc;
#[cfg(all(feature = "discovery", not(mobile)))]
type BrowseHandle = Arc<iroh_http_discovery::BrowseSession>;

/// Completion fence for native resources retired by a WebView page advance or
/// destruction. A new context reusing that label waits on this fence before
/// asking the platform to register the same service again.
#[cfg(mobile)]
async fn await_mobile_webview_cleanup_with_timeout(
    webview_label: &str,
    owner: &discovery_ownership::StartLease,
    timeout: std::time::Duration,
) -> Result<(), String> {
    tokio::time::timeout(timeout, owner.wait_for_cleanup())
        .await
        .map_err(|_| {
            format_error_json(
                "REFUSED",
                format!("prior WebView discovery cleanup timed out for {webview_label:?}"),
            )
        })?
        .map_err(|error| {
            format_error_json(
                "REFUSED",
                format!("prior WebView discovery cleanup failed for {webview_label:?}: {error}"),
            )
        })
}

#[cfg(mobile)]
async fn await_mobile_peer_start_barrier_with_timeout(
    webview_label: &str,
    owner: &discovery_ownership::StartLease,
    timeout: std::time::Duration,
) -> Result<(), String> {
    await_mobile_webview_cleanup_with_timeout(webview_label, owner, timeout).await?;
    if !owner.is_current() {
        return Err(format_error_json(
            "REFUSED",
            "discovery owner retired while waiting for prior WebView cleanup",
        ));
    }
    Ok(())
}

#[cfg(mobile)]
async fn await_mobile_generic_start_barrier_with_timeout(
    webview_label: &str,
    owner: &discovery_ownership::StartLease,
    timeout: std::time::Duration,
) -> Result<(), String> {
    await_mobile_webview_cleanup_with_timeout(webview_label, owner, timeout).await?;
    if !owner.is_current() {
        return Err(format_error_json(
            "REFUSED",
            "WebView retired while waiting for prior discovery cleanup",
        ));
    }
    Ok(())
}

#[cfg(mobile)]
async fn await_mobile_peer_start_barrier(
    webview_label: &str,
    owner: &discovery_ownership::StartLease,
) -> Result<(), String> {
    await_mobile_peer_start_barrier_with_timeout(
        webview_label,
        owner,
        std::time::Duration::from_secs(10),
    )
    .await
}

#[cfg(mobile)]
async fn await_mobile_generic_start_barrier(
    webview_label: &str,
    owner: &discovery_ownership::StartLease,
) -> Result<(), String> {
    await_mobile_generic_start_barrier_with_timeout(
        webview_label,
        owner,
        std::time::Duration::from_secs(10),
    )
    .await
}

#[cfg(mobile)]
fn mobile_peer_browse_slab(
) -> &'static Mutex<DiscoveryHandleMap<Arc<iroh_http_discovery::BrowseSession>>> {
    static S: OnceLock<Mutex<DiscoveryHandleMap<Arc<iroh_http_discovery::BrowseSession>>>> =
        OnceLock::new();
    S.get_or_init(|| Mutex::new(DiscoveryHandleMap::default()))
}

#[cfg(mobile)]
struct MobilePeerAdvertiseSession {
    advertisement: Arc<iroh_http_discovery::engine::AdvertisementSession>,
    cancel: tokio::sync::watch::Sender<bool>,
    refresh: Mutex<Option<tauri::async_runtime::JoinHandle<Result<(), String>>>>,
}

#[cfg(mobile)]
fn mobile_peer_advertise_slab(
) -> &'static Mutex<DiscoveryHandleMap<Arc<MobilePeerAdvertiseSession>>> {
    static S: OnceLock<Mutex<DiscoveryHandleMap<Arc<MobilePeerAdvertiseSession>>>> =
        OnceLock::new();
    S.get_or_init(|| Mutex::new(DiscoveryHandleMap::default()))
}

/// Native mobile DNS-SD obtains A/AAAA records from the registered service.
/// Keep iroh's dialable addresses in canonical TXT metadata and use the
/// non-dialable SRV sentinel expected by peer discovery.
#[cfg(any(mobile, test))]
fn mobile_peer_advertisement_update(
    config: &iroh_http_discovery::ServiceConfig,
) -> iroh_http_discovery::engine::AdvertisementUpdate {
    iroh_http_discovery::engine::AdvertisementUpdate {
        port: 1,
        addrs: Vec::new(),
        txt: config.txt.clone(),
    }
}

#[cfg(any(mobile, test))]
async fn mobile_refresh_start_gate(
    published: tokio::sync::oneshot::Receiver<()>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        result = published => result.is_ok(),
        result = cancel.changed() => {
            let _ = result;
            false
        }
    }
}

#[cfg(mobile)]
async fn join_mobile_peer_advertise_refresh(
    task: tauri::async_runtime::JoinHandle<Result<(), String>>,
) -> Result<(), String> {
    task.await
        .map_err(|error| format!("peer advertisement refresh cleanup failed: {error}"))
        .and_then(|result| result)
}

#[cfg(mobile)]
async fn await_mobile_start_reconciliation(task: tokio::task::JoinHandle<Result<(), String>>) {
    match task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "iroh-http-tauri: stale mobile discovery start cleanup failed");
        }
        Err(error) => {
            tracing::warn!(%error, "iroh-http-tauri: stale mobile discovery reconciliation task failed");
        }
    }
}

#[cfg(any(mobile, test))]
type MobileNativeStartCleanup = Box<
    dyn FnOnce(
            u64,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'static>,
        > + Send
        + 'static,
>;

/// A native id that has not yet been claimed by the command waiter.
///
/// Delivery through a oneshot is not sufficient on its own: the sender can
/// succeed immediately before the receiving command is aborted. Keeping the
/// cleanup closure in this drop guard closes that final cancellation window.
#[cfg(any(mobile, test))]
struct DeliveredMobileNativeStart {
    owner: Option<discovery_ownership::StartLease>,
    native_id: u64,
    cleanup: Option<MobileNativeStartCleanup>,
}

#[cfg(any(mobile, test))]
impl DeliveredMobileNativeStart {
    fn claim(mut self) -> (discovery_ownership::StartLease, u64) {
        self.cleanup.take();
        let owner = self
            .owner
            .take()
            .expect("native start owner already claimed");
        (owner, self.native_id)
    }
}

#[cfg(any(mobile, test))]
impl Drop for DeliveredMobileNativeStart {
    fn drop(&mut self) {
        let (Some(owner), Some(cleanup)) = (self.owner.take(), self.cleanup.take()) else {
            return;
        };
        let native_id = self.native_id;
        tokio::spawn(async move {
            let outcome = cleanup(native_id).await;
            owner.finish_cleanup(outcome);
        });
    }
}

/// Run a native start in a detached owner task and transfer its lease only to
/// a live command waiter. If that waiter disappears at any delivery boundary,
/// the returned native id is cleaned before the ownership fence completes.
#[cfg(any(mobile, test))]
async fn detached_mobile_native_start<S, C, CF>(
    mut owner: discovery_ownership::StartLease,
    start: S,
    cleanup: C,
) -> Result<(discovery_ownership::StartLease, u64), String>
where
    S: std::future::Future<Output = Result<u64, String>> + Send + 'static,
    C: FnOnce(u64) -> CF + Send + 'static,
    CF: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    let cleanup: MobileNativeStartCleanup = Box::new(move |native_id| Box::pin(cleanup(native_id)));
    let (delivery, delivered) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        match start.await {
            Ok(native_id) => {
                owner.arm_native_cleanup();
                let pending = DeliveredMobileNativeStart {
                    owner: Some(owner),
                    native_id,
                    cleanup: Some(cleanup),
                };
                let _ = delivery.send(Ok(pending));
            }
            Err(error) => {
                let _ = delivery.send(Err(error));
            }
        }
    });
    let delivered = delivered
        .await
        .map_err(|_| "native discovery start task ended before delivery".to_string())??;
    Ok(delivered.claim())
}

#[cfg(mobile)]
async fn close_mobile_peer_advertisement(
    session: Arc<MobilePeerAdvertiseSession>,
) -> Result<(), String> {
    let _ = session.cancel.send(true);
    session.advertisement.close();
    let refresh = session
        .refresh
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if let Some(refresh) = refresh {
        join_mobile_peer_advertise_refresh(refresh).await
    } else {
        session
            .advertisement
            .wait_closed()
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(all(feature = "discovery", not(mobile)))]
fn browse_slab() -> &'static Mutex<DiscoveryHandleMap<BrowseHandle>> {
    static S: OnceLock<Mutex<DiscoveryHandleMap<BrowseHandle>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(DiscoveryHandleMap::default()))
}

#[cfg(all(feature = "discovery", not(mobile)))]
fn advertise_slab() -> &'static Mutex<DiscoveryHandleMap<iroh_http_discovery::AdvertiseSession>> {
    static S: OnceLock<Mutex<DiscoveryHandleMap<iroh_http_discovery::AdvertiseSession>>> =
        OnceLock::new();
    S.get_or_init(|| Mutex::new(DiscoveryHandleMap::default()))
}

#[cfg(all(feature = "discovery", not(mobile)))]
fn retire_discovery_sessions<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    retirement: discovery_ownership::DiscoveryRetirement,
) {
    for session in retirement.sessions {
        match session.kind {
            DiscoveryKind::BrowsePeers => {
                let mut slab = browse_slab().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(browse) = slab.remove(session.handle) {
                    browse.close();
                }
            }
            // Peer and generic advertise share `advertise_slab`; dropping the
            // session stops the advertisement.
            DiscoveryKind::AdvertisePeer | DiscoveryKind::GenericAdvertise => {
                let mut slab = advertise_slab().lock().unwrap_or_else(|e| e.into_inner());
                slab.remove(session.handle);
            }
            DiscoveryKind::GenericBrowse => {
                let mut slab = generic_browse_slab()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(browse) = slab.remove(session.handle) {
                    browse.close();
                }
            }
        }
    }
}

#[cfg(mobile)]
enum MobileDiscoveryRetirement {
    BrowsePeers(Option<Arc<iroh_http_discovery::BrowseSession>>),
    AdvertisePeer(Option<Arc<MobilePeerAdvertiseSession>>),
    GenericAdvertise(Option<Arc<iroh_http_discovery::engine::AdvertisementSession>>),
    GenericBrowse(Option<Arc<iroh_http_discovery::engine::BrowseSession>>),
}

#[cfg(mobile)]
fn prepare_mobile_discovery_retirement(session: TrackedDiscovery) -> MobileDiscoveryRetirement {
    match session.kind {
        DiscoveryKind::BrowsePeers => {
            let browse = mobile_peer_browse_slab()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(session.handle);
            if let Some(browse) = &browse {
                browse.close();
            }
            MobileDiscoveryRetirement::BrowsePeers(browse)
        }
        DiscoveryKind::AdvertisePeer => {
            let advertisement = mobile_peer_advertise_slab()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(session.handle);
            if let Some(advertisement) = &advertisement {
                let _ = advertisement.cancel.send(true);
                advertisement.advertisement.close();
            }
            MobileDiscoveryRetirement::AdvertisePeer(advertisement)
        }
        DiscoveryKind::GenericAdvertise => {
            let advertisement = mobile_generic_advertise_slab()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(session.handle);
            if let Some(advertisement) = &advertisement {
                advertisement.close();
            }
            MobileDiscoveryRetirement::GenericAdvertise(advertisement)
        }
        DiscoveryKind::GenericBrowse => {
            let browse = mobile_generic_browse_slab()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(session.handle);
            if let Some(browse) = &browse {
                browse.close();
            }
            MobileDiscoveryRetirement::GenericBrowse(browse)
        }
    }
}

#[cfg(mobile)]
async fn complete_mobile_discovery_retirement(
    retirement: MobileDiscoveryRetirement,
) -> Result<(), String> {
    match retirement {
        MobileDiscoveryRetirement::BrowsePeers(browse) => {
            if let Some(browse) = browse {
                browse
                    .wait_closed()
                    .await
                    .map_err(|error| error.to_string())
            } else {
                Ok(())
            }
        }
        MobileDiscoveryRetirement::AdvertisePeer(advertisement) => {
            if let Some(advertisement) = advertisement {
                close_mobile_peer_advertisement(advertisement).await
            } else {
                Ok(())
            }
        }
        MobileDiscoveryRetirement::GenericAdvertise(advertisement) => {
            if let Some(advertisement) = advertisement {
                advertisement
                    .wait_closed()
                    .await
                    .map_err(|error| error.to_string())
            } else {
                Ok(())
            }
        }
        MobileDiscoveryRetirement::GenericBrowse(browse) => {
            if let Some(browse) = browse {
                browse
                    .wait_closed()
                    .await
                    .map_err(|error| error.to_string())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(any(mobile, test))]
async fn finish_terminal_mobile_browse_cleanup(
    cleanup: Option<discovery_ownership::SessionCleanup>,
    native_outcome: Result<(), String>,
) -> Result<(), String> {
    match cleanup {
        Some(cleanup) => cleanup.finish(native_outcome).await,
        None => native_outcome,
    }
}

#[cfg(mobile)]
fn retire_discovery_sessions<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    retirement: discovery_ownership::DiscoveryRetirement,
) {
    let retirements: Vec<_> = retirement
        .sessions
        .into_iter()
        .map(prepare_mobile_discovery_retirement)
        .collect();
    let starts = retirement.starts;
    let cleanups = retirement.owner_cleanups;
    tauri::async_runtime::spawn(async move {
        let mut outcome = Ok(());
        for cleanup in &cleanups {
            if let Err(error) = cleanup.wait_for_previous().await {
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        for retirement in retirements {
            if let Err(error) = complete_mobile_discovery_retirement(retirement).await {
                tracing::warn!(%error, "iroh-http-tauri: mobile discovery retirement cleanup failed");
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        for start in starts {
            if let Err(error) = start.wait().await {
                tracing::warn!(%error, "iroh-http-tauri: in-flight mobile discovery start cleanup failed");
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        for cleanup in cleanups {
            cleanup.finish(outcome.clone());
        }
    });
}

/// Retire one WebView generation immediately in Rust, then serialize its
/// native unregister calls behind any older generation for the same label. The
/// barrier remains visible to new-context start commands until every
/// unregister callback has completed.
#[cfg(mobile)]
fn retire_mobile_webview_generation<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    mut retirement: discovery_ownership::DiscoveryRetirement,
) {
    let retirements: Vec<_> = retirement
        .sessions
        .into_iter()
        .map(prepare_mobile_discovery_retirement)
        .collect();
    let starts = retirement.starts;
    let cleanup = retirement
        .webview_cleanup
        .take()
        .expect("WebView retirement must publish its cleanup fence");
    tauri::async_runtime::spawn(async move {
        let mut outcome = Ok(());
        if let Err(error) = cleanup.wait_for_previous().await {
            outcome = Err(error);
        }
        for retirement in retirements {
            if let Err(error) = complete_mobile_discovery_retirement(retirement).await {
                tracing::warn!(%error, "iroh-http-tauri: WebView discovery cleanup failed");
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        for start in starts {
            if let Err(error) = start.wait().await {
                tracing::warn!(%error, "iroh-http-tauri: stale discovery start cleanup failed");
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        cleanup.finish(outcome);
    });
}

#[cfg(all(not(feature = "discovery"), not(mobile)))]
fn retire_discovery_sessions<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    _sessions: discovery_ownership::DiscoveryRetirement,
) {
}

/// Retire only peer discovery sessions owned by `endpoint_handle`.
pub(crate) async fn retire_endpoint_discovery_sessions<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    endpoint_handle: u64,
) {
    let sessions = discovery_ownership::retire_endpoint(endpoint_handle);
    #[cfg(mobile)]
    {
        let cleanups = sessions.owner_cleanups;
        let retirements: Vec<_> = sessions
            .sessions
            .into_iter()
            .map(prepare_mobile_discovery_retirement)
            .collect();
        let mut outcome = Ok(());
        for cleanup in &cleanups {
            if let Err(error) = cleanup.wait_for_previous().await {
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        for retirement in retirements {
            if let Err(error) = complete_mobile_discovery_retirement(retirement).await {
                tracing::warn!(%error, "iroh-http-tauri: endpoint discovery cleanup failed");
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        for start in sessions.starts {
            if let Err(error) = start.wait().await {
                tracing::warn!(%error, "iroh-http-tauri: endpoint discovery start cleanup failed");
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
        }
        for cleanup in cleanups {
            cleanup.finish(outcome.clone());
        }
    }
    #[cfg(not(mobile))]
    retire_discovery_sessions(_app, sessions);
}

/// Retire every peer/generic discovery session owned by one WebView context.
pub(crate) fn retire_webview_discovery_sessions<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    webview_label: &str,
) {
    let sessions = discovery_ownership::retire_webview(webview_label);
    #[cfg(mobile)]
    retire_mobile_webview_generation(app, sessions);
    #[cfg(not(mobile))]
    retire_discovery_sessions(app, sessions);
}

/// Advance one WebView's page generation and retire the prior JS context.
pub(crate) fn advance_webview_discovery_context<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    webview_label: &str,
) {
    let sessions = discovery_ownership::advance_webview(webview_label);
    #[cfg(mobile)]
    retire_mobile_webview_generation(app, sessions);
    #[cfg(not(mobile))]
    retire_discovery_sessions(app, sessions);
}

/// Defensive last-window cleanup for any owner not already retired.
pub(crate) fn retire_all_discovery_sessions<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    retire_discovery_sessions(app, discovery_ownership::retire_all());
}

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
pub async fn browse_peers<R: tauri::Runtime>(
    webview: tauri::Webview<R>,
    endpoint_handle: u64,
    service_name: String,
) -> Result<u64, String> {
    let owner =
        discovery_ownership::begin_peer(endpoint_handle, webview.label()).ok_or_else(|| {
            format_error_json(
                "INVALID_HANDLE",
                "endpoint or WebView discovery owner is closed",
            )
        })?;
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
        .insert(Arc::new(session))
        .map_err(|error| format_error_json("REFUSED", error))?;
    if discovery_ownership::track_peer(owner, DiscoveryKind::BrowsePeers, handle).is_err() {
        let mut slab = browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(session) = slab.remove(handle) {
            session.close();
        }
        return Err(format_error_json(
            "REFUSED",
            "discovery owner retired while peer browse was starting",
        ));
    }
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
    webview: tauri::Webview<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    endpoint_handle: u64,
    service_name: String,
) -> Result<u64, String> {
    let owner = discovery_ownership::begin_peer_start(endpoint_handle, webview.label())
        .ok_or_else(|| {
            format_error_json(
                "INVALID_HANDLE",
                "endpoint or WebView discovery owner is closed",
            )
        })?;
    await_mobile_peer_start_barrier(webview.label(), &owner).await?;
    let endpoint = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let protocol = iroh_http_discovery::engine::Protocol::Udp;
    let service_type = iroh_http_discovery::engine::service_type(&service_name, protocol)
        .map_err(|error| format_error_json("INVALID_INPUT", error))?;
    let start_mdns = (*state).clone();
    let start_service_name = service_name.clone();
    let native_api = state.inner().clone();
    let cleanup_api = native_api.clone();
    let cleanup_service_type = service_type.clone();
    let (mut owner, native_id) = detached_mobile_native_start(
        owner,
        async move { start_mdns.browse_start(&start_service_name, "udp").await },
        move |native_id| async move {
            let session = iroh_http_discovery::engine::BrowseSession::new(
                crate::mobile_discovery_transport::NativeBrowseHandle::new(
                    cleanup_api,
                    native_id,
                    cleanup_service_type,
                ),
            );
            session.close();
            session
                .wait_closed()
                .await
                .map_err(|error| error.to_string())
        },
    )
    .await
    .map_err(|e| format_error_json("REFUSED", e))?;
    let engine = Arc::new(iroh_http_discovery::engine::BrowseSession::new(
        crate::mobile_discovery_transport::NativeBrowseHandle::new(
            native_api,
            native_id,
            service_type,
        ),
    ));
    let session =
        match iroh_http_discovery::browse_peers_with_engine(endpoint.raw(), Arc::clone(&engine)) {
            Ok(session) => Arc::new(session),
            Err(error) => {
                engine.close();
                let cleanup = owner.reconcile_cleanup(async move {
                    engine
                        .wait_closed()
                        .await
                        .map_err(|error| error.to_string())
                });
                await_mobile_start_reconciliation(cleanup).await;
                return Err(format_error_json("REFUSED", error));
            }
        };
    let handle_result = {
        mobile_peer_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(Arc::clone(&session))
    };
    let handle = match handle_result {
        Ok(handle) => handle,
        Err(error) => {
            session.close();
            let cleanup = owner.reconcile_cleanup(async move {
                session
                    .wait_closed()
                    .await
                    .map_err(|error| error.to_string())
            });
            await_mobile_start_reconciliation(cleanup).await;
            return Err(format_error_json("REFUSED", error));
        }
    };
    if owner.track(DiscoveryKind::BrowsePeers, handle).is_err() {
        mobile_peer_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(handle);
        session.close();
        let cleanup = owner.reconcile_cleanup(async move {
            session
                .wait_closed()
                .await
                .map_err(|error| error.to_string())
        });
        await_mobile_start_reconciliation(cleanup).await;
        return Err(format_error_json(
            "REFUSED",
            "discovery owner retired while peer browse was starting",
        ));
    }
    Ok(handle)
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
            .get(browse_handle)
            .cloned()
    }
    .ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid browse handle: {browse_handle}"),
        )
    })?;
    let event = session.next_event().await;
    if event.is_none() {
        let removed = browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove_if(browse_handle, |current| Arc::ptr_eq(current, &session))
            .is_some();
        if removed {
            // Keep the public handle valid while buffered events drain, then
            // retire it at the same terminal observation returned to JS.
            discovery_ownership::untrack(DiscoveryKind::BrowsePeers, browse_handle);
        }
    }
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
    _state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    browse_handle: u64,
) -> Result<Option<PeerDiscoveryEventPayload>, String> {
    let session = mobile_peer_browse_slab()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(browse_handle)
        .cloned();
    let Some(session) = session else {
        return Ok(None);
    };
    let event = session.next_event().await;
    if event.is_none() {
        let cleanup =
            discovery_ownership::begin_session_cleanup(DiscoveryKind::BrowsePeers, browse_handle);
        let outcome = session
            .wait_closed()
            .await
            .map_err(|error| error.to_string());
        mobile_peer_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove_if(browse_handle, |current| Arc::ptr_eq(current, &session));
        let outcome = finish_terminal_mobile_browse_cleanup(cleanup, outcome).await;
        if let Err(error) = outcome {
            tracing::warn!(%error, "iroh-http-tauri: terminal mobile peer browse cleanup failed");
            return Err(format_error_json(
                "REFUSED",
                format!("terminal mobile peer browse cleanup failed: {error}"),
            ));
        }
    }
    Ok(event.map(|event| PeerDiscoveryEventPayload {
        is_active: event.is_active,
        node_id: event.node_id,
        addrs: event.addrs,
    }))
}

/// Close a browse session, stopping mDNS discovery.
#[command]
#[cfg(not(mobile))]
pub fn browse_peers_close(_browse_handle: u64) {
    #[cfg(feature = "discovery")]
    {
        let browse_handle = _browse_handle;
        let mut slab = browse_slab().lock().unwrap_or_else(|e| e.into_inner());
        if discovery_ownership::untrack(DiscoveryKind::BrowsePeers, browse_handle).is_some() {
            if let Some(session) = slab.remove(browse_handle) {
                session.close();
            }
        }
    }
}

#[command]
#[cfg(mobile)]
pub async fn browse_peers_close<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    _state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    browse_handle: u64,
) -> Result<(), String> {
    let Some(cleanup) =
        discovery_ownership::begin_session_cleanup(DiscoveryKind::BrowsePeers, browse_handle)
    else {
        let session = mobile_peer_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(browse_handle)
            .cloned();
        if let Some(session) = session {
            session.close();
            session.wait_closed().await.map_err(|error| {
                format_error_json(
                    "REFUSED",
                    format!("concurrent mobile peer browse cleanup failed: {error}"),
                )
            })?;
        }
        return Ok(());
    };
    let session = mobile_peer_browse_slab()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(browse_handle);
    if let Some(session) = session {
        session.close();
        let outcome = session
            .wait_closed()
            .await
            .map_err(|error| error.to_string());
        if let Err(error) = cleanup.finish(outcome).await {
            tracing::warn!(%error, "iroh-http-tauri: mobile peer browse cleanup failed");
            return Err(format_error_json(
                "REFUSED",
                format!("mobile peer browse cleanup failed: {error}"),
            ));
        }
    } else if let Err(error) = cleanup.finish(Ok(())).await {
        tracing::warn!(%error, "iroh-http-tauri: mobile peer browse cleanup fence failed");
        return Err(format_error_json(
            "REFUSED",
            format!("mobile peer browse cleanup fence failed: {error}"),
        ));
    }
    Ok(())
}

/// Start advertising this node on the local network via mDNS.
///
/// Declared `async` so Tauri runs setup and the endpoint refresh worker inside
/// the Tokio runtime. A synchronous Tauri command runs on a plain worker thread
/// without the runtime needed by those tasks. Mirrors the Node adapter fix
/// (#243).
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn advertise_peer<R: tauri::Runtime>(
    webview: tauri::Webview<R>,
    endpoint_handle: u64,
    service_name: String,
) -> Result<u64, String> {
    let owner =
        discovery_ownership::begin_peer(endpoint_handle, webview.label()).ok_or_else(|| {
            format_error_json(
                "INVALID_HANDLE",
                "endpoint or WebView discovery owner is closed",
            )
        })?;
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
        .insert(session)
        .map_err(|error| format_error_json("REFUSED", error))?;
    if discovery_ownership::track_peer(owner, DiscoveryKind::AdvertisePeer, handle).is_err() {
        let mut slab = advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        slab.remove(handle);
        return Err(format_error_json(
            "REFUSED",
            "discovery owner retired while peer advertisement was starting",
        ));
    }
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
pub async fn advertise_peer<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    endpoint_handle: u64,
    service_name: String,
) -> Result<u64, String> {
    let owner = discovery_ownership::begin_peer_start(endpoint_handle, webview.label())
        .ok_or_else(|| {
            format_error_json(
                "INVALID_HANDLE",
                "endpoint or WebView discovery owner is closed",
            )
        })?;
    await_mobile_peer_start_barrier(webview.label(), &owner).await?;
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    use iroh::Watcher as _;
    let interface_ips = mobile_interface_ips(&state).await;
    let config =
        iroh_http_discovery::peer_service_config_with_ips(ep.raw(), &service_name, &interface_ips)
            .map_err(|error| format_error_json("REFUSED", error))?;
    let initial = mobile_peer_advertisement_update(&config);
    let txt: std::collections::HashMap<_, _> = config.txt.iter().cloned().collect();
    let no_addrs = Vec::new();
    let start_mdns = (*state).clone();
    let start_service_name = config.service_name.clone();
    let start_instance_name = config.instance_name.clone();
    let native_api = state.inner().clone();
    let cleanup_api = native_api.clone();
    let (mut owner, native_id) = detached_mobile_native_start(
        owner,
        async move {
            start_mdns
                .advertise_start(
                    &start_service_name,
                    &start_instance_name,
                    initial.port,
                    "udp",
                    &no_addrs,
                    &txt,
                )
                .await
        },
        move |native_id| async move {
            let session = iroh_http_discovery::engine::AdvertisementSession::new(
                crate::mobile_discovery_transport::NativeAdvertisementHandle::new(
                    cleanup_api,
                    native_id,
                ),
            );
            session.close();
            session
                .wait_closed()
                .await
                .map_err(|error| error.to_string())
        },
    )
    .await
    .map_err(|error| format_error_json("REFUSED", error))?;
    let advertisement = Arc::new(
        iroh_http_discovery::engine::AdvertisementSession::with_initial(
            crate::mobile_discovery_transport::NativeAdvertisementHandle::new(
                native_api, native_id,
            ),
            initial,
        ),
    );
    let (cancel, mut cancel_rx) = tokio::sync::watch::channel(false);
    let session = Arc::new(MobilePeerAdvertiseSession {
        advertisement: Arc::clone(&advertisement),
        cancel,
        refresh: Mutex::new(None),
    });
    let handle_result = {
        mobile_peer_advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(Arc::clone(&session))
    };
    let handle = match handle_result {
        Ok(handle) => handle,
        Err(error) => {
            advertisement.close();
            let cleanup = owner.reconcile_cleanup(async move {
                advertisement
                    .wait_closed()
                    .await
                    .map_err(|error| error.to_string())
            });
            await_mobile_start_reconciliation(cleanup).await;
            return Err(format_error_json("REFUSED", error));
        }
    };
    let mut watcher = ep.raw().watch_addr();
    let mdns = (*state).clone();
    let refresh_endpoint = ep.clone();
    let refresh_service_name = service_name.clone();
    let refresh_session = Arc::clone(&session);
    let (refresh_start, refresh_start_rx) = tokio::sync::oneshot::channel();
    let refresh = tauri::async_runtime::spawn(async move {
        let started = mobile_refresh_start_gate(refresh_start_rx, &mut cancel_rx).await;
        if started {
            while !*cancel_rx.borrow() {
                let watcher_result = tokio::select! {
                    result = cancel_rx.changed() => {
                        let _ = result;
                        break;
                    }
                    result = watcher.updated() => result,
                };
                if watcher_result.is_err() || *cancel_rx.borrow() {
                    break;
                }
                let interface_ips = mobile_interface_ips(&mdns).await;
                if *cancel_rx.borrow() {
                    break;
                }
                let config = match iroh_http_discovery::peer_service_config_with_ips(
                    refresh_endpoint.raw(),
                    &refresh_service_name,
                    &interface_ips,
                ) {
                    Ok(config) => config,
                    Err(error) => {
                        tracing::warn!(%error, advertise_handle = handle, "iroh-http-tauri: cannot refresh peer advertisement");
                        break;
                    }
                };
                if let Err(error) = refresh_session
                    .advertisement
                    .update(mobile_peer_advertisement_update(&config))
                    .await
                {
                    tracing::warn!(%error, advertise_handle = handle, "iroh-http-tauri: failed to refresh peer advertisement");
                    break;
                }
            }
        }
        let cleanup =
            discovery_ownership::begin_session_cleanup(DiscoveryKind::AdvertisePeer, handle);
        refresh_session.advertisement.close();
        let native_outcome = refresh_session
            .advertisement
            .wait_closed()
            .await
            .map_err(|error| error.to_string());
        let removed = mobile_peer_advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove_if(handle, |current| Arc::ptr_eq(current, &refresh_session))
            .is_some();
        let outcome = if let Some(cleanup) = cleanup {
            cleanup.finish(native_outcome).await
        } else {
            native_outcome
        };
        if let Err(error) = &outcome {
            tracing::warn!(%error, advertise_handle = handle, removed, "iroh-http-tauri: peer advertisement refresh terminal cleanup failed");
        }
        outcome
    });
    *session
        .refresh
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(refresh);
    if owner.track(DiscoveryKind::AdvertisePeer, handle).is_err() {
        mobile_peer_advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(handle);
        drop(refresh_start);
        let cleanup =
            owner.reconcile_cleanup(async move { close_mobile_peer_advertisement(session).await });
        await_mobile_start_reconciliation(cleanup).await;
        return Err(format_error_json(
            "REFUSED",
            "discovery owner retired while peer advertisement was starting",
        ));
    }
    let _ = refresh_start.send(());
    Ok(handle)
}

/// Discover every IP on an operational mobile interface.
///
/// Interface inventory is required here: asking the kernel for a source address
/// by connecting to an off-link documentation address only exercises a default
/// route, and therefore misses IPv6 ULA/local-only LANs. Keeping all usable
/// interface addresses also preserves the VPN + physical-LAN multi-path case.
/// The discovery crate owns routability filtering alongside port selection.
#[cfg(target_os = "ios")]
async fn mobile_interface_ips<R: tauri::Runtime>(
    _state: &crate::mobile_mdns::MobileMdns<R>,
) -> Vec<std::net::IpAddr> {
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => select_operational_interface_ips(
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
async fn mobile_interface_ips<R: tauri::Runtime>(
    state: &crate::mobile_mdns::MobileMdns<R>,
) -> Vec<std::net::IpAddr> {
    match state.get_interface_addresses().await {
        Ok(addresses) => parse_mobile_interface_ips(addresses),
        Err(reason) => {
            // Interface fallback supplements iroh's own candidates; failure to
            // enumerate it must not prevent endpoint use or relay fallback.
            tracing::warn!(%reason, "iroh-http: native Android interface inventory failed");
            Vec::new()
        }
    }
}

/// Select IPs from an operational platform interface inventory.
///
/// The `(is_operational, ip)` seam keeps platform collection separate from
/// deterministic adapter tests. Enumeration order is preserved and duplicates
/// and inactive addresses are removed. Address policy belongs to the discovery
/// crate's canonical peer selector.
#[cfg(any(mobile, test))]
fn select_operational_interface_ips(
    interfaces: impl IntoIterator<Item = (bool, std::net::IpAddr)>,
) -> Vec<std::net::IpAddr> {
    let mut out = Vec::new();
    for (_, ip) in interfaces
        .into_iter()
        .filter(|(is_operational, _)| *is_operational)
    {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// Parse native mobile interface literals and apply the same ordering and
/// deduplication policy as the iOS Rust-side interface inventory.
#[cfg(any(target_os = "android", test))]
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
    select_operational_interface_ips(parsed)
}

/// Stop advertising this node on the local network.
#[command]
#[cfg(not(mobile))]
pub fn advertise_peer_close(_advertise_handle: u64) {
    #[cfg(feature = "discovery")]
    {
        let advertise_handle = _advertise_handle;
        let mut slab = advertise_slab().lock().unwrap_or_else(|e| e.into_inner());
        if discovery_ownership::untrack(DiscoveryKind::AdvertisePeer, advertise_handle).is_some() {
            slab.remove(advertise_handle);
        }
    }
}

#[command]
#[cfg(mobile)]
pub async fn advertise_peer_close<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    _state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    advertise_handle: u64,
) -> Result<(), String> {
    let Some(cleanup) =
        discovery_ownership::begin_session_cleanup(DiscoveryKind::AdvertisePeer, advertise_handle)
    else {
        let session = mobile_peer_advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(advertise_handle)
            .cloned();
        if let Some(session) = session {
            close_mobile_peer_advertisement(session)
                .await
                .map_err(|error| {
                    format_error_json(
                        "REFUSED",
                        format!("concurrent peer advertisement cleanup failed: {error}"),
                    )
                })?;
        }
        return Ok(());
    };
    let session = mobile_peer_advertise_slab()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(advertise_handle);
    if let Some(session) = session {
        let outcome = close_mobile_peer_advertisement(session).await;
        if let Err(error) = cleanup.finish(outcome).await {
            tracing::warn!(%error, "iroh-http-tauri: peer advertisement cleanup failed");
            return Err(format_error_json(
                "REFUSED",
                format!("peer advertisement cleanup failed: {error}"),
            ));
        }
    } else if let Err(error) = cleanup.finish(Ok(())).await {
        tracing::warn!(%error, "iroh-http-tauri: peer advertisement cleanup fence failed");
        return Err(format_error_json(
            "REFUSED",
            format!("peer advertisement cleanup fence failed: {error}"),
        ));
    }
    Ok(())
}

// ── Generic DNS-SD ─────────────────────────────────────────────────────────────────────────

#[cfg(all(feature = "discovery", not(mobile)))]
type GenericBrowseHandle = Arc<iroh_http_discovery::ServiceBrowseSession>;

#[cfg(all(feature = "discovery", not(mobile)))]
fn generic_browse_slab() -> &'static Mutex<DiscoveryHandleMap<GenericBrowseHandle>> {
    static S: OnceLock<Mutex<DiscoveryHandleMap<GenericBrowseHandle>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(DiscoveryHandleMap::default()))
}

#[cfg(mobile)]
fn mobile_generic_browse_slab(
) -> &'static Mutex<DiscoveryHandleMap<Arc<iroh_http_discovery::engine::BrowseSession>>> {
    static S: OnceLock<Mutex<DiscoveryHandleMap<Arc<iroh_http_discovery::engine::BrowseSession>>>> =
        OnceLock::new();
    S.get_or_init(|| Mutex::new(DiscoveryHandleMap::default()))
}

#[cfg(mobile)]
fn mobile_generic_advertise_slab(
) -> &'static Mutex<DiscoveryHandleMap<Arc<iroh_http_discovery::engine::AdvertisementSession>>> {
    static S: OnceLock<
        Mutex<DiscoveryHandleMap<Arc<iroh_http_discovery::engine::AdvertisementSession>>>,
    > = OnceLock::new();
    S.get_or_init(|| Mutex::new(DiscoveryHandleMap::default()))
}

/// A generic DNS-SD service to advertise, mirroring the shared `ServiceConfig`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(all(not(feature = "discovery"), not(mobile)), allow(dead_code))]
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

/// Native DNS-SD APIs own A/AAAA publication and do not accept caller-selected
/// interface addresses. Rejecting the field explicitly avoids pretending the
/// cross-platform configuration was honoured.
#[cfg(any(mobile, test))]
fn reject_mobile_explicit_addrs(addrs: &[String]) -> Result<(), String> {
    if addrs.is_empty() {
        Ok(())
    } else {
        Err(format_error_json(
            "INVALID_INPUT",
            "generic DNS-SD explicit addrs are desktop-only; native mobile adapters select A/AAAA records",
        ))
    }
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

#[cfg(any(mobile, all(feature = "discovery", not(mobile))))]
fn parse_protocol(p: Option<&str>) -> Result<iroh_http_discovery::engine::Protocol, String> {
    match p.unwrap_or("udp") {
        "udp" => Ok(iroh_http_discovery::engine::Protocol::Udp),
        "tcp" => Ok(iroh_http_discovery::engine::Protocol::Tcp),
        other => Err(format_error_json(
            "INVALID_INPUT",
            format!("invalid protocol: {other:?}"),
        )),
    }
}

/// Advertise a generic DNS-SD service (not tied to an iroh endpoint).
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn advertise<R: tauri::Runtime>(
    webview: tauri::Webview<R>,
    config: ServiceConfigPayload,
) -> Result<u64, String> {
    let owner = discovery_ownership::begin_webview(webview.label())
        .ok_or_else(|| format_error_json("INVALID_HANDLE", "WebView discovery owner is closed"))?;
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
        .insert(session)
        .map_err(|error| format_error_json("REFUSED", error))?;
    if discovery_ownership::track_generic(owner, DiscoveryKind::GenericAdvertise, handle).is_err() {
        let mut slab = advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        slab.remove(handle);
        return Err(format_error_json(
            "REFUSED",
            "WebView retired while DNS-SD advertisement was starting",
        ));
    }
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
pub async fn advertise<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    config: ServiceConfigPayload,
) -> Result<u64, String> {
    let owner = discovery_ownership::begin_webview_start(webview.label())
        .ok_or_else(|| format_error_json("INVALID_HANDLE", "WebView discovery owner is closed"))?;
    await_mobile_generic_start_barrier(webview.label(), &owner).await?;
    reject_mobile_explicit_addrs(&config.addrs)?;
    let protocol = parse_protocol(config.protocol.as_deref())?;
    let protocol_label = match protocol {
        iroh_http_discovery::engine::Protocol::Udp => "udp",
        iroh_http_discovery::engine::Protocol::Tcp => "tcp",
    };
    iroh_http_discovery::engine::service_type(&config.service_name, protocol)
        .map_err(|error| format_error_json("INVALID_INPUT", error))?;
    let initial = iroh_http_discovery::engine::AdvertisementUpdate {
        port: config.port,
        addrs: Vec::new(),
        txt: {
            let mut txt: Vec<_> = config
                .txt
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            txt.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            txt
        },
    };
    let start_mdns = (*state).clone();
    let start_service_name = config.service_name.clone();
    let start_instance_name = config.instance_name.clone();
    let start_addrs = config.addrs.clone();
    let start_txt = config.txt.clone();
    let native_api = state.inner().clone();
    let cleanup_api = native_api.clone();
    let (mut owner, native_id) = detached_mobile_native_start(
        owner,
        async move {
            start_mdns
                .advertise_start(
                    &start_service_name,
                    &start_instance_name,
                    config.port,
                    protocol_label,
                    &start_addrs,
                    &start_txt,
                )
                .await
        },
        move |native_id| async move {
            let session = iroh_http_discovery::engine::AdvertisementSession::new(
                crate::mobile_discovery_transport::NativeAdvertisementHandle::new(
                    cleanup_api,
                    native_id,
                ),
            );
            session.close();
            session
                .wait_closed()
                .await
                .map_err(|error| error.to_string())
        },
    )
    .await
    .map_err(|e| format_error_json("REFUSED", e))?;
    let session = Arc::new(
        iroh_http_discovery::engine::AdvertisementSession::with_initial(
            crate::mobile_discovery_transport::NativeAdvertisementHandle::new(
                native_api, native_id,
            ),
            initial,
        ),
    );
    let handle_result = {
        mobile_generic_advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(Arc::clone(&session))
    };
    let handle = match handle_result {
        Ok(handle) => handle,
        Err(error) => {
            session.close();
            let cleanup = owner.reconcile_cleanup(async move {
                session
                    .wait_closed()
                    .await
                    .map_err(|error| error.to_string())
            });
            await_mobile_start_reconciliation(cleanup).await;
            return Err(format_error_json("REFUSED", error));
        }
    };
    if owner
        .track(DiscoveryKind::GenericAdvertise, handle)
        .is_err()
    {
        mobile_generic_advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(handle);
        session.close();
        let cleanup = owner.reconcile_cleanup(async move {
            session
                .wait_closed()
                .await
                .map_err(|error| error.to_string())
        });
        await_mobile_start_reconciliation(cleanup).await;
        return Err(format_error_json(
            "REFUSED",
            "WebView retired while DNS-SD advertisement was starting",
        ));
    }
    Ok(handle)
}

/// Stop a generic DNS-SD advertisement.
#[command]
#[cfg(not(mobile))]
pub fn advertise_close(_advertise_handle: u64) {
    #[cfg(feature = "discovery")]
    {
        let advertise_handle = _advertise_handle;
        let mut slab = advertise_slab().lock().unwrap_or_else(|e| e.into_inner());
        if discovery_ownership::untrack(DiscoveryKind::GenericAdvertise, advertise_handle).is_some()
        {
            slab.remove(advertise_handle);
        }
    }
}

#[command]
#[cfg(mobile)]
pub async fn advertise_close<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    _state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    advertise_handle: u64,
) -> Result<(), String> {
    let Some(cleanup) = discovery_ownership::begin_session_cleanup(
        DiscoveryKind::GenericAdvertise,
        advertise_handle,
    ) else {
        let session = mobile_generic_advertise_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(advertise_handle)
            .cloned();
        if let Some(session) = session {
            session.close();
            session.wait_closed().await.map_err(|error| {
                format_error_json(
                    "REFUSED",
                    format!("concurrent generic mobile advertisement cleanup failed: {error}"),
                )
            })?;
        }
        return Ok(());
    };
    let session = mobile_generic_advertise_slab()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(advertise_handle);
    if let Some(session) = session {
        session.close();
        let outcome = session
            .wait_closed()
            .await
            .map_err(|error| error.to_string());
        if let Err(error) = cleanup.finish(outcome).await {
            tracing::warn!(%error, "iroh-http-tauri: generic mobile advertisement cleanup failed");
            return Err(format_error_json(
                "REFUSED",
                format!("generic mobile advertisement cleanup failed: {error}"),
            ));
        }
    } else if let Err(error) = cleanup.finish(Ok(())).await {
        tracing::warn!(%error, "iroh-http-tauri: generic mobile advertisement cleanup fence failed");
        return Err(format_error_json(
            "REFUSED",
            format!("generic mobile advertisement cleanup fence failed: {error}"),
        ));
    }
    Ok(())
}

/// Browse for a generic DNS-SD service.
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn browse<R: tauri::Runtime>(
    webview: tauri::Webview<R>,
    service_name: String,
    protocol: Option<String>,
) -> Result<u64, String> {
    let owner = discovery_ownership::begin_webview(webview.label())
        .ok_or_else(|| format_error_json("INVALID_HANDLE", "WebView discovery owner is closed"))?;
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
        .insert(Arc::new(session))
        .map_err(|error| format_error_json("REFUSED", error))?;
    if discovery_ownership::track_generic(owner, DiscoveryKind::GenericBrowse, handle).is_err() {
        let mut slab = generic_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(session) = slab.remove(handle) {
            session.close();
        }
        return Err(format_error_json(
            "REFUSED",
            "WebView retired while DNS-SD browse was starting",
        ));
    }
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
pub async fn browse<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    service_name: String,
    protocol: Option<String>,
) -> Result<u64, String> {
    let owner = discovery_ownership::begin_webview_start(webview.label())
        .ok_or_else(|| format_error_json("INVALID_HANDLE", "WebView discovery owner is closed"))?;
    await_mobile_generic_start_barrier(webview.label(), &owner).await?;
    let protocol = parse_protocol(protocol.as_deref())?;
    let protocol_label = match protocol {
        iroh_http_discovery::engine::Protocol::Udp => "udp",
        iroh_http_discovery::engine::Protocol::Tcp => "tcp",
    };
    let service_type = iroh_http_discovery::engine::service_type(&service_name, protocol)
        .map_err(|error| format_error_json("INVALID_INPUT", error))?;
    let start_mdns = (*state).clone();
    let start_service_name = service_name.clone();
    let native_api = state.inner().clone();
    let cleanup_api = native_api.clone();
    let cleanup_service_type = service_type.clone();
    let (mut owner, native_id) = detached_mobile_native_start(
        owner,
        async move {
            start_mdns
                .browse_start(&start_service_name, protocol_label)
                .await
        },
        move |native_id| async move {
            let session = iroh_http_discovery::engine::BrowseSession::new(
                crate::mobile_discovery_transport::NativeBrowseHandle::new(
                    cleanup_api,
                    native_id,
                    cleanup_service_type,
                ),
            );
            session.close();
            session
                .wait_closed()
                .await
                .map_err(|error| error.to_string())
        },
    )
    .await
    .map_err(|e| format_error_json("REFUSED", e))?;
    let session = Arc::new(iroh_http_discovery::engine::BrowseSession::new(
        crate::mobile_discovery_transport::NativeBrowseHandle::new(
            native_api,
            native_id,
            service_type,
        ),
    ));
    let handle_result = {
        mobile_generic_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(Arc::clone(&session))
    };
    let handle = match handle_result {
        Ok(handle) => handle,
        Err(error) => {
            session.close();
            let cleanup = owner.reconcile_cleanup(async move {
                session
                    .wait_closed()
                    .await
                    .map_err(|error| error.to_string())
            });
            await_mobile_start_reconciliation(cleanup).await;
            return Err(format_error_json("REFUSED", error));
        }
    };
    if owner.track(DiscoveryKind::GenericBrowse, handle).is_err() {
        mobile_generic_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(handle);
        session.close();
        let cleanup = owner.reconcile_cleanup(async move {
            session
                .wait_closed()
                .await
                .map_err(|error| error.to_string())
        });
        await_mobile_start_reconciliation(cleanup).await;
        return Err(format_error_json(
            "REFUSED",
            "WebView retired while DNS-SD browse was starting",
        ));
    }
    Ok(handle)
}

/// Poll the next record from a generic DNS-SD browse session.
#[command]
#[cfg(all(feature = "discovery", not(mobile)))]
pub async fn browse_next(browse_handle: u64) -> Result<Option<ServiceRecordPayload>, String> {
    let session = {
        generic_browse_slab()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(browse_handle)
            .cloned()
    }
    .ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid browse handle: {browse_handle}"),
        )
    })?;
    let record = session.next_record().await;
    if record.is_none() {
        let removed = generic_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove_if(browse_handle, |current| Arc::ptr_eq(current, &session))
            .is_some();
        if removed {
            discovery_ownership::untrack(DiscoveryKind::GenericBrowse, browse_handle);
        }
    }
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
    _state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    browse_handle: u64,
) -> Result<Option<ServiceRecordPayload>, String> {
    let session = mobile_generic_browse_slab()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(browse_handle)
        .cloned();
    let Some(session) = session else {
        return Ok(None);
    };
    let event = session.next().await;
    let mut cleanup_error = None;
    if event.as_ref().is_err() || event.as_ref().is_ok_and(|event| event.is_none()) {
        let cleanup =
            discovery_ownership::begin_session_cleanup(DiscoveryKind::GenericBrowse, browse_handle);
        session.close();
        let outcome = session
            .wait_closed()
            .await
            .map_err(|error| error.to_string());
        mobile_generic_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove_if(browse_handle, |current| Arc::ptr_eq(current, &session));
        let outcome = finish_terminal_mobile_browse_cleanup(cleanup, outcome).await;
        if let Err(error) = outcome {
            tracing::warn!(%error, "iroh-http-tauri: terminal generic mobile browse cleanup failed");
            cleanup_error = Some(error);
        }
    }
    let event = event.map_err(|error| format_error_json("REFUSED", error))?;
    if let Some(error) = cleanup_error {
        return Err(format_error_json(
            "REFUSED",
            format!("terminal generic mobile browse cleanup failed: {error}"),
        ));
    }
    Ok(event.map(|event| {
        let record = iroh_http_discovery::engine::ServiceRecord::from(event);
        ServiceRecordPayload {
            is_active: record.is_active,
            service_type: record.service_type,
            instance_name: record.instance_name,
            host: record.host,
            port: record.port,
            addrs: record
                .addrs
                .into_iter()
                .map(|address| address.to_string())
                .collect(),
            txt: record.txt.into_iter().collect(),
        }
    }))
}

/// Close a generic DNS-SD browse session.
#[command]
#[cfg(not(mobile))]
pub fn browse_close(_browse_handle: u64) {
    #[cfg(feature = "discovery")]
    {
        let browse_handle = _browse_handle;
        let mut slab = generic_browse_slab()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if discovery_ownership::untrack(DiscoveryKind::GenericBrowse, browse_handle).is_some() {
            if let Some(session) = slab.remove(browse_handle) {
                session.close();
            }
        }
    }
}

#[command]
#[cfg(mobile)]
pub async fn browse_close<R: tauri::Runtime>(
    _app: tauri::AppHandle<R>,
    _state: tauri::State<'_, crate::mobile_mdns::MobileMdns<R>>,
    browse_handle: u64,
) -> Result<(), String> {
    let Some(cleanup) =
        discovery_ownership::begin_session_cleanup(DiscoveryKind::GenericBrowse, browse_handle)
    else {
        let session = mobile_generic_browse_slab()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(browse_handle)
            .cloned();
        if let Some(session) = session {
            session.close();
            session.wait_closed().await.map_err(|error| {
                format_error_json(
                    "REFUSED",
                    format!("concurrent generic mobile browse cleanup failed: {error}"),
                )
            })?;
        }
        return Ok(());
    };
    let session = mobile_generic_browse_slab()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(browse_handle);
    if let Some(session) = session {
        session.close();
        let outcome = session
            .wait_closed()
            .await
            .map_err(|error| error.to_string());
        if let Err(error) = cleanup.finish(outcome).await {
            tracing::warn!(%error, "iroh-http-tauri: generic mobile browse cleanup failed");
            return Err(format_error_json(
                "REFUSED",
                format!("generic mobile browse cleanup failed: {error}"),
            ));
        }
    } else if let Err(error) = cleanup.finish(Ok(())).await {
        tracing::warn!(%error, "iroh-http-tauri: generic mobile browse cleanup fence failed");
        return Err(format_error_json(
            "REFUSED",
            format!("generic mobile browse cleanup fence failed: {error}"),
        ));
    }
    Ok(())
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

type PathRxMap = std::sync::Mutex<
    std::collections::HashMap<(u64, String, u32), std::sync::Arc<PathSub>>,
>;

fn path_change_rxs() -> &'static PathRxMap {
    static PATH_CHANGE_RXS: std::sync::OnceLock<PathRxMap> = std::sync::OnceLock::new();
    PATH_CHANGE_RXS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn clear_path_change_rxs(endpoint_handle: u64) {
    let mut map = path_change_rxs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.retain(|key, sub| {
        if key.0 == endpoint_handle {
            sub.notify.notify_waiters();
            false
        } else {
            true
        }
    });
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
    subscription_id: u32,
) -> Result<Option<iroh_http_core::endpoint::PathInfo>, String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;

    let key = (endpoint_handle, node_id.clone(), subscription_id);
    let rx_arc = {
        let mut map = path_change_rxs()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(key.clone())
            .or_insert_with(|| {
                let rx = ep.subscribe_path_changes(&node_id, subscription_id);
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
pub fn unsubscribe_path_changes(
    endpoint_handle: u64,
    node_id: String,
    subscription_id: u32,
) -> Result<(), String> {
    let ep = state::get_endpoint(endpoint_handle).ok_or_else(|| {
        format_error_json(
            "INVALID_HANDLE",
            format!("invalid endpoint handle: {endpoint_handle}"),
        )
    })?;
    let key = (endpoint_handle, node_id.clone(), subscription_id);
    {
        let mut map = path_change_rxs()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match map.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let sub = entry.remove();
                sub.notify.notify_waiters();
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
                entry.insert(std::sync::Arc::new(PathSub {
                    rx: tokio::sync::Mutex::new(rx),
                    notify: tokio::sync::Notify::new(),
                }));
            }
        }
    }
    ep.unsubscribe_path_changes(&node_id, subscription_id);
    Ok(())
}

#[cfg(test)]
mod endpoint_lifecycle_tests {
    use super::wait_endpoint_closed;

    #[tokio::test]
    async fn wait_endpoint_closed_treats_a_removed_handle_as_closed() {
        assert!(wait_endpoint_closed(u64::MAX).await.is_ok());
    }
}

#[cfg(test)]
mod mobile_dns_policy_tests {
    use super::{
        detached_mobile_native_start, finish_terminal_mobile_browse_cleanup,
        mobile_peer_advertisement_update, mobile_refresh_start_gate, reject_mobile_explicit_addrs,
        should_query_mobile_dns,
    };
    use iroh_http_core::endpoint::{DiscoveryOptions, NodeOptions};
    use std::{sync::Arc, time::Duration};

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

    #[test]
    fn mobile_generic_advertising_rejects_explicit_addresses() {
        assert!(reject_mobile_explicit_addrs(&[]).is_ok());
        let error = reject_mobile_explicit_addrs(&["192.168.1.2".to_string()])
            .expect_err("native adapters cannot honour explicit A/AAAA records");
        assert!(error.contains("desktop-only"));
    }

    #[test]
    fn mobile_peer_advertising_keeps_peer_metadata_in_rust() {
        let address_txt = "10.8.0.24:59234,192.168.1.24:59234";
        let config = iroh_http_discovery::ServiceConfig {
            service_name: "iroh".to_string(),
            instance_name: "peer".to_string(),
            port: 4433,
            addrs: vec!["192.168.1.2".parse().unwrap()],
            txt: vec![
                ("pk".to_string(), "node".to_string()),
                ("address".to_string(), address_txt.to_string()),
            ],
            protocol: iroh_http_discovery::engine::Protocol::Udp,
        };

        let update = mobile_peer_advertisement_update(&config);

        assert_eq!(update.port, 1);
        assert!(update.addrs.is_empty());
        assert_eq!(update.txt, config.txt);
        assert_eq!(
            update
                .txt
                .iter()
                .find(|(key, _)| key == "address")
                .map(|(_, value)| value.as_str()),
            Some(address_txt),
            "the native SRV sentinel must not erase Rust's mobile fallback candidates"
        );
    }

    #[tokio::test]
    async fn terminal_browse_preserves_native_cleanup_failure_when_retirement_owns_the_fence() {
        assert_eq!(
            finish_terminal_mobile_browse_cleanup(
                None,
                Err("native browse stop failed".to_string())
            )
            .await,
            Err("native browse stop failed".to_string())
        );
    }

    async fn aborted_native_start_is_cleaned_before_fence_completion(
        endpoint_handle: u64,
        label: &'static str,
        native_id: u64,
        cleanup_outcome: Result<(), String>,
    ) -> Result<(), String> {
        crate::discovery_ownership::activate_endpoint(endpoint_handle);
        crate::discovery_ownership::advance_webview(label);
        let owner = crate::discovery_ownership::begin_peer_start(endpoint_handle, label).unwrap();
        let start_release = Arc::new(tokio::sync::Semaphore::new(0));
        let task_start_release = Arc::clone(&start_release);
        let (start_entered_tx, start_entered_rx) = tokio::sync::oneshot::channel();
        let cleanup_release = Arc::new(tokio::sync::Semaphore::new(0));
        let task_cleanup_release = Arc::clone(&cleanup_release);
        let (cleanup_entered_tx, cleanup_entered_rx) = tokio::sync::oneshot::channel();

        let caller = tokio::spawn(detached_mobile_native_start(
            owner,
            async move {
                let _ = start_entered_tx.send(());
                let _ = task_start_release.acquire().await;
                Ok(native_id)
            },
            move |cleaned_id| async move {
                assert_eq!(cleaned_id, native_id);
                let _ = cleanup_entered_tx.send(());
                let _ = task_cleanup_release.acquire().await;
                cleanup_outcome
            },
        ));
        start_entered_rx.await.unwrap();
        caller.abort();
        let _ = caller.await;

        let retirement = crate::discovery_ownership::retire_endpoint(endpoint_handle);
        assert_eq!(retirement.starts.len(), 1);
        let completion = retirement.starts[0].clone();
        start_release.add_permits(1);
        cleanup_entered_rx.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), completion.wait())
                .await
                .is_err(),
            "ownership fence completed before detached native cleanup"
        );
        cleanup_release.add_permits(1);
        completion.wait().await
    }

    #[tokio::test]
    async fn aborted_browse_start_stops_returned_native_id_before_releasing_fence() {
        let _ownership_guard = crate::discovery_ownership::ownership_test_lock()
            .lock()
            .await;
        aborted_native_start_is_cleaned_before_fence_completion(
            u64::MAX - 81,
            "aborted-detached-browse-start",
            8101,
            Ok(()),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn aborted_advertise_start_preserves_native_cleanup_failure() {
        let _ownership_guard = crate::discovery_ownership::ownership_test_lock()
            .lock()
            .await;
        let error = aborted_native_start_is_cleaned_before_fence_completion(
            u64::MAX - 82,
            "aborted-detached-advertise-start",
            8201,
            Err("native advertisement stop failed".to_string()),
        )
        .await
        .expect_err("cleanup error must remain on the ownership fence");
        assert_eq!(error, "native advertisement stop failed");
    }

    #[tokio::test]
    async fn peer_refresh_cannot_observe_terminal_state_before_publication() {
        let (publish, published) = tokio::sync::oneshot::channel();
        let (_cancel, mut cancel_rx) = tokio::sync::watch::channel(false);
        let terminal_observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_terminal = Arc::clone(&terminal_observed);
        let worker = tokio::spawn(async move {
            if mobile_refresh_start_gate(published, &mut cancel_rx).await {
                task_terminal.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });

        tokio::task::yield_now().await;
        assert!(!terminal_observed.load(std::sync::atomic::Ordering::SeqCst));
        publish.send(()).unwrap();
        worker.await.unwrap();
        assert!(terminal_observed.load(std::sync::atomic::Ordering::SeqCst));
    }
}

#[cfg(test)]
mod mobile_interface_inventory_tests {
    use super::{parse_mobile_interface_ips, select_operational_interface_ips};
    use std::net::{IpAddr, Ipv4Addr};

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

        assert_eq!(
            select_operational_interface_ips(inventory),
            vec![ula, lan, "fe80::7".parse().unwrap()]
        );
    }

    #[test]
    fn android_native_interface_literals_filter_and_deduplicate_inventory() {
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

        assert_eq!(
            parse_mobile_interface_ips(literals),
            vec![vpn, lan, ula, "fe80::7".parse().unwrap()]
        );
    }
}
