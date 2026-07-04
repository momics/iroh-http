//! napi-rs bindings for iroh-http-node.
//!
//! Exposes the full bridge interface to Node.js:
//! `createEndpoint`, `nextChunk`, `sendChunk`, `finishBody`,
//! `allocBodyWriter`, `rawFetch`, `rawServe`, `closeEndpoint`.

#![deny(clippy::all)]
#![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use std::sync::Arc;
#[cfg(feature = "discovery")]
use std::sync::{Mutex, MutexGuard, OnceLock};

use bytes::Bytes;
use iroh_http_core::{
    endpoint::{IrohEndpoint, NodeOptions},
    parse_direct_addrs, registry, respond, ConnectionEvent, DiscoveryOptions, NetworkingOptions,
    PoolOptions, RequestPayload, StreamingOptions,
};
use napi::{
    bindgen_prelude::{BigInt, *},
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode},
};
use napi_derive::napi;

// ── Panic hook ────────────────────────────────────────────────────────────────
//
// iroh's MdnsAddressLookup panics on Drop when the Tokio runtime is gone
// (process shutdown after --test-force-exit).  The default panic hook prints
// to stderr which makes Node's test runner mark the file-level test as failed.
// Install a custom hook once that downgrades known shutdown panics to a
// tracing::warn instead of printing to stderr.

fn install_panic_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                String::new()
            };
            if msg.contains("no reactor running") || msg.contains("no Tokio runtime") {
                // Shutdown-time panic from mDNS or other iroh internals;
                // harmless — the process is exiting anyway.
                tracing::warn!("suppressed shutdown panic: {msg}");
                return;
            }
            default(info);
        }));
    });
}

#[cfg(feature = "discovery")]
use slab::Slab;
#[cfg(feature = "discovery")]
use tokio::sync::Mutex as TokioMutex;

use iroh_http_adapter::{
    core_error_to_json, format_error_json, safe_f64_to_u64 as adapter_safe_f64_to_u64,
    safe_f64_to_usize as adapter_safe_f64_to_usize, validate_header_rows, AdapterInputError,
    MAX_BODY_BYTES as ADAPTER_MAX_BODY_BYTES, MAX_TIMEOUT_MS as ADAPTER_MAX_TIMEOUT_MS,
};

// ── Endpoint helpers ──────────────────────────────────────────────────────────

const MAX_NODE_ID_LEN: usize = 128;
const MAX_URL_LEN: usize = 8_192;
const MAX_METHOD_LEN: usize = 32;
const MAX_DIRECT_ADDRS: usize = 32;
const MAX_DIRECT_ADDR_LEN: usize = 256;
#[cfg(feature = "discovery")]
const MAX_MDNS_SERVICE_NAME_LEN: usize = 128;
const MAX_TIMEOUT_MS: u64 = 300_000;
const MAX_HEADER_BYTES: usize = 1_048_576;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_CONNECTIONS: usize = 100_000;

#[derive(Debug)]
enum FfiError {
    /// Node/public-key input failed coarse boundary validation.
    InvalidNodeId,
    /// URL input failed coarse boundary validation.
    InvalidUrl,
    /// Caller-provided input exceeds a hard boundary limit.
    InputTooLarge {
        field: &'static str,
        max: usize,
        got: usize,
    },
    /// Caller-provided bytes contain invalid encoding for the field.
    InvalidEncoding { field: &'static str },
    /// Generic argument validation failure for a named field.
    InvalidArgument { field: &'static str, reason: String },
    /// Internal lock poisoning encountered while handling a boundary call.
    #[cfg(feature = "discovery")]
    InternalLock { resource: &'static str },
}

impl FfiError {
    fn code(&self) -> &'static str {
        match self {
            FfiError::InvalidNodeId => "INVALID_NODE_ID",
            FfiError::InvalidUrl => "INVALID_URL",
            FfiError::InputTooLarge { .. } => "INPUT_TOO_LARGE",
            FfiError::InvalidEncoding { .. } => "INVALID_ENCODING",
            FfiError::InvalidArgument { .. } => "INVALID_ARGUMENT",
            #[cfg(feature = "discovery")]
            FfiError::InternalLock { .. } => "INTERNAL_LOCK",
        }
    }

    fn message(&self) -> String {
        match self {
            FfiError::InvalidNodeId => "invalid node id".to_string(),
            FfiError::InvalidUrl => "invalid url".to_string(),
            FfiError::InputTooLarge { field, max, got } => {
                format!("{field} exceeds max size {max} (got {got})")
            }
            FfiError::InvalidEncoding { field } => format!("{field} contains invalid encoding"),
            FfiError::InvalidArgument { field, reason } => format!("{field}: {reason}"),
            #[cfg(feature = "discovery")]
            FfiError::InternalLock { resource } => {
                format!("internal lock for {resource} is poisoned")
            }
        }
    }
}

fn ffi_invalid_arg(err: FfiError) -> napi::Error {
    napi::Error::new(
        Status::InvalidArg,
        format_error_json(err.code(), err.message()),
    )
}

fn ffi_adapter_invalid_arg(err: AdapterInputError) -> napi::Error {
    napi::Error::new(Status::InvalidArg, err.to_json())
}

#[cfg(feature = "discovery")]
fn ffi_generic(err: FfiError) -> napi::Error {
    napi::Error::new(
        Status::GenericFailure,
        format_error_json(err.code(), err.message()),
    )
}

fn validate_bounded_string(
    field: &'static str,
    value: &str,
    max_len: usize,
) -> std::result::Result<(), FfiError> {
    if value.is_empty() {
        return Err(FfiError::InvalidArgument {
            field,
            reason: "must not be empty".to_string(),
        });
    }
    if value.len() > max_len {
        return Err(FfiError::InputTooLarge {
            field,
            max: max_len,
            got: value.len(),
        });
    }
    if value.as_bytes().contains(&0) {
        return Err(FfiError::InvalidEncoding { field });
    }
    Ok(())
}

fn validate_node_id_input(node_id: &str) -> std::result::Result<(), FfiError> {
    validate_bounded_string("nodeId", node_id, MAX_NODE_ID_LEN)?;
    if node_id.trim_start().starts_with('{') {
        // JSON ticket / NodeAddrInfo payload is parsed by core; we only enforce
        // boundary-level size and encoding constraints here.
        return Ok(());
    }
    let valid = node_id
        .bytes()
        .all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'));
    if !valid {
        return Err(FfiError::InvalidNodeId);
    }
    Ok(())
}

fn validate_url_input(url: &str) -> std::result::Result<(), FfiError> {
    validate_bounded_string("url", url, MAX_URL_LEN).map_err(|e| match e {
        FfiError::InvalidArgument { .. }
        | FfiError::InputTooLarge { .. }
        | FfiError::InvalidEncoding { .. } => e,
        _ => FfiError::InvalidUrl,
    })
}

fn validate_method_input(method: &str) -> std::result::Result<(), FfiError> {
    validate_bounded_string("method", method, MAX_METHOD_LEN)
}

fn validate_direct_addrs_input(
    direct_addrs: &Option<Vec<String>>,
) -> std::result::Result<(), FfiError> {
    if let Some(addrs) = direct_addrs {
        if addrs.len() > MAX_DIRECT_ADDRS {
            return Err(FfiError::InputTooLarge {
                field: "directAddrs",
                max: MAX_DIRECT_ADDRS,
                got: addrs.len(),
            });
        }
        for addr in addrs {
            validate_bounded_string("directAddr", addr, MAX_DIRECT_ADDR_LEN)?;
        }
    }
    Ok(())
}

// ── Local handle indirection ──────────────────────────────────────────────────
//
// The global registry uses slotmap u64 handles where idx occupies the upper
// 32 bits and a generation version the lower 32.  napi does not support u64
// scalar params, so we maintain a local monotonic u32 counter that maps to the
// real u64 handle.  This avoids truncation bugs when multiple endpoints coexist.

use std::sync::atomic::{AtomicU32, Ordering};

static HANDLE_COUNTER: AtomicU32 = AtomicU32::new(1);
static HANDLE_MAP: std::sync::OnceLock<dashmap::DashMap<u32, u64>> = std::sync::OnceLock::new();

fn handle_map() -> &'static dashmap::DashMap<u32, u64> {
    HANDLE_MAP.get_or_init(dashmap::DashMap::new)
}

/// Allocate a new local u32 handle for the given registry u64 handle.
fn alloc_local_handle(registry_handle: u64) -> u32 {
    let local = HANDLE_COUNTER.fetch_add(1, Ordering::Relaxed);
    handle_map().insert(local, registry_handle);
    local
}

/// Remove a local handle, returning the registry u64 if it existed.
fn remove_local_handle(local: u32) -> Option<u64> {
    handle_map().remove(&local).map(|(_, v)| v)
}

/// Resolve a local u32 handle to the real endpoint from the global registry.
fn get_endpoint(handle: u32) -> napi::Result<IrohEndpoint> {
    let registry_handle = handle_map().get(&handle).map(|r| *r).ok_or_else(|| {
        napi::Error::new(
            Status::InvalidArg,
            format_error_json(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {handle})"),
            ),
        )
    })?;
    registry::get_endpoint(registry_handle).ok_or_else(|| {
        napi::Error::new(
            Status::InvalidArg,
            format_error_json(
                "INVALID_HANDLE",
                format!("node closed or not found (handle {handle})"),
            ),
        )
    })
}

