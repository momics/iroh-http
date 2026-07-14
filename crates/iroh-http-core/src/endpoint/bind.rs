//! `IrohEndpoint::bind` constructor and related helpers.
//!
//! Split from `mod.rs` so the facade stays ≤ 200 LoC. All other
//! `IrohEndpoint` methods live in `mod.rs` (accessors), `lifecycle.rs`
//! (close / drain / serve wiring), and `observe.rs` (peer info).
// bind constructs HandleStore directly; endpoint/ is neither mod http nor
// mod ffi, so this allow is correct here (same rationale as mod.rs lines 20-22).
#![allow(clippy::disallowed_types)]

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

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
        let mut scoped_dns_proxies = Vec::new();

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

        // Custom DNS nameservers. iroh's default resolver reads the system DNS
        // config, but that is unavailable on some platforms (notably Android,
        // which has no `/etc/resolv.conf` and needs a JNI-initialised
        // `ndk_context`). When explicit nameservers are supplied, build a
        // resolver from them so relay, pkarr, and DNS-discovery lookups resolve
        // instead of timing out.
        if !opts.discovery.dns_nameservers.is_empty() {
            let mut resolver = iroh::dns::DnsResolver::builder();
            let mut added = 0usize;
            let mut rejected = Vec::new();
            for ns in &opts.discovery.dns_nameservers {
                match parse_dns_nameserver(ns) {
                    Ok(addr) => {
                        // Hickory's nameserver config stores only `addr.ip()`, so
                        // passing a scoped SocketAddrV6 directly would silently
                        // discard the interface index. Route scoped servers via
                        // a loopback UDP proxy whose outbound send still uses the
                        // exact SocketAddrV6, including its scope ID.
                        let resolver_addr = match addr {
                            SocketAddr::V6(v6) if v6.scope_id() != 0 => {
                                let proxy = start_scoped_dns_proxy(addr).await.map_err(|e| {
                                    crate::CoreError::connection_failed(format!(
                                        "failed to start scoped DNS proxy for {ns:?}: {e}"
                                    ))
                                })?;
                                let local_addr = proxy.local_addr();
                                scoped_dns_proxies.push(proxy);
                                local_addr
                            }
                            _ => addr,
                        };
                        resolver =
                            resolver.with_nameserver(resolver_addr, iroh::dns::DnsProtocol::Udp);
                        added = added.saturating_add(1);
                    }
                    Err(reason) => rejected.push(reason),
                }
            }
            if !rejected.is_empty() {
                tracing::warn!(
                    "iroh-http: ignoring {} unparseable DNS nameserver(s): {}",
                    rejected.len(),
                    rejected.join("; "),
                );
            }
            // F26: explicit nameservers were requested but NONE parsed. Failing
            // silently would fall back to the default resolver — exactly the
            // broken path (e.g. Android) these servers were supplied to avoid.
            // Surface it instead of shipping a node that can't resolve.
            if added == 0 {
                return Err(crate::CoreError::invalid_input(format!(
                    "all {} supplied dns_nameservers were invalid (expected IP addresses): {}",
                    rejected.len(),
                    rejected.join("; "),
                )));
            }
            builder = builder.dns_resolver(resolver.build());
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
            transport: Transport {
                ep,
                node_id_str,
                scoped_dns_proxies,
            },
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
                serve_started_gen: std::sync::atomic::AtomicU64::new(0),
                serve_stopped_gen: std::sync::atomic::AtomicU64::new(0),
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

/// Parse a configured DNS nameserver without losing IPv6 routing scope.
///
/// A scoped link-local IPv6 server must retain its interface index in the
/// resulting `SocketAddrV6`; sending to the same address with scope zero is not
/// equivalent. Named scopes are rejected because resolving interface names is
/// platform-specific. Mobile adapters should supply the numeric interface
/// index exposed by their networking API.
fn parse_dns_nameserver(value: &str) -> Result<std::net::SocketAddr, String> {
    if let Some((host, scope)) = value.split_once('%') {
        let ip = host
            .parse::<std::net::Ipv6Addr>()
            .map_err(|e| format!("invalid scoped IPv6 DNS nameserver {value:?}: {e}"))?;
        let scope_id = scope.parse::<u32>().map_err(|_| {
            format!("IPv6 DNS nameserver {value:?} must use a numeric interface scope")
        })?;
        if scope_id == 0 {
            return Err(format!(
                "IPv6 DNS nameserver {value:?} must use a non-zero interface scope"
            ));
        }
        return Ok(std::net::SocketAddr::V6(std::net::SocketAddrV6::new(
            ip, 53, 0, scope_id,
        )));
    }
    let ip = value
        .parse::<std::net::IpAddr>()
        .map_err(|e| format!("invalid DNS nameserver {value:?}: {e}"))?;
    if let std::net::IpAddr::V6(v6) = ip {
        let is_link_local = (v6.segments()[0] & 0xffc0) == 0xfe80;
        if is_link_local {
            return Err(format!(
                "link-local IPv6 DNS nameserver {value:?} requires a numeric interface scope"
            ));
        }
    }
    Ok(std::net::SocketAddr::new(ip, 53))
}

/// Per-endpoint forwarding proxy for a scoped IPv6 DNS nameserver.
///
/// `iroh-dns`/Hickory preserves a configured nameserver's port but represents
/// its destination as an `IpAddr`, which cannot carry an IPv6 scope ID. The
/// resolver therefore talks to `local_addr`, while this task forwards each
/// datagram to the exact scoped upstream address. Dropping the handle aborts
/// the listener and all in-flight forwards.
const MAX_SCOPED_DNS_IN_FLIGHT: usize = 32;
/// Practical EDNS UDP ceiling. One extra byte is allocated while receiving so
/// a truncated oversized datagram is distinguishable from an exact-size one.
const MAX_SCOPED_DNS_UDP_PAYLOAD: usize = 4096;

#[derive(Default)]
struct ScopedDnsProxyLoad {
    in_flight: AtomicUsize,
    dropped_queries: AtomicUsize,
}

struct InFlightDnsQuery(Arc<ScopedDnsProxyLoad>);

impl InFlightDnsQuery {
    fn begin(load: Arc<ScopedDnsProxyLoad>) -> Self {
        load.in_flight.fetch_add(1, Ordering::Relaxed);
        Self(load)
    }
}

impl Drop for InFlightDnsQuery {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) struct ScopedDnsProxy {
    local_addr: SocketAddr,
    _upstream: SocketAddr,
    task: tokio::task::JoinHandle<()>,
    _load: Arc<ScopedDnsProxyLoad>,
}

impl ScopedDnsProxy {
    fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    #[cfg(test)]
    fn upstream(&self) -> SocketAddr {
        self._upstream
    }

    #[cfg(test)]
    fn in_flight(&self) -> usize {
        self._load.in_flight.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn dropped_queries(&self) -> usize {
        self._load.dropped_queries.load(Ordering::Relaxed)
    }

    pub(super) fn shutdown(&self) {
        // Tokio documents `abort` as safe before or after task completion;
        // repeated close/close_force calls are therefore idempotent.
        self.task.abort();
    }

    #[cfg(test)]
    pub(super) fn is_running(&self) -> bool {
        !self.task.is_finished()
    }
}

impl Drop for ScopedDnsProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn start_scoped_dns_proxy(upstream: SocketAddr) -> io::Result<ScopedDnsProxy> {
    let SocketAddr::V6(v6) = upstream else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scoped DNS proxy requires an IPv6 upstream",
        ));
    };
    if v6.scope_id() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scoped DNS proxy requires a non-zero IPv6 scope ID",
        ));
    }

    // Hickory retains this ephemeral port. IPv4 loopback is deliberate: the
    // resolver-facing hop needs no scope and works even when the upstream is
    // reachable only over IPv6.
    let listener = Arc::new(tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
    let local_addr = listener.local_addr()?;
    let load = Arc::new(ScopedDnsProxyLoad::default());
    let task = tokio::spawn(run_scoped_dns_proxy(listener, upstream, Arc::clone(&load)));
    Ok(ScopedDnsProxy {
        local_addr,
        _upstream: upstream,
        task,
        _load: load,
    })
}

