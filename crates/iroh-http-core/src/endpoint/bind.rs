//! `IrohEndpoint::bind` constructor and related helpers.
//!
//! Split from `mod.rs` so the facade stays ≤ 200 LoC. All other
//! `IrohEndpoint` methods live in `mod.rs` (accessors), `lifecycle.rs`
//! (close / drain / serve wiring), and `observe.rs` (peer info).
// bind constructs HandleStore directly; endpoint/ is neither mod http nor
// mod ffi, so this allow is correct here (same rationale as mod.rs lines 20-22).
#![allow(clippy::disallowed_types)]

use std::{sync::Arc, time::Duration};

use iroh::{
    address_lookup::{DnsAddressLookup, PkarrPublisher},
    endpoint::{IdleTimeout, QuicTransportConfig},
    Endpoint, RelayMode, SecretKey,
};

use crate::ffi::handles::{HandleStore, StoreConfig};
use crate::http::transport::pool::ConnectionPool;
use crate::{ALPN, ALPN_DUPLEX};

use super::{
    config::NodeOptions, ffi_bridge::FfiBridge, http_runtime::HttpRuntime,
    session_runtime::SessionRuntime, transport::Transport, EndpointInner, IrohEndpoint,
};

impl IrohEndpoint {
    /// Bind an Iroh endpoint with the supplied options.
    pub async fn bind(opts: NodeOptions) -> Result<Self, crate::CoreError> {
        // Validate: if networking is disabled, relay_mode should not be explicitly set to a
        // network-requiring mode.
        if opts.networking.disabled
            && opts
                .networking
                .relay_mode
                .as_deref()
                .is_some_and(|m| !matches!(m, "disabled"))
        {
            return Err(crate::CoreError::invalid_input(
                "networking.disabled is true but relay_mode is set to a non-disabled value; \
                 set relay_mode to \"disabled\" or omit it when networking.disabled is true",
            ));
        }

        let relay_mode = if opts.networking.disabled {
            RelayMode::Disabled
        } else {
            match opts.networking.relay_mode.as_deref() {
                None | Some("default") => RelayMode::Default,
                Some("staging") => RelayMode::Staging,
                Some("disabled") => RelayMode::Disabled,
                Some("custom") => {
                    if opts.networking.relays.is_empty() {
                        return Err(crate::CoreError::invalid_input(
                            "relay_mode \"custom\" requires at least one URL in `relays`",
                        ));
                    }
                    let urls = opts
                        .networking
                        .relays
                        .iter()
                        .map(|u| {
                            u.parse::<iroh::RelayUrl>()
                                .map_err(crate::CoreError::invalid_input)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    RelayMode::custom(urls)
                }
                Some(other) => {
                    return Err(crate::CoreError::invalid_input(format!(
                        "unknown relay_mode: {other}"
                    )))
                }
            }
        };

        // Validate capabilities against known ALPN values.
        for cap in &opts.capabilities {
            if !crate::KNOWN_ALPNS.contains(&cap.as_str()) {
                return Err(crate::CoreError::invalid_input(format!(
                    "unknown ALPN capability: \"{cap}\"; valid values are {:?}",
                    crate::KNOWN_ALPNS,
                )));
            }
        }

        // Validate compression level range (zstd accepts 1–22).
        if let Some(level) = opts.compression.as_ref().and_then(|c| c.level) {
            if !(1..=22).contains(&level) {
                return Err(crate::CoreError::invalid_input(format!(
                    "compression level must be 1–22, got {level}"
                )));
            }
        }

        let alpns: Vec<Vec<u8>> = if opts.capabilities.is_empty() {
            // Advertise both ALPN variants.
            vec![ALPN_DUPLEX.to_vec(), ALPN.to_vec()]
        } else {
            let mut list: Vec<Vec<u8>> = opts
                .capabilities
                .iter()
                .map(|c| c.as_bytes().to_vec())
                .collect();
            // Always include the base protocol so the node can talk to base-only peers.
            if !list.iter().any(|a| a == ALPN) {
                list.push(ALPN.to_vec());
            }
            list
        };

        let mut builder = Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(relay_mode)
            .alpns(alpns);

        // DNS discovery (enabled by default unless networking.disabled).
        if !opts.networking.disabled && opts.discovery.enabled {
            if let Some(ref url_str) = opts.discovery.dns_server {
                let url: url::Url = url_str.parse().map_err(|e| {
                    crate::CoreError::invalid_input(format!("invalid dns_discovery URL: {e}"))
                })?;
                builder = builder
                    .address_lookup(PkarrPublisher::builder(url.clone()))
                    .address_lookup(DnsAddressLookup::builder(
                        url.host_str().unwrap_or_default().to_string(),
                    ));
            } else {
                builder = builder
                    .address_lookup(PkarrPublisher::n0_dns())
                    .address_lookup(DnsAddressLookup::n0_dns());
            }
        }

        if let Some(key_bytes) = opts.key {
            builder = builder.secret_key(SecretKey::from_bytes(&key_bytes));
        }

        if let Some(ms) = opts.networking.idle_timeout_ms {
            let timeout = IdleTimeout::try_from(Duration::from_millis(ms)).map_err(|e| {
                crate::CoreError::invalid_input(format!("idle_timeout_ms out of range: {e}"))
            })?;
            let transport = QuicTransportConfig::builder()
                .max_idle_timeout(Some(timeout))
                .max_concurrent_bidi_streams(128u32.into())
                .build();
            builder = builder.transport_config(transport);
        } else {
            let transport = QuicTransportConfig::builder()
                .max_concurrent_bidi_streams(128u32.into())
                .build();
            builder = builder.transport_config(transport);
        }

        // Bind address(es).
        for addr_str in &opts.networking.bind_addrs {
            let sock: std::net::SocketAddr = addr_str.parse().map_err(|e| {
                crate::CoreError::invalid_input(format!("invalid bind address \"{addr_str}\": {e}"))
            })?;
            builder = builder.bind_addr(sock).map_err(|e| {
                crate::CoreError::invalid_input(format!("bind address \"{addr_str}\": {e}"))
            })?;
        }

        // Proxy configuration.
        if let Some(ref proxy) = opts.networking.proxy_url {
            let url: url::Url = proxy
                .parse()
                .map_err(|e| crate::CoreError::invalid_input(format!("invalid proxy URL: {e}")))?;
            builder = builder.proxy_url(url);
        } else if opts.networking.proxy_from_env {
            builder = builder.proxy_from_env();
        }

        if opts.keylog {
            builder = builder.keylog(true);
        }

        let ep: Endpoint = builder.bind().await.map_err(classify_bind_error)?;
        let node_id_str = crate::base32_encode(ep.id().as_bytes());

        let store_config = StoreConfig {
            channel_capacity: opts
                .streaming
                .channel_capacity
                .unwrap_or(crate::ffi::handles::DEFAULT_CHANNEL_CAPACITY)
                .max(1),
            max_chunk_size: opts
                .streaming
                .max_chunk_size_bytes
                .unwrap_or(crate::ffi::handles::DEFAULT_MAX_CHUNK_SIZE)
                .max(1),
            drain_timeout: Duration::from_millis(
                opts.streaming
                    .drain_timeout_ms
                    .unwrap_or(crate::ffi::handles::DEFAULT_DRAIN_TIMEOUT_MS),
            ),
            max_handles: crate::ffi::handles::DEFAULT_MAX_HANDLES,
            ttl: Duration::from_millis(
                opts.streaming
                    .handle_ttl_ms
                    .unwrap_or(crate::ffi::handles::DEFAULT_SLAB_TTL_MS),
            ),
        };
        let sweep_ttl = store_config.ttl;
        let sweep_interval = Duration::from_millis(
            opts.streaming
                .sweep_interval_ms
                .unwrap_or(crate::ffi::handles::DEFAULT_SWEEP_INTERVAL_MS),
        );
        let (closed_tx, closed_rx) = tokio::sync::watch::channel(false);
        let (event_tx, event_rx) =
            tokio::sync::mpsc::channel::<crate::http::events::TransportEvent>(256);

        let pool = ConnectionPool::new(
            opts.pool.max_connections,
            opts.pool
                .idle_timeout_ms
                .map(std::time::Duration::from_millis),
            Some(event_tx.clone()),
        );

        let max_header_size = match opts.max_header_size {
            Some(0) => {
                return Err(crate::CoreError::invalid_input(
                    "max_header_size must be > 0; use None for the default (65536)",
                ));
            }
            None => 64 * 1024,
            Some(n) => n,
        };

        let inner = Arc::new(EndpointInner {
            transport: Transport { ep, node_id_str },
            http: HttpRuntime {
                pool,
                max_header_size,
                max_response_body_bytes: opts
                    .max_response_body_bytes
                    .unwrap_or(crate::http::server::DEFAULT_MAX_RESPONSE_BODY_BYTES),
                active_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                active_requests: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                compression: opts.compression,
            },
            session: SessionRuntime {
                serve_handle: std::sync::Mutex::new(None),
                serve_stopped_early: std::sync::atomic::AtomicBool::new(false),
                serve_done_rx: std::sync::Mutex::new(None),
                closed_tx,
                closed_rx,
                event_tx,
                event_rx: std::sync::Mutex::new(Some(event_rx)),
                path_subs: dashmap::DashMap::new(),
                active_path_watchers: std::sync::atomic::AtomicUsize::new(0),
            },
            ffi: FfiBridge {
                handles: HandleStore::new(store_config),
                local_service: std::sync::Mutex::new(None),
            },
        });

        // Start per-endpoint sweep task (held alive via Weak reference).
        if !sweep_ttl.is_zero() {
            let weak = Arc::downgrade(&inner);
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(sweep_interval);
                loop {
                    ticker.tick().await;
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    inner.ffi.handles.sweep(sweep_ttl);
                    drop(inner); // release strong ref between ticks
                }
            });
        }

        Ok(Self { inner })
    }
}

/// Classify a bind error into a `CoreError`.
pub(super) fn classify_bind_error(e: impl std::fmt::Display) -> crate::CoreError {
    let msg = e.to_string();
    crate::CoreError::connection_failed(msg)
}

/// Parse an optional list of socket address strings into `SocketAddr` values.
///
/// Returns `Err` if any string cannot be parsed as a `host:port` address so
/// that callers can surface misconfiguration rather than silently ignoring it.
pub fn parse_direct_addrs(
    addrs: &Option<Vec<String>>,
) -> Result<Option<Vec<std::net::SocketAddr>>, String> {
    match addrs {
        None => Ok(None),
        Some(v) => {
            let mut out = Vec::with_capacity(v.len());
            for s in v {
                let addr = s
                    .parse::<std::net::SocketAddr>()
                    .map_err(|e| format!("invalid direct address {s:?}: {e}"))?;
                out.push(addr);
            }
            Ok(Some(out))
        }
    }
}