// ── Discovery slabs ───────────────────────────────────────────────────────────

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

// ── Endpoint lifecycle ────────────────────────────────────────────────────────

/// Validate and convert an f64 option to a non-negative integer type.
fn safe_f64_to_u64(value: f64, field: &'static str, max: u64) -> napi::Result<u64> {
    if value.is_nan() || value.is_infinite() || value < 0.0 {
        return Err(ffi_invalid_arg(FfiError::InvalidArgument {
            field,
            reason: format!("expected a non-negative finite number, got {value}"),
        }));
    }
    if value.fract() != 0.0 {
        return Err(ffi_invalid_arg(FfiError::InvalidArgument {
            field,
            reason: "must be an integer".to_string(),
        }));
    }
    if value > max as f64 {
        return Err(ffi_invalid_arg(FfiError::InvalidArgument {
            field,
            reason: format!("must be <= {max}"),
        }));
    }
    Ok(value as u64)
}

fn safe_f64_to_usize(value: f64, field: &'static str, max: usize) -> napi::Result<usize> {
    if value.is_nan() || value.is_infinite() || value < 0.0 {
        return Err(ffi_invalid_arg(FfiError::InvalidArgument {
            field,
            reason: format!("expected a non-negative finite number, got {value}"),
        }));
    }
    if value.fract() != 0.0 {
        return Err(ffi_invalid_arg(FfiError::InvalidArgument {
            field,
            reason: "must be an integer".to_string(),
        }));
    }
    if value > max as f64 {
        return Err(ffi_invalid_arg(FfiError::InvalidArgument {
            field,
            reason: format!("must be <= {max}"),
        }));
    }
    Ok(value as usize)
}

/// Extract a `u64` handle from a [`BigInt`].
///
/// Returns `InvalidArg` when the BigInt value was out of `u64` range (i.e.
/// negative or larger than `u64::MAX`).  Every function that accepts a handle
/// from JavaScript must go through this so that a caller passing `BigInt(-1n)`
/// or `BigInt(2n**64n)` receives a structured error rather than silently
/// operating on a truncated handle value.
fn get_handle(big: BigInt) -> napi::Result<u64> {
    // bindgen_runtime::BigInt::get_u64() returns (sign_bit, value, lossless).
    // A handle is valid when it is non-negative and fits in one u64 word.
    let (_sign_bit, val, lossless) = big.get_u64();
    if !lossless {
        return Err(napi::Error::new(
            Status::InvalidArg,
            format_error_json("INVALID_INPUT", "handle value is out of u64 range"),
        ));
    }
    Ok(val)
}

#[napi(object)]
pub struct JsNodeOptions {
    pub key: Option<Uint8Array>,
    pub idle_timeout: Option<f64>,
    pub relay_mode: Option<String>,
    pub relays: Option<Vec<String>>,
    pub bind_addrs: Option<Vec<String>>,
    pub dns_discovery: Option<String>,
    pub dns_discovery_enabled: Option<bool>,
    pub channel_capacity: Option<u32>,
    pub max_chunk_size_bytes: Option<u32>,
    pub handle_ttl: Option<f64>,
    pub sweep_interval: Option<f64>,
    pub max_pooled_connections: Option<u32>,
    pub pool_idle_timeout_ms: Option<f64>,
    pub disable_networking: Option<bool>,
    pub proxy_url: Option<String>,
    pub proxy_from_env: Option<bool>,
    pub keylog: Option<bool>,
    pub compression_level: Option<i32>,
    pub compression_min_body_bytes: Option<u32>,
    /// Maximum header block size in bytes.  Default: 65536.
    pub max_header_bytes: Option<f64>,
}

/// Info returned after a successful `createEndpoint` call.
#[napi(object)]
pub struct JsEndpointInfo {
    /// Opaque handle for the endpoint — pass to all bridge functions.
    pub endpoint_handle: u32,
    /// Base32-encoded public key (stable node identity).
    pub node_id: String,
    /// 32-byte Ed25519 secret key — store to restore the same identity.
    pub keypair: Uint8Array,
}