async fn run_scoped_dns_proxy(
    listener: Arc<tokio::net::UdpSocket>,
    upstream: SocketAddr,
    load: Arc<ScopedDnsProxyLoad>,
) {
    let mut query = vec![0u8; MAX_SCOPED_DNS_UDP_PAYLOAD + 1];
    let mut forwards = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            received = listener.recv_from(&mut query) => {
                let (len, client) = match received {
                    Ok(received) => received,
                    Err(reason) => {
                        tracing::debug!(%upstream, %reason, "scoped DNS proxy listener stopped");
                        break;
                    }
                };
                if len > MAX_SCOPED_DNS_UDP_PAYLOAD {
                    load.dropped_queries.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(%upstream, %client, len, "scoped DNS proxy dropping oversized query");
                    continue;
                }
                if forwards.len() >= MAX_SCOPED_DNS_IN_FLIGHT {
                    load.dropped_queries.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(%upstream, %client, "scoped DNS proxy at capacity; dropping query");
                    continue;
                }
                let listener = Arc::clone(&listener);
                let query = query[..len].to_vec();
                let in_flight = InFlightDnsQuery::begin(Arc::clone(&load));
                forwards.spawn(async move {
                    let _in_flight = in_flight;
                    if let Err(reason) = forward_dns_datagram(&listener, upstream, client, &query).await {
                        tracing::debug!(%upstream, %client, %reason, "scoped DNS query forwarding failed");
                    }
                });
            }
            Some(completed) = forwards.join_next(), if !forwards.is_empty() => {
                if let Err(reason) = completed {
                    tracing::debug!(%upstream, %reason, "scoped DNS forwarding task failed");
                }
            }
        }
    }
}

