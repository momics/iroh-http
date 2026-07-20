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
    config::NodeOptions, dns_nameservers::ConfiguredDns, ffi_bridge::FfiBridge,
    http_runtime::HttpRuntime, session_runtime::SessionRuntime, transport::Transport,
    EndpointInner, IrohEndpoint,
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
        let configured_dns = ConfiguredDns::configure(&opts.discovery.dns_nameservers).await?;
        let (dns_resolver, scoped_dns_compat) = configured_dns.into_parts();

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

        if let Some(resolver) = dns_resolver {
            builder = builder.dns_resolver(resolver);
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
                scoped_dns_compat,
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
                path_subs: std::sync::Mutex::new(std::collections::HashMap::new()),
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