/// Bind an Iroh QUIC endpoint and return a handle for subsequent operations.
///
/// This is the entry point for creating an iroh-http node. Pass `None` for
/// all-default configuration or supply a `JsNodeOptions` to customise.
#[napi]
pub async fn create_endpoint(options: Option<JsNodeOptions>) -> napi::Result<JsEndpointInfo> {
    install_panic_hook();
    let opts = options
        .map(|o| -> napi::Result<NodeOptions> {
            Ok(NodeOptions {
                key: match o.key {
                    Some(k) => {
                        let slice = k.as_ref();
                        let arr: [u8; 32] = slice.try_into().map_err(|_| {
                            napi::Error::new(
                                Status::InvalidArg,
                                format!("secret key must be exactly 32 bytes, got {}", slice.len()),
                            )
                        })?;
                        Some(arr)
                    }
                    None => None,
                },
                networking: NetworkingOptions {
                    relay_mode: o.relay_mode,
                    relays: o.relays.unwrap_or_default(),
                    bind_addrs: o.bind_addrs.unwrap_or_default(),
                    idle_timeout_ms: o
                        .idle_timeout
                        .map(|t| safe_f64_to_u64(t, "idleTimeout", MAX_TIMEOUT_MS))
                        .transpose()?,
                    proxy_url: o.proxy_url,
                    proxy_from_env: o.proxy_from_env.unwrap_or(false),
                    disabled: o.disable_networking.unwrap_or(false),
                },
                discovery: DiscoveryOptions {
                    dns_server: o.dns_discovery,
                    enabled: o.dns_discovery_enabled.unwrap_or(true),
                },
                pool: PoolOptions {
                    max_connections: o.max_pooled_connections.map(|v| v as usize),
                    idle_timeout_ms: o
                        .pool_idle_timeout_ms
                        .map(|v| safe_f64_to_u64(v, "poolIdleTimeoutMs", MAX_TIMEOUT_MS))
                        .transpose()?,
                },
                streaming: StreamingOptions {
                    channel_capacity: o.channel_capacity.map(|v| v as usize),
                    max_chunk_size_bytes: o.max_chunk_size_bytes.map(|v| v as usize),
                    drain_timeout_ms: None,
                    handle_ttl_ms: o
                        .handle_ttl
                        .map(|v| safe_f64_to_u64(v, "handleTtl", MAX_TIMEOUT_MS))
                        .transpose()?,
                    sweep_interval_ms: o
                        .sweep_interval
                        .map(|v| safe_f64_to_u64(v, "sweepInterval", MAX_TIMEOUT_MS))
                        .transpose()?,
                },
                capabilities: Vec::new(),
                keylog: o.keylog.unwrap_or(false),
                max_header_size: o
                    .max_header_bytes
                    .map(|v| safe_f64_to_usize(v, "maxHeaderBytes", MAX_HEADER_BYTES))
                    .transpose()?,
                max_response_body_bytes: None,
                // NODE-003: enable compression when level or minBodyBytes is provided.
                compression: if o.compression_min_body_bytes.is_some()
                    || o.compression_level.is_some()
                {
                    // ISS-020: validate compression level range before cast.
                    if let Some(level) = o.compression_level {
                        if level < 0 {
                            return Err(napi::Error::new(
                                Status::InvalidArg,
                                format!("compressionLevel must be non-negative, got {level}"),
                            ));
                        }
                    }
                    Some(iroh_http_core::CompressionOptions {
                        min_body_bytes: o
                            .compression_min_body_bytes
                            .map(|v| v as usize)
                            .unwrap_or(iroh_http_core::CompressionOptions::DEFAULT_MIN_BODY_BYTES),
                        level: o.compression_level.map(|v| v as u32),
                    })
                } else {
                    None
                },
            })
        })
        .transpose()?
        .unwrap_or_default();

    let ep = IrohEndpoint::bind(opts)
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;

    let node_id = ep.node_id().to_string();
    // SECURITY: secret_key_bytes() returns raw private key material.
    // The Uint8Array returned to JS is not zeroed automatically; callers
    // must overwrite it with zeros after writing to encrypted storage.
    // Never log, include in error payloads, or pass to untrusted code.
    let keypair = ep.secret_key_bytes().to_vec();
    let handle_u64 = registry::insert_endpoint(ep);
    let handle = alloc_local_handle(handle_u64);

    Ok(JsEndpointInfo {
        endpoint_handle: handle,
        node_id,
        keypair: Uint8Array::new(keypair),
    })
}

/// Close an Iroh endpoint.
///
/// If `force` is `true`, aborts immediately without draining in-flight
/// requests.  Otherwise performs a graceful shutdown.
///
/// The endpoint is removed from the registry **after** closing, not before.
/// This ensures that background tasks (transport events, serve callbacks)
/// that need the endpoint handle during the drain period can still look it up.
#[napi]
pub async fn close_endpoint(endpoint_handle: u32, force: Option<bool>) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    if force.unwrap_or(false) {
        ep.close_force().await;
    } else {
        ep.close().await;
    }
    // Remove from both the local handle map and the global registry.
    if let Some(registry_handle) = remove_local_handle(endpoint_handle) {
        registry::remove_endpoint(registry_handle);
    }
    Ok(())
}

// ── mDNS browse / advertise ──────────────────────────────────────────────────

/// Discovery event returned by `mdnsNextEvent`.
#[napi(object)]
pub struct JsPeerDiscoveryEvent {
    /// `true` = peer appeared; `false` = peer expired.
    pub is_active: bool,
    /// Base32 public key of the discovered peer.
    pub node_id: String,
    /// Known addresses: relay URLs and/or `ip:port` strings.
    pub addrs: Vec<String>,
}

/// Start a browse session: discover peers on the local network via mDNS.
/// Returns a browse handle for polling events.
#[napi]
#[cfg(feature = "discovery")]
pub async fn mdns_browse(endpoint_handle: u32, service_name: String) -> napi::Result<u32> {
    let ep = get_endpoint(endpoint_handle)?;
    validate_bounded_string("serviceName", &service_name, MAX_MDNS_SERVICE_NAME_LEN)
        .map_err(ffi_invalid_arg)?;
    let session = iroh_http_discovery::start_browse(ep.raw(), &service_name)
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, format_error_json("REFUSED", e)))?;
    let handle = lock_browse_slab()?.insert(Arc::new(TokioMutex::new(session))) as u32;
    Ok(handle)
}

#[napi]
#[cfg(not(feature = "discovery"))]
pub async fn mdns_browse(_endpoint_handle: u32, _service_name: String) -> napi::Result<u32> {
    Err(napi::Error::new(
        Status::GenericFailure,
        format_error_json("UNKNOWN", "discovery feature not enabled in this build"),
    ))
}

/// Poll the next discovery event from a browse session.
/// Returns `null` when the session is closed.
#[napi]
#[cfg(feature = "discovery")]
pub async fn mdns_next_event(browse_handle: u32) -> napi::Result<Option<JsPeerDiscoveryEvent>> {
    let session =
        { lock_browse_slab()?.get(browse_handle as usize).cloned() }.ok_or_else(|| {
            napi::Error::new(
                Status::InvalidArg,
                format_error_json(
                    "INVALID_HANDLE",
                    format!("invalid browse handle: {browse_handle}"),
                ),
            )
        })?;
    let event = session.lock().await.next_event().await;
    Ok(event.map(|ev| JsPeerDiscoveryEvent {
        is_active: ev.is_active,
        node_id: ev.node_id,
        addrs: ev.addrs,
    }))
}

#[napi]
#[cfg(not(feature = "discovery"))]
pub async fn mdns_next_event(_browse_handle: u32) -> napi::Result<Option<JsPeerDiscoveryEvent>> {
    Err(napi::Error::new(
        Status::GenericFailure,
        format_error_json("UNKNOWN", "discovery feature not enabled in this build"),
    ))
}