async fn forward_dns_datagram(
    listener: &tokio::net::UdpSocket,
    upstream: SocketAddr,
    client: SocketAddr,
    query: &[u8],
) -> io::Result<()> {
    let bind_addr = match upstream {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let outbound = tokio::net::UdpSocket::bind(bind_addr).await?;
    // A connected UDP socket retains the IPv6 scope on the destination and
    // makes the kernel discard datagrams from every other source.
    outbound.connect(upstream).await?;
    outbound.send(query).await?;

    let mut response = vec![0u8; MAX_SCOPED_DNS_UDP_PAYLOAD + 1];
    let len = tokio::time::timeout(iroh::dns::DNS_TIMEOUT, outbound.recv(&mut response))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "DNS upstream timed out"))??;
    if len > MAX_SCOPED_DNS_UDP_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "DNS upstream response exceeded the proxy payload cap",
        ));
    }
    listener.send_to(&response[..len], client).await?;
    Ok(())
}

/// Parse an optional list of socket address strings into `SocketAddr` values.
///
/// Returns `Err` if any string cannot be parsed as a `host:port` address, or if
/// a parsed address has port 0. A port-0 direct address is silently undialable
/// (it fails at connect time as an opaque error); rejecting it here — before it
/// reaches iroh's dialer — surfaces the misconfiguration loudly. See #346.
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
                if addr.port() == 0 {
                    return Err(format!(
                        "invalid direct address {s:?}: refusing to dial address with no port (port 0)"
                    ));
                }
                out.push(addr);
            }
            Ok(Some(out))
        }
    }
}