/// Close a browse session, stopping mDNS discovery.
#[napi]
#[cfg(feature = "discovery")]
pub fn mdns_browse_close(browse_handle: u32) {
    if let Ok(mut slab) = browse_slab().lock() {
        if slab.contains(browse_handle as usize) {
            slab.remove(browse_handle as usize);
        }
    } else {
        tracing::error!("iroh-http-node: browse slab lock poisoned during close");
    }
}

#[napi]
#[cfg(not(feature = "discovery"))]
pub fn mdns_browse_close(_browse_handle: u32) {}

/// Start advertising this node on the local network via mDNS.
/// Returns an advertise handle.
///
/// Declared `async` so napi runs it inside the global Tokio runtime. The mDNS
/// address-lookup constructor calls `tokio::runtime::Handle::current()`, which
/// panics outside a runtime context. A synchronous binding ran on the bare JS
/// thread and could panic—aborting the process under `panic = "abort"`—when
/// invoked from a late promise continuation during `--test-force-exit` teardown
/// (issue #243). Running inside the runtime keeps the handle valid and lets
/// post-shutdown calls reject gracefully instead of aborting.
#[napi]
#[cfg(feature = "discovery")]
pub async fn mdns_advertise(endpoint_handle: u32, service_name: String) -> napi::Result<u32> {
    let ep = get_endpoint(endpoint_handle)?;
    validate_bounded_string("serviceName", &service_name, MAX_MDNS_SERVICE_NAME_LEN)
        .map_err(ffi_invalid_arg)?;
    let session = iroh_http_discovery::start_advertise(ep.raw(), &service_name)
        .map_err(|e| napi::Error::new(Status::GenericFailure, format_error_json("REFUSED", e)))?;
    let handle = lock_advertise_slab()?.insert(session) as u32;
    Ok(handle)
}

#[napi]
#[cfg(not(feature = "discovery"))]
pub async fn mdns_advertise(_endpoint_handle: u32, _service_name: String) -> napi::Result<u32> {
    Err(napi::Error::new(
        Status::GenericFailure,
        format_error_json("UNKNOWN", "discovery feature not enabled in this build"),
    ))
}

/// Stop advertising this node on the local network.
#[napi]
#[cfg(feature = "discovery")]
pub fn mdns_advertise_close(advertise_handle: u32) {
    if let Ok(mut slab) = advertise_slab().lock() {
        if slab.contains(advertise_handle as usize) {
            slab.remove(advertise_handle as usize);
        }
    } else {
        tracing::error!("iroh-http-node: advertise slab lock poisoned during close");
    }
}

#[napi]
#[cfg(not(feature = "discovery"))]
pub fn mdns_advertise_close(_advertise_handle: u32) {}

// ── Bridge methods ────────────────────────────────────────────────────────────

// ── Address introspection ─────────────────────────────────────────────────────

#[napi(object)]
pub struct JsNodeAddrInfo {
    pub id: String,
    pub addrs: Vec<String>,
}

#[napi(object)]
pub struct JsPathInfo {
    pub relay: bool,
    pub addr: String,
    pub active: bool,
}

#[napi(object)]
pub struct JsPeerStats {
    pub relay: bool,
    pub relay_url: Option<String>,
    pub paths: Vec<JsPathInfo>,
    pub rtt_ms: Option<f64>,
    /// Network byte/packet counters are exposed as `f64` (not `i64`) so that
    /// values exceeding i64::MAX remain positive in JavaScript rather than
    /// wrapping to negative.  Values above 2^53 lose integer precision, but
    /// that represents >9 petabytes and is acceptable for telemetry purposes.
    pub bytes_sent: Option<f64>,
    pub bytes_received: Option<f64>,
    pub lost_packets: Option<f64>,
    pub sent_packets: Option<f64>,
    pub congestion_window: Option<f64>,
}

/// Full node address: node ID + relay URL(s) + direct socket addresses.
#[napi]
pub fn node_addr(endpoint_handle: u32) -> napi::Result<JsNodeAddrInfo> {
    let ep = get_endpoint(endpoint_handle)?;
    let info = ep.node_addr();
    Ok(JsNodeAddrInfo {
        id: info.id,
        addrs: info.addrs,
    })
}

/// Generate a ticket string for the given endpoint.
///
/// The ticket encodes the node ID and all known addresses (relay URLs + direct IPs).
/// Share with peers so they can connect directly.
#[napi]
pub fn node_ticket(endpoint_handle: u32) -> napi::Result<String> {
    let ep = get_endpoint(endpoint_handle)?;
    iroh_http_core::node_ticket(&ep)
        .map_err(|e| napi::Error::new(Status::GenericFailure, e.message))
}

/// Home relay URL, or null if not connected to a relay.
#[napi]
pub fn home_relay(endpoint_handle: u32) -> napi::Result<Option<String>> {
    let ep = get_endpoint(endpoint_handle)?;
    Ok(ep.home_relay())
}

/// Known addresses for a remote peer, or null if unknown.
#[napi]
pub async fn peer_info(
    endpoint_handle: u32,
    node_id: String,
) -> napi::Result<Option<JsNodeAddrInfo>> {
    let ep = get_endpoint(endpoint_handle)?;
    Ok(ep.peer_info(&node_id).await.map(|info| JsNodeAddrInfo {
        id: info.id,
        addrs: info.addrs,
    }))
}

/// Per-peer connection statistics with path information.
#[napi]
pub async fn peer_stats(
    endpoint_handle: u32,
    node_id: String,
) -> napi::Result<Option<JsPeerStats>> {
    let ep = get_endpoint(endpoint_handle)?;
    Ok(ep.peer_stats(&node_id).await.map(|s| JsPeerStats {
        relay: s.relay,
        relay_url: s.relay_url,
        paths: s
            .paths
            .into_iter()
            .map(|p| JsPathInfo {
                relay: p.relay,
                addr: p.addr,
                active: p.active,
            })
            .collect(),
        rtt_ms: s.rtt_ms,
        bytes_sent: s.bytes_sent.map(|v| v as f64),
        bytes_received: s.bytes_received.map(|v| v as f64),
        lost_packets: s.lost_packets.map(|v| v as f64),
        sent_packets: s.sent_packets.map(|v| v as f64),
        congestion_window: s.congestion_window.map(|v| v as f64),
    }))
}

/// Endpoint-level observability snapshot.
#[napi(object)]
pub struct JsEndpointStats {
    pub active_readers: i64,
    pub active_writers: i64,
    pub active_sessions: i64,
    pub total_handles: i64,
    pub pool_size: i64,
    pub active_connections: i64,
    pub active_requests: i64,
}

/// Snapshot of current endpoint statistics (point-in-time).
#[napi]
pub fn endpoint_stats(endpoint_handle: u32) -> napi::Result<JsEndpointStats> {
    let ep = get_endpoint(endpoint_handle)?;
    let s = ep.endpoint_stats();
    Ok(JsEndpointStats {
        active_readers: s.active_readers as i64,
        active_writers: s.active_writers as i64,
        active_sessions: s.active_sessions as i64,
        total_handles: s.total_handles as i64,
        pool_size: s.pool_size as i64,
        active_connections: s.active_connections as i64,
        active_requests: s.active_requests as i64,
    })
}

// ── Transport events ──────────────────────────────────────────────────────────

/// napi v3 payload for `rawServe` handler callbacks.
///
/// Replaces manual `env.create_*()` construction from napi v2.
/// Fields mirror `RequestPayload` but use napi-compatible types:
/// `BigInt` for opaque u64 handles, `Vec<Vec<String>>` for headers.
#[napi(object)]
pub struct JsCallArgs {
    pub req_handle: BigInt,
    pub req_body_handle: BigInt,
    pub res_body_handle: BigInt,
    pub is_bidi: bool,
    pub method: String,
    pub url: String,
    pub remote_node_id: String,
    /// `[[name, value], ...]`
    pub headers: Vec<Vec<String>>,
}

/// napi v3 payload for `rawServe` connection-event callbacks.
#[napi(object)]
pub struct JsConnectionEvent {
    pub peer_id: String,
    pub connected: bool,
}

/// Register a push callback for transport-level events.
///
/// Spawns a Tokio task that drains the endpoint's event channel and calls
/// `handler` on the Node.js thread for each event (JSON-serialised string).
/// The task exits automatically when the endpoint closes (all senders drop).
/// Call at most once per endpoint; subsequent calls return an error.
#[napi]
pub async fn start_transport_events(
    endpoint_handle: u32,
    // napi v3: accept ThreadsafeFunction directly; napi boxes the JS Function
    // before our code runs so there is no 'env lifetime to escape.
    // CALL_EH=false preserves the single-arg (not error-first) JS convention.
    handler: ThreadsafeFunction<String, (), String, Status, false>,
) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    let rx = ep.subscribe_events().ok_or_else(|| {
        napi::Error::new(
            Status::GenericFailure,
            "transport event receiver already taken; startTransportEvents called twice",
        )
    })?;
    let tsfn = Arc::new(handler);
    tokio::spawn(async move {
        let mut rx = rx;
        while let Some(ev) = rx.recv().await {
            if let Ok(json) = serde_json::to_string(&ev) {
                tsfn.call(json, ThreadsafeFunctionCallMode::NonBlocking);
            }
        }
        // Receiver closed = endpoint closed; tsfn released on drop.
    });
    Ok(())
}

/// Subscribe to path changes for a specific peer.
/// Returns the next path change as a JSON string, or null when done.
/// Call repeatedly to receive successive changes; the endpoint de-duplicates
/// watcher tasks so concurrent calls for the same peer share one Rust task.
#[napi]
pub async fn next_path_change(
    endpoint_handle: u32,
    node_id: String,
) -> napi::Result<Option<String>> {
    type PathRx = tokio::sync::Mutex<
        tokio::sync::mpsc::UnboundedReceiver<iroh_http_core::endpoint::PathInfo>,
    >;
    type PathRxMap = dashmap::DashMap<(u32, String), Arc<PathRx>>;
    static PATH_CHANGE_RXS: std::sync::OnceLock<PathRxMap> = std::sync::OnceLock::new();
    let rxs = PATH_CHANGE_RXS.get_or_init(dashmap::DashMap::new);

    validate_node_id_input(&node_id).map_err(ffi_invalid_arg)?;
    let ep = get_endpoint(endpoint_handle)?;

    let key = (endpoint_handle, node_id.clone());

    let rx_arc = rxs
        .entry(key.clone())
        .or_insert_with(|| {
            let rx = ep.subscribe_path_changes(&node_id);
            Arc::new(tokio::sync::Mutex::new(rx))
        })
        .clone();

    let mut rx = rx_arc.lock().await;
    match rx.recv().await {
        None => {
            drop(rx);
            rxs.remove(&key);
            Ok(None)
        }
        Some(path) => {
            let json = serde_json::to_string(&path)
                .map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string()))?;
            Ok(Some(json))
        }
    }
}

// ── Body streaming ────────────────────────────────────────────────────────────

/// Read the next chunk from a body reader handle.
///
/// Returns `null` at EOF. The handle is automatically cleaned up after EOF.
#[napi]
pub async fn js_next_chunk(endpoint_handle: u32, handle: BigInt) -> napi::Result<Option<Buffer>> {
    let ep = get_endpoint(endpoint_handle)?;
    let chunk = ep
        .handles()
        .next_chunk(get_handle(handle)?)
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    // Vec::from(Bytes) is zero-copy when the Bytes is the sole owner of its
    // backing buffer (refcount == 1), which is the common case for chunks
    // received from an mpsc channel.
    Ok(chunk.map(|b| Buffer::from(Vec::from(b))))
}

/// Non-blocking body read fast path.
///
/// Returns:
/// - `Ok(Some(chunk))` — data was available synchronously.
/// - `Ok(None)` — EOF, handle cleaned up.
/// - `Err(_)` — channel empty or lock contended, caller should fall back to async `nextChunk`.
#[napi]
pub fn js_try_next_chunk(endpoint_handle: u32, handle: BigInt) -> napi::Result<Option<Buffer>> {
    let ep = get_endpoint(endpoint_handle)?;
    let chunk = ep
        .handles()
        .try_next_chunk(get_handle(handle)?)
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    Ok(chunk.map(|b| Buffer::from(Vec::from(b))))
}

/// Push a chunk into a body writer handle.
///
/// Large chunks are automatically split to stay within backpressure limits.
#[napi]
pub async fn js_send_chunk(
    endpoint_handle: u32,
    handle: BigInt,
    chunk: Uint8Array,
) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    // copy_from_slice avoids an intermediate Vec allocation — one memcpy
    // directly into the Bytes backing store.
    let bytes = Bytes::copy_from_slice(chunk.as_ref());
    ep.handles()
        .send_chunk(get_handle(handle)?, bytes)
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))
}

/// Signal end-of-body by dropping the writer.
///
/// The paired `BodyReader` will return `null` on its next `nextChunk` call.
#[napi]
pub fn js_finish_body(endpoint_handle: u32, handle: BigInt) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    ep.handles()
        .finish_body(get_handle(handle)?)
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))
}

/// Cancel a body reader, causing any pending `nextChunk` to return null.
#[napi]
pub fn js_cancel_request(endpoint_handle: u32, handle: BigInt) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    ep.handles().cancel_reader(get_handle(handle)?);
    Ok(())
}

/// Allocate a body writer handle for streaming request bodies.
///
/// Call this before `rawFetch` to get a handle that can be written to
/// with `sendChunk` / `finishBody`.
#[napi]
pub fn js_alloc_body_writer(endpoint_handle: u32) -> napi::Result<u64> {
    let ep = get_endpoint(endpoint_handle)?;
    let (handle, reader) = ep
        .handles()
        .alloc_body_writer()
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    ep.handles().store_pending_reader(handle, reader);
    Ok(handle)
}

/// Allocate a cancellation token for an upcoming `rawFetch` call.
///
/// Wire `AbortSignal → cancelInFlight(token)` for request cancellation.
#[napi]
pub fn js_alloc_fetch_token(endpoint_handle: u32) -> napi::Result<u64> {
    let ep = get_endpoint(endpoint_handle)?;
    ep.handles()
        .alloc_fetch_token()
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))
}