/// Reconcile a node's derived direct-address ports against its real bound
/// sockets.
///
/// Some platforms (notably iOS) enumerate a *local* interface address with the
/// port replaced by a non-dialable placeholder — `0` (unspecified) or `1` —
/// even though the QUIC socket is bound to a real port. Advertising such an
/// address makes the node undialable: peers reject the port-less address at
/// parse time (`invalid socket address syntax`, #346), and a `:1` is dialed as
/// literal TCP-mux port 1 which never answers QUIC (#350 F5).
///
/// For each candidate whose port is a placeholder, substitute a port taken from
/// `bound_sockets` of the same IP family (v4↔v4, v6↔v6). A candidate that has
/// no usable same-family socket is dropped with a warning rather than paired
/// with a port on which its address family is not listening.
///
/// Candidates with a real (non-placeholder) port are returned **unchanged**.
/// This is deliberate and is why we do *not* treat "any port not present among
/// the bound sockets" as a placeholder: `EndpointAddr` also carries reflexive
/// QAD addresses whose global `ip:port` is discovered via relay probing and by
/// design differs from the local bound port. Rewriting those to a bound port
/// would corrupt the node's NAT-traversal candidates. Only ports `0`/`1` — which
/// are never real QUIC ephemeral ports — are treated as placeholders.
pub(crate) fn is_placeholder_port(port: u16) -> bool {
    port == 0 || port == 1
}