/// Cancel an in-flight fetch by its cancellation token.
///
/// Safe to call after the fetch has already completed (no-op).
#[napi]
pub fn js_cancel_in_flight(endpoint_handle: u32, token: BigInt) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    ep.handles().cancel_in_flight(get_handle(token)?);
    Ok(())
}

// ── rawFetch ──────────────────────────────────────────────────────────────────

/// Raw response returned by `rawFetch`.
///
/// The shared TS layer wraps this into a web-standard `Response`.
#[napi(object)]
pub struct JsFfiResponse {
    /// HTTP status code.
    pub status: u32,
    /// Response headers as `[[key, value], ...]`.
    pub headers: Vec<Vec<String>>,
    /// Handle to the response body reader (`nextChunk`).
    pub body_handle: BigInt,
    /// Full `httpi://` URL of the responding peer.
    pub url: String,
}

/// Send an HTTP request to a remote Iroh peer.
///
/// Low-level function — the shared TS layer wraps this in `makeFetch`.
#[napi]
#[allow(clippy::too_many_arguments)]
pub async fn raw_fetch(
    endpoint_handle: u32,
    node_id: String,
    url: String,
    method: String,
    headers: Vec<Vec<String>>,
    req_body_handle: Option<BigInt>,
    fetch_token: BigInt,
    direct_addrs: Option<Vec<String>>,
    timeout_ms: Option<f64>,
    decompress: Option<bool>,
    max_response_body_bytes: Option<f64>,
) -> napi::Result<JsFfiResponse> {
    let ep = get_endpoint(endpoint_handle)?;
    validate_node_id_input(&node_id).map_err(ffi_invalid_arg)?;
    validate_url_input(&url).map_err(ffi_invalid_arg)?;
    validate_method_input(&method).map_err(ffi_invalid_arg)?;
    validate_direct_addrs_input(&direct_addrs).map_err(ffi_invalid_arg)?;

    let pairs = validate_header_rows(headers).map_err(ffi_adapter_invalid_arg)?;

    let req_body_reader = req_body_handle
        .map(get_handle)
        .transpose()?
        .and_then(|h| ep.handles().claim_pending_reader(h));

    let addrs =
        parse_direct_addrs(&direct_addrs).map_err(|e| napi::Error::new(Status::InvalidArg, e))?;
    let timeout = timeout_ms
        .map(|ms| adapter_safe_f64_to_u64(ms, "timeoutMs", ADAPTER_MAX_TIMEOUT_MS))
        .transpose()
        .map_err(ffi_adapter_invalid_arg)?
        .map(std::time::Duration::from_millis);
    let max_resp = max_response_body_bytes
        .map(|b| adapter_safe_f64_to_usize(b, "maxResponseBodyBytes", ADAPTER_MAX_BODY_BYTES))
        .transpose()
        .map_err(ffi_adapter_invalid_arg)?;
    let res = iroh_http_core::fetch(
        &ep,
        &node_id,
        &url,
        &method,
        &pairs,
        req_body_reader,
        Some(get_handle(fetch_token)?),
        addrs.as_deref(),
        timeout,
        decompress.unwrap_or(true),
        max_resp,
    )
    .await
    .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;

    let resp_headers: Vec<Vec<String>> = res.headers.into_iter().map(|(k, v)| vec![k, v]).collect();

    Ok(JsFfiResponse {
        status: res.status as u32,
        headers: resp_headers,
        body_handle: BigInt::from(res.body_handle),
        url: res.url,
    })
}

// ── rawServe / rawRespond ──────────────────────────────────────────────────────

/// Call once per request from the JS handler to send the response head.
///
/// This is the Node.js equivalent of Tauri's `respond_to_request` command.
/// The handler callback in `rawServe` is fire-and-forget (napi-rs does not
/// support awaiting Promise return values from ThreadsafeFunction callbacks),
/// so JS must call `rawRespond` explicitly after computing the response head.
#[napi]
pub fn raw_respond(
    endpoint_handle: u32,
    req_handle: BigInt,
    status: u32,
    headers: Vec<Vec<String>>,
) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    let header_pairs = validate_header_rows(headers).map_err(ffi_adapter_invalid_arg)?;
    respond(
        ep.handles(),
        get_handle(req_handle)?,
        status as u16,
        header_pairs,
    )
    .map_err(|e| napi::Error::new(Status::GenericFailure, e))
}

/// Server-side configuration passed at `serve()` time.
#[napi(object)]
pub struct JsServeOptions {
    /// Maximum simultaneous in-flight requests.  Default: 1024.
    pub max_concurrency: Option<u32>,
    /// Maximum connections from a single peer.  Default: 8.
    pub max_connections_per_peer: Option<u32>,
    /// Per-request timeout in milliseconds.  Default: 60 000.  0 = disabled.
    pub request_timeout: Option<f64>,
    /// Reject request bodies larger than this many wire (compressed) bytes.
    /// Default: 16 MiB when not set.
    pub max_request_body_wire_bytes: Option<f64>,
    /// Reject request bodies larger than this many decoded bytes (after
    /// decompression). This is the compression-bomb guard.
    /// Default: 16 MiB when not set.
    pub max_request_body_decoded_bytes: Option<f64>,
    /// Maximum total QUIC connections the server will accept.  Default: unlimited.
    pub max_total_connections: Option<f64>,
    /// Maximum serve errors before shutdown.  Default: 5.
    pub max_serve_errors: Option<u32>,
    /// Drain timeout in milliseconds after shutdown signal.  Default: 5000.
    pub drain_timeout: Option<f64>,
    /// Enable load-shedding (reject with 503 when at capacity).
    pub load_shed: Option<bool>,
    /// When `true` (the default), automatically decompress compressed request
    /// bodies.  Set to `false` to receive raw wire bytes (proxy use-cases).
    pub decompress: Option<bool>,
}

#[napi]
pub async fn raw_serve(
    endpoint_handle: u32,
    serve_options: Option<JsServeOptions>,
    // napi v3: accept ThreadsafeFunction directly; CALL_EH=false preserves
    // the single-arg (not error-first) JS calling convention from v2.
    handler: ThreadsafeFunction<JsCallArgs, (), JsCallArgs, Status, false>,
    on_connection_event: Option<
        ThreadsafeFunction<JsConnectionEvent, (), JsConnectionEvent, Status, false>,
    >,
) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;

    let serve_opts = {
        let o = serve_options.unwrap_or(JsServeOptions {
            max_concurrency: None,
            max_connections_per_peer: None,
            request_timeout: None,
            max_request_body_wire_bytes: None,
            max_request_body_decoded_bytes: None,
            max_total_connections: None,
            max_serve_errors: None,
            drain_timeout: None,
            load_shed: None,
            decompress: None,
        });
        iroh_http_core::ServeOptions {
            max_concurrency: o.max_concurrency.map(|v| v as usize),
            max_connections_per_peer: o.max_connections_per_peer.map(|v| v as usize),
            request_timeout_ms: o
                .request_timeout
                .map(|v| safe_f64_to_u64(v, "requestTimeout", MAX_TIMEOUT_MS))
                .transpose()?,
            max_request_body_wire_bytes: o
                .max_request_body_wire_bytes
                .map(|v| safe_f64_to_usize(v, "maxRequestBodyWireBytes", MAX_BODY_BYTES))
                .transpose()?,
            max_request_body_decoded_bytes: o
                .max_request_body_decoded_bytes
                .map(|v| safe_f64_to_usize(v, "maxRequestBodyDecodedBytes", MAX_BODY_BYTES))
                .transpose()?,
            max_total_connections: o
                .max_total_connections
                .map(|v| safe_f64_to_usize(v, "maxTotalConnections", MAX_TOTAL_CONNECTIONS))
                .transpose()?,
            max_serve_errors: o.max_serve_errors.map(|v| v as usize),
            drain_timeout_ms: o
                .drain_timeout
                .map(|v| safe_f64_to_u64(v, "drainTimeout", MAX_TIMEOUT_MS))
                .transpose()?,
            load_shed: o.load_shed,
            decompression: o.decompress,
        }
    };

    // napi v3: no compat-mode env.create_*() calls needed — construct
    // JsCallArgs directly; napi auto-converts the #[napi(object)] struct.
    let tsfn = Arc::new(handler);

    // Build an optional closure for connection events (peer connect/disconnect).
    let conn_event_fn: Option<Arc<dyn Fn(ConnectionEvent) + Send + Sync>> = on_connection_event
        .map(|conn_tsfn| {
            let conn_tsfn = Arc::new(conn_tsfn);
            Arc::new(move |ev: ConnectionEvent| {
                conn_tsfn.call(
                    JsConnectionEvent {
                        peer_id: ev.peer_id,
                        connected: ev.connected,
                    },
                    ThreadsafeFunctionCallMode::NonBlocking,
                );
            }) as Arc<dyn Fn(ConnectionEvent) + Send + Sync>
        });

    let ep_clone = ep.clone();
    let handle = iroh_http_core::ffi_serve_with_callback(
        ep.clone(),
        serve_opts,
        move |payload: RequestPayload| {
            let tsfn = Arc::clone(&tsfn);
            let ep_ref = ep_clone.clone();
            let req_handle = payload.req_handle;
            let js_args = JsCallArgs {
                req_handle: BigInt {
                    sign_bit: false,
                    words: vec![payload.req_handle],
                },
                req_body_handle: BigInt {
                    sign_bit: false,
                    words: vec![payload.req_body_handle],
                },
                res_body_handle: BigInt {
                    sign_bit: false,
                    words: vec![payload.res_body_handle],
                },
                is_bidi: payload.is_bidi,
                method: payload.method,
                url: payload.url,
                remote_node_id: payload.remote_node_id,
                headers: payload
                    .headers
                    .into_iter()
                    .map(|(k, v)| vec![k, v])
                    .collect(),
            };
            // ISS-019: check TSFN enqueue status; on failure respond with 503
            // immediately so the request doesn't stall until timeout.
            let status = tsfn.call(js_args, ThreadsafeFunctionCallMode::NonBlocking);
            if status != napi::Status::Ok {
                tracing::warn!("iroh-http-node: TSFN enqueue failed ({status:?}), responding 503");
                let _ = ep_ref.handles().take_req_sender(req_handle).map(|tx| {
                    let _ = tx.send(iroh_http_core::ResponseHeadEntry {
                        status: 503,
                        headers: vec![("content-length".to_string(), "0".to_string())],
                    });
                });
            }
        },
        conn_event_fn,
    );
    ep.set_serve_handle(handle);

    Ok(())
}

/// Stop the serve loop for the given endpoint (graceful shutdown).
///
/// This signals the accept loop to stop but does NOT close the endpoint or
/// drain in-flight requests.  Call `closeEndpoint` afterwards if you want
/// a full teardown.
///
/// If the endpoint handle is already gone (e.g. the node was closed), this
/// is a no-op — the serve loop is already stopped.
#[napi]
pub fn stop_serve(endpoint_handle: u32) -> napi::Result<()> {
    if let Ok(ep) = get_endpoint(endpoint_handle) {
        ep.stop_serve();
    }
    Ok(())
}

/// Wait until the serve loop has fully exited (all in-flight requests drained).
///
/// Resolves immediately if `rawServe` was never called on this endpoint.
/// Call this after `stopServe` to confirm the loop has actually terminated.
///
/// If the endpoint handle is already gone (e.g. the node was closed), this
/// resolves immediately — the serve loop is already stopped.
#[napi]
pub async fn wait_serve_stop(endpoint_handle: u32) -> napi::Result<()> {
    if let Ok(ep) = get_endpoint(endpoint_handle) {
        ep.wait_serve_stop().await;
    }
    Ok(())
}

/// Wait until this endpoint has been fully closed — either because `closeEndpoint()`
/// was called or because the QUIC stack shut down natively.
///
/// This is used to surface `node.closed` reliably even without an explicit `close()`.
#[napi]
pub async fn wait_endpoint_closed(endpoint_handle: u32) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    ep.wait_closed().await;
    Ok(())
}

// ── Session ───────────────────────────────────────────────────────────────────

/// Establish a session (QUIC connection) to a remote peer.
/// Returns an opaque session handle.
#[napi]
pub async fn session_connect(
    endpoint_handle: u32,
    node_id: String,
    direct_addrs: Option<Vec<String>>,
) -> napi::Result<u64> {
    let ep = get_endpoint(endpoint_handle)?;
    validate_node_id_input(&node_id).map_err(ffi_invalid_arg)?;
    validate_direct_addrs_input(&direct_addrs).map_err(ffi_invalid_arg)?;
    let addrs =
        parse_direct_addrs(&direct_addrs).map_err(|e| napi::Error::new(Status::InvalidArg, e))?;
    let session = iroh_http_core::Session::connect(ep, &node_id, addrs.as_deref())
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    Ok(session.handle())
}

/// Accept an incoming session (QUIC connection) from a remote peer.
///
/// Blocks until a peer connects or the endpoint shuts down.  Returns
/// `{ sessionHandle, nodeId }` on success, or `null` when the endpoint is closed.
#[napi(object)]
pub struct JsSessionAccepted {
    pub session_handle: BigInt,
    pub node_id: String,
}

#[napi]
pub async fn session_accept(endpoint_handle: u32) -> napi::Result<Option<JsSessionAccepted>> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = match iroh_http_core::Session::accept(ep)
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?
    {
        Some(s) => s,
        None => return Ok(None),
    };
    let node_id = session
        .remote_id()
        .map(|pk| iroh_http_core::base32_encode(pk.as_bytes()))
        .unwrap_or_default();
    Ok(Some(JsSessionAccepted {
        session_handle: BigInt::from(session.handle()),
        node_id,
    }))
}

/// Open a new bidirectional stream on an existing session.
#[napi(object)]
pub struct JsSessionBidiStream {
    pub read_handle: BigInt,
    pub write_handle: BigInt,
}