pub(crate) fn reconcile_direct_addr_ports(
    candidates: &[std::net::SocketAddr],
    bound_sockets: &[std::net::SocketAddr],
) -> Vec<std::net::SocketAddr> {
    let borrow_port = |ipv6: bool| -> Option<u16> {
        bound_sockets
            .iter()
            .find(|s| s.is_ipv6() == ipv6 && !is_placeholder_port(s.port()))
            .map(|s| s.port())
    };

    let mut out = Vec::with_capacity(candidates.len());
    for &addr in candidates {
        if !is_placeholder_port(addr.port()) {
            out.push(addr);
            continue;
        }
        match borrow_port(addr.is_ipv6()) {
            Some(port) => {
                let mut fixed = addr;
                fixed.set_port(port);
                tracing::warn!(
                    "iroh-http: reconciled placeholder-port direct address {addr} to bound port {port} (#346/#350)"
                );
                out.push(fixed);
            }
            None => {
                tracing::warn!(
                    "iroh-http: dropping direct address {addr} with placeholder port (no same-family bound port to borrow) (#346/#350)"
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod direct_addr_tests {
    use super::{parse_direct_addrs, reconcile_direct_addr_ports};
    use std::net::SocketAddr;

    fn sock(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    // Regression: #346 — iOS advertises a direct address whose port is 0, even
    // though the QUIC socket is bound to a real port (seen as `:59234` in iOS
    // syslog). The derived/advertised address must carry the real bound port.
    #[test]
    fn reconcile_substitutes_bound_port_for_port_zero() {
        let candidates = vec![sock("192.168.50.227:0")];
        let bound = vec![sock("192.168.50.227:59234")];
        let out = reconcile_direct_addr_ports(&candidates, &bound);
        assert_eq!(out, vec![sock("192.168.50.227:59234")]);
        assert!(
            out.iter().all(|a| a.port() != 0),
            "no reconciled direct address may carry port 0, got {out:?}"
        );
    }

    #[test]
    fn reconcile_prefers_bound_socket_of_same_ip_family() {
        // A port-0 IPv4 candidate must take an IPv4 bound port, not the IPv6 one.
        let candidates = vec![sock("192.168.50.227:0"), sock("[fe80::1]:0")];
        let bound = vec![sock("[::]:60000"), sock("0.0.0.0:59234")];
        let out = reconcile_direct_addr_ports(&candidates, &bound);
        assert_eq!(out[0], sock("192.168.50.227:59234"));
        assert_eq!(out[1], sock("[fe80::1]:60000"));
    }

    // A placeholder address is only repairable with a real socket from the
    // same address family. Borrowing an IPv4 listener's port for an IPv6
    // candidate (or vice versa) advertises an address the endpoint never bound.
    #[test]
    fn reconcile_drops_placeholder_without_same_family_bound_socket() {
        let candidates = vec![sock("[2001:db8::7]:1")];
        let bound = vec![sock("0.0.0.0:62546")];

        assert_eq!(reconcile_direct_addr_ports(&candidates, &bound), vec![]);
    }

    #[test]
    fn reconcile_keeps_nonzero_ports_untouched() {
        let candidates = vec![sock("10.0.0.5:4433")];
        let bound = vec![sock("0.0.0.0:59234")];
        let out = reconcile_direct_addr_ports(&candidates, &bound);
        assert_eq!(out, vec![sock("10.0.0.5:4433")]);
    }

    // Regression: #350 F5 — iOS was observed enumerating a local address with
    // port `1` (not just `0`). `reconcile_direct_addr_ports` only replaced `:0`,
    // so `:1` leaked through and peers dialed literal TCP-mux port 1 → no QUIC.
    #[test]
    fn reconcile_substitutes_bound_port_for_port_one() {
        let candidates = vec![sock("192.168.50.227:1")];
        let bound = vec![sock("0.0.0.0:62546")];
        let out = reconcile_direct_addr_ports(&candidates, &bound);
        assert_eq!(out, vec![sock("192.168.50.227:62546")]);
    }

    #[test]
    fn reconcile_drops_port_one_when_no_bound_port_available() {
        let candidates = vec![sock("192.168.50.227:1")];
        let bound: Vec<SocketAddr> = vec![];
        let out = reconcile_direct_addr_ports(&candidates, &bound);
        assert!(
            out.is_empty(),
            "a :1 placeholder with no bound port must be dropped, got {out:?}"
        );
    }

    // Regression: #350 F5/F14 — a reflexive QAD address carries a real global
    // port discovered via relay probing that intentionally differs from the
    // local bound port. It must be preserved verbatim, never rewritten to the
    // bound port (that would corrupt the node's NAT-traversal candidate).
    #[test]
    fn reconcile_preserves_reflexive_qad_port() {
        // Local placeholder + reflexive global address, one bound socket.
        let candidates = vec![sock("192.168.50.227:1"), sock("192.26.168.188:40349")];
        let bound = vec![sock("0.0.0.0:62546")];
        let out = reconcile_direct_addr_ports(&candidates, &bound);
        assert_eq!(
            out[0],
            sock("192.168.50.227:62546"),
            "local placeholder reconciled"
        );
        assert_eq!(
            out[1],
            sock("192.26.168.188:40349"),
            "reflexive QAD global port must be preserved, not clobbered to the bound port"
        );
    }

    #[test]
    fn reconcile_drops_port_zero_when_no_bound_port_available() {
        let candidates = vec![sock("192.168.50.227:0")];
        let bound: Vec<SocketAddr> = vec![];
        let out = reconcile_direct_addr_ports(&candidates, &bound);
        assert!(
            out.is_empty(),
            "a port-0 address with no bound port to borrow must be dropped, got {out:?}"
        );
    }

    // Regression: #346 — a port-0 direct address must be rejected loudly before
    // it reaches the dialer, not silently handed to iroh as a useless `:0`.
    #[test]
    fn parse_direct_addrs_rejects_port_zero() {
        let err = parse_direct_addrs(&Some(vec!["192.168.50.227:0".to_string()]))
            .expect_err("port-0 direct address must be rejected");
        assert!(
            err.contains("192.168.50.227:0") && err.contains("port 0"),
            "error should name the offending address and port 0, got: {err}"
        );
    }

    #[test]
    fn parse_direct_addrs_still_rejects_portless_bare_ip() {
        // The exact shape observed in #346: a bare IPv4 with no `:port`.
        let err = parse_direct_addrs(&Some(vec!["192.168.50.227".to_string()]))
            .expect_err("bare IP without a port must be rejected");
        assert!(err.contains("192.168.50.227"));
    }

    #[test]
    fn parse_direct_addrs_accepts_well_formed() {
        let out = parse_direct_addrs(&Some(vec!["192.168.50.227:59234".to_string()]))
            .expect("well-formed address parses");
        assert_eq!(out, Some(vec![sock("192.168.50.227:59234")]));
    }

    // #350 F13 — parity corpus shared with the TypeScript `isDialableSocketAddr`
    // (mirrored in packages/iroh-http-tauri/guest-js/__tests__/discovery.test.ts).
    // `parse_direct_addrs` (all-or-nothing) must accept exactly the strings the
    // TS sanitiser accepts and reject exactly what it rejects, so a value that
    // passes the sanitiser never poisons the dialer's direct-address list.
    #[test]
    fn parse_direct_addrs_matches_ts_dialable_corpus() {
        let dialable = [
            "1.2.3.4:443",
            "192.168.50.227:59234",
            "[2001:db8::1]:443",
            "[::1]:8080",
            "[::ffff:192.168.1.1]:443",
            "0.0.0.0:1",
            "255.255.255.255:65535",
        ];
        for s in dialable {
            assert!(
                parse_direct_addrs(&Some(vec![s.to_string()])).is_ok(),
                "Rust must accept dialable {s:?} that the TS sanitiser accepts"
            );
        }

        let undialable = [
            "example.com:443",
            "999.999.999.999:443",
            "[not-ip]:443",
            "1.2.3:443",
            "1.2.3.4.5:443",
            "01.2.3.4:443",
            "1.2.3.4",
            "1.2.3.4:0",
            "2001:db8::1:443",
            "[2001:db8::1]:0",
            "[fe80::1",
            "1.2.3.4:70000",
        ];
        for s in undialable {
            assert!(
                parse_direct_addrs(&Some(vec![s.to_string()])).is_err(),
                "Rust must reject undialable {s:?} that the TS sanitiser rejects"
            );
        }
    }
}

#[cfg(test)]
mod dns_nameserver_tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
        time::Duration,
    };

    use super::{
        parse_dns_nameserver, start_scoped_dns_proxy, IrohEndpoint, MAX_SCOPED_DNS_IN_FLIGHT,
        MAX_SCOPED_DNS_UDP_PAYLOAD,
    };
    use crate::endpoint::config::{DiscoveryOptions, NodeOptions};

    fn a_record_response(query: &[u8], answer: Ipv4Addr) -> Vec<u8> {
        assert!(query.len() >= 12, "DNS query header is truncated");
        let mut question_end = 12;
        while query[question_end] != 0 {
            question_end += usize::from(query[question_end]) + 1;
            assert!(question_end < query.len(), "DNS query name is truncated");
        }
        question_end += 5; // root label + QTYPE + QCLASS
        assert!(question_end <= query.len(), "DNS question is truncated");

        let mut response = Vec::with_capacity(question_end + 16);
        response.extend_from_slice(&query[..2]); // transaction ID
        response.extend_from_slice(&[0x81, 0x80]); // response, recursion available, no error
        response.extend_from_slice(&[0, 1]); // one question
        response.extend_from_slice(&[0, 1]); // one answer
        response.extend_from_slice(&[0, 0, 0, 0]); // no authority/additional records
        response.extend_from_slice(&query[12..question_end]);
        response.extend_from_slice(&[
            0xc0, 0x0c, // compressed owner name
            0, 1, // A
            0, 1, // IN
            0, 0, 0, 60, // TTL
            0, 4, // RDLENGTH
        ]);
        response.extend_from_slice(&answer.octets());
        response
    }

    // Regression: #350 F26 — explicit DNS nameservers that all fail to parse
    // must fail the bind, not silently fall back to the default resolver (which
    // is exactly the broken path, e.g. Android with no /etc/resolv.conf, that
    // supplying nameservers was meant to avoid).
    #[tokio::test]
    async fn bind_rejects_all_invalid_dns_nameservers() {
        let discovery = {
            let mut d = DiscoveryOptions::new(None, true);
            d.dns_nameservers = vec!["not-an-ip".to_string(), "999.1.1.1".to_string()];
            d
        };
        let opts = NodeOptions {
            discovery,
            ..Default::default()
        };

        let err = match IrohEndpoint::bind(opts).await {
            Ok(_) => panic!("bind must fail when no supplied dns_nameserver parses"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("dns_nameservers") && msg.contains("not-an-ip"),
            "error should name the rejected nameservers, got: {msg}"
        );
    }

    // A mix of valid + invalid entries binds successfully (invalid ones are
    // warned and skipped) — only an all-invalid list is fatal.
    #[tokio::test]
    async fn bind_accepts_when_at_least_one_dns_nameserver_parses() {
        let discovery = {
            let mut d = DiscoveryOptions::new(None, true);
            d.dns_nameservers = vec!["bogus".to_string(), "8.8.8.8".to_string()];
            d
        };
        let opts = NodeOptions {
            discovery,
            ..Default::default()
        };

        let ep = match IrohEndpoint::bind(opts).await {
            Ok(ep) => ep,
            Err(e) => panic!("bind should succeed when at least one nameserver is valid: {e}"),
        };
        ep.close().await;
    }

    // A named IPv6 zone cannot be represented by the resolver's SocketAddr
    // interface without a platform-specific name-to-index lookup. Reject it
    // explicitly instead of silently discarding the scope and installing an
    // unroutable link-local nameserver. Android supplies numeric scope IDs.
    #[tokio::test]
    async fn bind_rejects_named_ipv6_dns_scope_instead_of_discarding_it() {
        let discovery = {
            let mut d = DiscoveryOptions::new(None, true);
            d.dns_nameservers = vec!["fe80::1%wlan0".to_string()];
            d
        };
        let opts = NodeOptions {
            discovery,
            ..Default::default()
        };

        let err = match IrohEndpoint::bind(opts).await {
            Ok(ep) => {
                ep.close().await;
                panic!("bind must not silently discard a required IPv6 DNS scope")
            }
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("wlan0"),
            "error should identify the unsupported scope, got: {err}"
        );
    }

    #[test]
    fn numeric_ipv6_dns_scope_is_preserved_in_resolver_address() {
        let addr = parse_dns_nameserver("fe80::1%17").expect("numeric scope must parse");

        let std::net::SocketAddr::V6(addr) = addr else {
            panic!("scoped IPv6 input must produce SocketAddrV6");
        };
        assert_eq!(addr.ip(), &"fe80::1".parse::<std::net::Ipv6Addr>().unwrap());
        assert_eq!(addr.port(), 53);
        assert_eq!(addr.scope_id(), 17, "routing scope must not be discarded");
    }

    #[test]
    fn unscoped_link_local_ipv6_dns_is_rejected() {
        let err = parse_dns_nameserver("fe80::1")
            .expect_err("a link-local DNS server without an interface cannot be routed");

        assert!(err.contains("interface scope"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn scoped_proxy_preserves_scope_through_actual_dns_lookup() {
        let upstream = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind fake IPv6 DNS server");
        let upstream_port = upstream.local_addr().unwrap().port();
        let exact_upstream =
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, upstream_port, 0, 17));
        let expected = Ipv4Addr::new(192, 0, 2, 44);
        let upstream_task = tokio::spawn(async move {
            let mut query = [0u8; 4096];
            let (len, peer) = upstream
                .recv_from(&mut query)
                .await
                .expect("receive forwarded DNS query");
            let response = a_record_response(&query[..len], expected);
            upstream
                .send_to(&response, peer)
                .await
                .expect("send fake DNS response");
        });

        let proxy = start_scoped_dns_proxy(exact_upstream)
            .await
            .expect("start scoped DNS proxy");
        assert_eq!(
            proxy.upstream(),
            exact_upstream,
            "the forwarding destination must retain its non-zero scope ID"
        );

        let resolver = iroh::dns::DnsResolver::with_nameserver(proxy.local_addr());
        let answers: Vec<_> = resolver
            .lookup_ipv4("scoped.test.", Duration::from_secs(2))
            .await
            .expect("actual iroh DNS lookup through proxy")
            .collect();

        assert_eq!(answers, [IpAddr::V4(expected)]);
        upstream_task.await.expect("fake DNS server task");
    }

    #[tokio::test]
    async fn scoped_proxy_load_sheds_flood_when_upstream_never_responds() {
        // A bound-but-silent socket deterministically accepts every forwarded
        // datagram without producing ICMP errors or DNS responses. Every
        // accepted proxy job therefore remains in flight until timeout.
        let silent_upstream = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind silent IPv6 upstream");
        let upstream = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            silent_upstream.local_addr().unwrap().port(),
            0,
            17,
        ));
        let proxy = start_scoped_dns_proxy(upstream)
            .await
            .expect("start scoped DNS proxy");
        let client = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind flood client");
        let query = [0u8; 12];

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                for _ in 0..8 {
                    client
                        .send_to(&query, proxy.local_addr())
                        .await
                        .expect("send local DNS flood packet");
                }
                tokio::task::yield_now().await;
                if proxy.in_flight() == MAX_SCOPED_DNS_IN_FLIGHT && proxy.dropped_queries() > 0 {
                    break;
                }
            }
        })
        .await
        .expect("proxy must reach its bound and load-shed promptly");

        assert_eq!(proxy.in_flight(), MAX_SCOPED_DNS_IN_FLIGHT);
        assert!(proxy.dropped_queries() > 0);
        drop(silent_upstream);
    }

    #[tokio::test]
    async fn scoped_proxy_drops_oversized_dns_datagrams() {
        let upstream = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind fake IPv6 upstream");
        let upstream_addr = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            upstream.local_addr().unwrap().port(),
            0,
            17,
        ));
        let proxy = start_scoped_dns_proxy(upstream_addr)
            .await
            .expect("start scoped DNS proxy");
        let client = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind local DNS client");

        client
            .send_to(
                &vec![0u8; MAX_SCOPED_DNS_UDP_PAYLOAD + 1],
                proxy.local_addr(),
            )
            .await
            .expect("send oversized local DNS datagram");
        tokio::time::timeout(Duration::from_secs(1), async {
            while proxy.dropped_queries() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("oversized datagram must be dropped promptly");

        assert_eq!(proxy.in_flight(), 0);
        let mut forwarded = vec![0u8; MAX_SCOPED_DNS_UDP_PAYLOAD + 1];
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                upstream.recv_from(&mut forwarded)
            )
            .await
            .is_err(),
            "oversized DNS datagram must never reach the upstream"
        );
    }

    #[tokio::test]
    async fn retained_endpoint_close_modes_stop_scoped_dns_proxy() {
        async fn assert_close_mode(force: bool) {
            let discovery = {
                let mut discovery = DiscoveryOptions::new(None, false);
                discovery.dns_nameservers = vec!["fe80::1%17".to_string()];
                discovery
            };
            let mut opts = NodeOptions {
                discovery,
                ..Default::default()
            };
            opts.networking.disabled = true;
            let ep = IrohEndpoint::bind(opts)
                .await
                .expect("bind retained endpoint with scoped DNS proxy");

            assert_eq!(ep.inner.transport.scoped_dns_proxy_count(), 1);
            assert_eq!(ep.inner.transport.running_scoped_dns_proxy_count(), 1);
            let retained_node_id = ep.node_id().to_string();
            if force {
                ep.close_force().await;
            } else {
                ep.close().await;
            }

            tokio::time::timeout(Duration::from_secs(1), async {
                while ep.inner.transport.running_scoped_dns_proxy_count() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("close must stop proxy while endpoint handle is retained");
            assert_eq!(ep.node_id(), retained_node_id);
        }

        assert_close_mode(false).await;
        assert_close_mode(true).await;
    }
}