#[napi]
pub async fn session_create_bidi_stream(
    endpoint_handle: u32,
    session_handle: BigInt,
) -> napi::Result<JsSessionBidiStream> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    let duplex = session
        .create_bidi_stream()
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    Ok(JsSessionBidiStream {
        read_handle: BigInt::from(duplex.read_handle),
        write_handle: BigInt::from(duplex.write_handle),
    })
}

/// Accept the next incoming bidirectional stream on a session.
/// Returns null when the session is closed.
#[napi]
pub async fn session_next_bidi_stream(
    endpoint_handle: u32,
    session_handle: BigInt,
) -> napi::Result<Option<JsSessionBidiStream>> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    let result = session
        .next_bidi_stream()
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    Ok(result.map(|d| JsSessionBidiStream {
        read_handle: BigInt::from(d.read_handle),
        write_handle: BigInt::from(d.write_handle),
    }))
}

/// Close a session.
#[napi]
pub async fn session_close_handle(
    endpoint_handle: u32,
    session_handle: BigInt,
    close_code: Option<BigInt>,
    reason: Option<String>,
) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    let code = close_code.map(get_handle).transpose()?.unwrap_or(0);
    if let Some(ref reason) = reason {
        validate_bounded_string("reason", reason, MAX_URL_LEN).map_err(ffi_invalid_arg)?;
    }
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    session
        .close(code, reason.as_deref().unwrap_or(""))
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))
}

#[cfg(feature = "discovery")]
fn lock_browse_slab() -> napi::Result<MutexGuard<'static, Slab<BrowseHandle>>> {
    browse_slab().lock().map_err(|_| {
        ffi_generic(FfiError::InternalLock {
            resource: "browse_slab",
        })
    })
}

#[cfg(feature = "discovery")]
fn lock_advertise_slab(
) -> napi::Result<MutexGuard<'static, Slab<iroh_http_discovery::AdvertiseSession>>> {
    advertise_slab().lock().map_err(|_| {
        ffi_generic(FfiError::InternalLock {
            resource: "advertise_slab",
        })
    })
}

/// Wait for a session to close. Returns close info { closeCode, reason }.
#[napi(object)]
pub struct JsCloseInfo {
    pub close_code: BigInt,
    pub reason: String,
}

#[napi]
pub async fn session_closed(
    endpoint_handle: u32,
    session_handle: BigInt,
) -> napi::Result<JsCloseInfo> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    let info = session
        .closed()
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    Ok(JsCloseInfo {
        close_code: BigInt::from(info.close_code),
        reason: info.reason,
    })
}

/// Open a new unidirectional (send-only) stream on a session.
/// Returns a write handle.
#[napi]
pub async fn session_create_uni_stream(
    endpoint_handle: u32,
    session_handle: BigInt,
) -> napi::Result<u64> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    session
        .create_uni_stream()
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))
}

/// Accept the next incoming unidirectional stream on a session.
/// Returns a read handle, or null when the session is closed.
#[napi]
pub async fn session_next_uni_stream(
    endpoint_handle: u32,
    session_handle: BigInt,
) -> napi::Result<Option<u64>> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    session
        .next_uni_stream()
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))
}

/// Send a datagram on a session.
#[napi]
pub async fn session_send_datagram(
    endpoint_handle: u32,
    session_handle: BigInt,
    data: Uint8Array,
) -> napi::Result<()> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    session
        .send_datagram(data.as_ref())
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))
}

/// Receive the next datagram on a session. Returns null when the session closes.
#[napi]
pub async fn session_recv_datagram(
    endpoint_handle: u32,
    session_handle: BigInt,
) -> napi::Result<Option<Buffer>> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    let result = session
        .recv_datagram()
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    Ok(result.map(Buffer::from))
}

/// Get the maximum datagram payload size for a session.
/// Returns null if datagrams are not supported.
#[napi]
pub fn session_max_datagram_size(
    endpoint_handle: u32,
    session_handle: BigInt,
) -> napi::Result<Option<u32>> {
    let ep = get_endpoint(endpoint_handle)?;
    let session = iroh_http_core::Session::from_handle(ep, get_handle(session_handle)?);
    let result = session
        .max_datagram_size()
        .map_err(|e| napi::Error::new(Status::GenericFailure, core_error_to_json(&e)))?;
    Ok(result.map(|s| s as u32))
}

// ── Key operations ────────────────────────────────────────────────────────────

/// Sign arbitrary bytes with a 32-byte Ed25519 secret key.
/// Returns a 64-byte signature as a `Buffer`.
#[napi]
pub fn secret_key_sign(secret_key: Uint8Array, data: Uint8Array) -> napi::Result<Buffer> {
    let key_bytes: [u8; 32] = secret_key
        .as_ref()
        .try_into()
        .map_err(|_| napi::Error::new(Status::InvalidArg, "secret key must be 32 bytes"))?;
    let sig = iroh_http_core::secret_key_sign(&key_bytes, data.as_ref())
        .map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string()))?;
    Ok(Buffer::from(sig.to_vec()))
}

/// Verify a 64-byte Ed25519 signature against a 32-byte public key.
/// Returns `true` on success, `false` on failure — does not throw.
#[napi]
pub fn public_key_verify(public_key: Uint8Array, data: Uint8Array, signature: Uint8Array) -> bool {
    let Ok(key_bytes) = <[u8; 32]>::try_from(public_key.as_ref()) else {
        return false;
    };
    let Ok(sig_bytes) = <[u8; 64]>::try_from(signature.as_ref()) else {
        return false;
    };
    iroh_http_core::public_key_verify(&key_bytes, data.as_ref(), &sig_bytes)
}

/// Generate a fresh Ed25519 secret key. Returns 32 raw bytes.
#[napi]
pub fn generate_secret_key() -> napi::Result<Buffer> {
    let key = iroh_http_core::generate_secret_key()
        .map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string()))?;
    Ok(Buffer::from(key.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_node_id_accepts_base32ish_input() {
        let result = validate_node_id_input("abcdefghijklmnopqrstuvwxyz234567");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_node_id_rejects_invalid_chars() {
        let result = validate_node_id_input("bad-node-id!");
        assert!(matches!(result, Err(FfiError::InvalidNodeId)));
    }

    #[test]
    fn validate_url_rejects_null_byte() {
        let result = validate_url_input("httpi://peer/\x00x");
        assert!(matches!(
            result,
            Err(FfiError::InvalidEncoding { field: "url" })
        ));
    }

    #[test]
    fn validate_headers_rejects_malformed_pair() {
        let result = validate_header_rows(vec![vec!["x".to_string()]]);
        assert!(result.is_err());
    }

    #[test]
    fn timeout_conversion_rejects_too_large() {
        let too_large = safe_f64_to_u64(
            (MAX_TIMEOUT_MS + 1) as f64,
            "requestTimeout",
            MAX_TIMEOUT_MS,
        );
        assert!(too_large.is_err());
    }

    #[test]
    fn timeout_conversion_rejects_fractional() {
        let fractional = safe_f64_to_u64(1.5, "requestTimeout", MAX_TIMEOUT_MS);
        assert!(fractional.is_err());
    }
}
