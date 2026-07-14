//! `iroh-http-discovery` — local mDNS peer discovery for iroh-http.
//!
//! Implements standard DNS-SD (RFC 6763) mDNS so nodes on the same local
//! network can find each other without a relay server.
//!
//! Use [`browse_peers`] and [`advertise_peer`] to start discovery sessions.
//!
//! # Wire format
//!
//! Discovery speaks **standard DNS-SD**: advertising publishes `PTR` + `SRV` +
//! `TXT` + `A`/`AAAA` records under `_<service_name>._udp.local`, with the
//! service instance name set to the node's base32 endpoint id and a `pk` TXT
//! property carrying the same id. This is what Apple's mDNSResponder (iOS
//! `NWBrowser`) and Android's `NsdManager` expect, so desktop nodes are
//! discoverable from mobile — the previous swarm-discovery backend omitted the
//! `PTR` record and was invisible to those browsers (issue #329).
//!
//! # Platform notes
//!
//! - Desktop (macOS, Linux, Windows): enabled with the `mdns` feature (default).
//! - iOS / Android (Tauri mobile): use the platform's native service discovery.
#![deny(unsafe_code)]

#[cfg(feature = "mdns")]
pub mod dns_sd;

#[cfg(feature = "mdns")]
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};

#[cfg(feature = "mdns")]
use futures::stream::BoxStream;

#[cfg(feature = "mdns")]
use iroh::{
    address_lookup::{AddressLookup, AddressLookupServices, Error as AddressLookupError, Item},
    EndpointId, Watcher,
};

#[cfg(feature = "mdns")]
use iroh_http_core::{AddressLookupSource, SourceScopedAddressLookup};

#[cfg(feature = "mdns")]
pub use dns_sd::{
    advertise, browse, AdvertiseSession, BrowseConfig, BrowseSession as ServiceBrowseSession,
    Protocol, ServiceConfig, ServiceRecord,
};

// ── DiscoveryError ────────────────────────────────────────────────────────────

/// Structured error returned by [`browse_peers`] and [`advertise_peer`].
///
/// Using an enum instead of `String` lets callers handle different failure
/// modes programmatically (issue-45 fix).
#[derive(Debug)]
pub enum DiscoveryError {
    /// The mDNS subsystem could not be initialised (e.g., network interface
    /// unavailable, permission denied, unsupported OS).
    Setup(String),
    /// The provided service name is invalid (e.g., contains illegal characters).
    InvalidServiceName(String),
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoveryError::Setup(msg) => write!(f, "mDNS setup failed: {msg}"),
            DiscoveryError::InvalidServiceName(msg) => {
                write!(f, "invalid mDNS service name: {msg}")
            }
        }
    }
}

impl std::error::Error for DiscoveryError {}

// ── Peer discovery event ─────────────────────────────────────────────────────

/// A discovery event suitable for FFI transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDiscoveryEvent {
    /// `true` = peer appeared or updated; `false` = peer expired.
    pub is_active: bool,
    /// Base32 public key of the discovered peer.
    pub node_id: String,
    /// Known addresses: relay URLs and/or `ip:port` strings.
    pub addrs: Vec<String>,
}

// ── iroh-http conventions ─────────────────────────────────────────────────────

/// TXT property carrying the peer's base32 endpoint id.
///
/// Part of the iroh-http DNS-SD convention (ADR-018); also exported so generic
/// [`dns_sd`] browsers can recognize iroh-http peers.
pub const TXT_PK: &str = "pk";
/// TXT property carrying the peer's home relay URL, if any.
pub const TXT_RELAY: &str = "relay";
/// TXT property carrying direct `ip:port` addresses, if any.
///
/// Advertisers whose SRV port is not the real QUIC port — notably the iOS
/// native `NWListener`, whose UDP port is unrelated to the QUIC socket —
/// publish the complete dialable candidates here as a comma-separated value.
/// Real local and reflexive candidate ports are authoritative; only `:0`/`:1`
/// platform placeholders borrow a same-family bound port. Browsers split,
/// validate, and deduplicate every member. See #346/#350.
pub const TXT_ADDRESS: &str = "address";

#[cfg(feature = "mdns")]
const DNS_SD_TXT_ENTRY_MAX_BYTES: usize = 255;

/// Join as many complete values as fit in one DNS-SD TXT entry.
///
/// RFC 6763 TXT entries are length-prefixed by one byte, so `key=value` must
/// not exceed 255 bytes. Selection is stable: input order is preserved and an
/// item that does not fit is skipped without truncating it.
#[cfg(feature = "mdns")]
fn fit_txt_values(key: &str, values: &[String]) -> Option<String> {
    let prefix_len = key.len().checked_add(1)?;
    let value_budget = DNS_SD_TXT_ENTRY_MAX_BYTES.checked_sub(prefix_len)?;
    let mut joined = String::new();
    for value in values.iter().filter(|value| !value.is_empty()) {
        let separator_len = usize::from(!joined.is_empty());
        let candidate_len = joined
            .len()
            .checked_add(separator_len)?
            .checked_add(value.len())?;
        if candidate_len > value_budget {
            continue;
        }
        if separator_len != 0 {
            joined.push(',');
        }
        joined.push_str(value);
    }
    (!joined.is_empty()).then_some(joined)
}

/// Encode an endpoint id as lowercase RFC 4648 base32 (no padding) — a 52-char
/// label that fits a DNS-SD instance name and matches the node-id form used by
/// the rest of iroh-http. iroh's own `Display` is 64-char hex, which exceeds the
/// 63-byte DNS label limit and is rejected by `mdns-sd`.
#[cfg(feature = "mdns")]
fn node_id_label(id: &iroh::EndpointId) -> String {
    base32::encode(
        base32::Alphabet::Rfc4648Lower { padding: false },
        id.as_bytes(),
    )
}

/// Interpret a generic DNS-SD [`ServiceRecord`] as an iroh-http peer event.
///
/// The node id comes from the `pk` TXT property, falling back to the instance
/// label. On active records the resolved socket addresses are surfaced and a
/// `relay` TXT property is appended if present. Returns `None` if no usable
/// node id can be derived.
#[cfg(feature = "mdns")]
fn peer_event_from_record(rec: &ServiceRecord) -> Option<PeerDiscoveryEvent> {
    let node_id = rec
        .txt
        .iter()
        .find(|(k, _)| k == TXT_PK)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            if rec.instance_name.is_empty() {
                None
            } else {
                Some(rec.instance_name.clone())
            }
        })?;

    if !rec.is_active {
        return Some(PeerDiscoveryEvent {
            is_active: false,
            node_id,
            addrs: Vec::new(),
        });
    }

    // Direct `ip:port` candidates in the `address` TXT are dialable even when
    // the record's SRV port is not the real QUIC port (native adapters, #346).
    // Parse them first so an authoritative TXT port can suppress a fabricated
    // same-IP SRV/A pairing while unrelated SRV fallbacks remain available.
    let mut txt_addrs = Vec::new();
    if let Some((_, addresses)) = rec.txt.iter().find(|(k, _)| k == TXT_ADDRESS) {
        for address in addresses.split(',').map(str::trim) {
            let Ok(address) = address.parse::<SocketAddr>() else {
                continue;
            };
            if address.port() <= 1 {
                continue;
            }
            if matches!(
                address,
                SocketAddr::V6(address)
                    if (address.ip().segments()[0] & 0xffc0) == 0xfe80
                        && address.scope_id() == 0
            ) {
                // A link-local IPv6 literal is ambiguous without the
                // interface index carried by a scoped DNS-SD A/AAAA result.
                continue;
            }
            if !txt_addrs.contains(&address) {
                txt_addrs.push(address);
            }
        }
    }
    let mut addrs: Vec<String> = rec
        .addrs
        .iter()
        .filter(|srv| srv.port() > 1)
        .filter(|srv| {
            !txt_addrs.iter().any(|authoritative| {
                same_scoped_host(authoritative, srv) && authoritative.port() != srv.port()
            })
        })
        .map(SocketAddr::to_string)
        .collect();
    for address in txt_addrs {
        let address = address.to_string();
        if !addrs.contains(&address) {
            addrs.push(address);
        }
    }
    if let Some((_, relay)) = rec.txt.iter().find(|(k, _)| k == TXT_RELAY) {
        addrs.push(relay.clone());
    }

    Some(PeerDiscoveryEvent {
        is_active: true,
        node_id,
        addrs,
    })
}

#[cfg(feature = "mdns")]
fn same_scoped_host(left: &SocketAddr, right: &SocketAddr) -> bool {
    match (left, right) {
        (SocketAddr::V4(left), SocketAddr::V4(right)) => left.ip() == right.ip(),
        (SocketAddr::V6(left), SocketAddr::V6(right)) => {
            left.ip() == right.ip()
                && ((left.ip().segments()[0] & 0xffc0) != 0xfe80
                    || left.scope_id() == right.scope_id())
        }
        _ => false,
    }
}

// ── iroh-http peer browse (specialization of dns_sd::browse) ─────────────────

/// An active iroh-http peer-browse session that yields [`PeerDiscoveryEvent`]s.
///
/// Wraps a generic [`dns_sd::browse`] session and, as records arrive, feeds the
/// endpoint's in-process address lookup so `fetch(nodeId)` resolves LAN peers.
/// Drop to stop receiving events; the underlying DNS-SD browse and its daemon
/// are shut down, and every address contributed by this browse is synchronously
/// retracted from the endpoint lookup.
#[cfg(feature = "mdns")]
#[derive(Debug)]
struct EndpointLookupProvider {
    lookup: SourceScopedAddressLookup,
}

#[cfg(feature = "mdns")]
impl AddressLookup for EndpointLookupProvider {
    fn resolve(
        &self,
        endpoint_id: EndpointId,
    ) -> Option<BoxStream<'static, Result<Item, AddressLookupError>>> {
        self.lookup.resolve(endpoint_id)
    }
}

#[cfg(feature = "mdns")]
fn endpoint_lookup_registry() -> &'static Mutex<HashMap<usize, Weak<EndpointLookupProvider>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Weak<EndpointLookupProvider>>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(feature = "mdns")]
fn endpoint_lookup_for_key(
    endpoint_key: usize,
    services: &AddressLookupServices,
) -> SourceScopedAddressLookup {
    let mut registry = endpoint_lookup_registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    registry.retain(|_, provider| provider.strong_count() != 0);
    if let Some(provider) = registry.get(&endpoint_key).and_then(Weak::upgrade) {
        return provider.lookup.clone();
    }

    let provider = Arc::new(EndpointLookupProvider {
        lookup: SourceScopedAddressLookup::new("mdns-sd"),
    });
    services.add(provider.clone());
    registry.insert(endpoint_key, Arc::downgrade(&provider));
    provider.lookup.clone()
}

#[cfg(feature = "mdns")]
fn endpoint_lookup(services: &AddressLookupServices) -> SourceScopedAddressLookup {
    // `Endpoint::address_lookup` returns a reference to a field in the endpoint's
    // shared inner allocation, so this address is stable across Endpoint clones
    // and distinct for simultaneously live endpoints. The registry stores only
    // a Weak provider: once that actual endpoint is gone, allocator key reuse
    // creates a fresh provider instead of inheriting stale discovery state.
    endpoint_lookup_for_key(services as *const AddressLookupServices as usize, services)
}

#[cfg(feature = "mdns")]
struct PeerBrowseShared {
    inner: ServiceBrowseSession,
    source: AddressLookupSource,
    pending: Mutex<VecDeque<PeerDiscoveryEvent>>,
    next_event: tokio::sync::Mutex<()>,
    endpoint_task: Mutex<Option<tokio::task::AbortHandle>>,
    closed: AtomicBool,
}

#[cfg(feature = "mdns")]
impl PeerBrowseShared {
    fn new(inner: ServiceBrowseSession, source: AddressLookupSource) -> Arc<Self> {
        let shared = Arc::new(Self {
            inner,
            source,
            pending: Mutex::new(VecDeque::new()),
            next_event: tokio::sync::Mutex::new(()),
            endpoint_task: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        let terminal = Arc::downgrade(&shared);
        shared.inner.on_close(move || {
            if let Some(shared) = terminal.upgrade() {
                shared.close();
            }
        });
        shared
    }

    fn install_endpoint_task(&self, task: tokio::task::AbortHandle) {
        let mut slot = self
            .endpoint_task
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.closed.load(Ordering::Acquire) {
            task.abort();
        } else {
            *slot = Some(task);
        }
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.source.retire();
        self.inner.close();
        if let Some(task) = self
            .endpoint_task
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            task.abort();
        }
    }
}

#[cfg(feature = "mdns")]
pub struct BrowseSession {
    shared: Arc<PeerBrowseShared>,
}

#[cfg(feature = "mdns")]
fn apply_peer_record(
    source: &AddressLookupSource,
    record: &ServiceRecord,
) -> Vec<PeerDiscoveryEvent> {
    let Some(mut event) = peer_event_from_record(record) else {
        return Vec::new();
    };
    let from_removal = |removal: iroh_http_core::AddressLookupRemoval| PeerDiscoveryEvent {
        is_active: removal.has_remaining_contributions,
        node_id: removal.node_id,
        addrs: removal.remaining_addrs,
    };
    if event.is_active {
        match source.upsert(&record.instance_name, &event.node_id, &event.addrs) {
            Ok(update) => {
                let mut events = Vec::with_capacity(2);
                if let Some(replaced) = update.replaced {
                    events.push(from_removal(replaced));
                }
                event.node_id = update.node_id;
                event.addrs = update.effective_addrs;
                events.push(event);
                events
            }
            Err(error) => {
                tracing::warn!(
                    instance_name = %record.instance_name,
                    node_id = %event.node_id,
                    %error,
                    "iroh-http-discovery: rejecting invalid peer lookup contribution"
                );
                error.into_removal().map(from_removal).into_iter().collect()
            }
        }
    } else {
        source
            .remove_with_snapshot(&record.instance_name)
            .map(from_removal)
            .into_iter()
            .collect()
    }
}

#[cfg(feature = "mdns")]
impl BrowseSession {
    /// Returns the next peer event, or `None` when the session is closed.
    ///
    /// Generic (non-iroh) records that carry no usable node id are skipped.
    /// This takes `&self`, so callers can share a session through an [`Arc`]
    /// without adding an external asynchronous mutex.
    pub async fn next_event(&self) -> Option<PeerDiscoveryEvent> {
        // Keep receive + lookup mutation ordered when callers issue concurrent
        // polls through a shared Arc.
        let _next_event = self.shared.next_event.lock().await;
        loop {
            if self.shared.closed.load(Ordering::Acquire) {
                return None;
            }
            if let Some(event) = self
                .shared
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pop_front()
            {
                if self.shared.closed.load(Ordering::Acquire) {
                    return None;
                }
                return Some(event);
            }
            let Some(record) = self.shared.inner.next_record().await else {
                self.close();
                return None;
            };
            let mut events = apply_peer_record(&self.shared.source, &record).into_iter();
            let Some(event) = events.next() else {
                continue;
            };
            let mut pending = self
                .shared
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if self.shared.closed.load(Ordering::Acquire) {
                return None;
            }
            pending.extend(events);
            drop(pending);
            return Some(event);
        }
    }

    /// Synchronously stop browsing and retract every address contributed by
    /// this browse. Closing is idempotent and wakes a pending [`Self::next_event`].
    pub fn close(&self) {
        self.shared.close();
    }
}

#[cfg(feature = "mdns")]
impl Drop for BrowseSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Start a browse session: discover iroh-http peers on the local network.
///
/// Browses `_<service_name>._udp.local`, and registers an in-process
/// [`AddressLookup`](iroh::address_lookup::AddressLookup) on the endpoint fed by
/// the discovered peers, so `fetch(nodeId)` auto-resolves LAN peers by node id.
///
/// For non-iroh services (custom instance names, ports, or TXT), use
/// [`dns_sd::browse`] directly.
#[cfg(feature = "mdns")]
pub async fn browse_peers(
    ep: &iroh::Endpoint,
    service_name: &str,
) -> Result<BrowseSession, DiscoveryError> {
    let services = ep
        .address_lookup()
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?;
    // Establish the native browse before installing the endpoint provider, so
    // invalid service names and daemon setup failures cannot grow the endpoint's
    // permanent AddressLookupServices list.
    let inner = dns_sd::browse(BrowseConfig::udp(service_name))?;
    if ep.is_closed() {
        inner.close();
        return Err(DiscoveryError::Setup("endpoint is closed".to_string()));
    }
    let lookup = endpoint_lookup(services);
    let source = lookup.new_source();
    let shared = PeerBrowseShared::new(inner, source);
    let weak = Arc::downgrade(&shared);
    let endpoint_closed = ep.closed();
    let task = tokio::spawn(async move {
        endpoint_closed.await;
        if let Some(shared) = weak.upgrade() {
            shared.close();
        }
    });
    shared.install_endpoint_task(task.abort_handle());
    drop(task);
    // Close can win after the first preflight but before the endpoint watcher
    // is installed. Reject when it did, rather than returning a browse handle
    // that was already retired before this start operation linearized.
    if ep.is_closed() {
        shared.close();
        return Err(DiscoveryError::Setup("endpoint is closed".to_string()));
    }
    Ok(BrowseSession { shared })
}

// ── iroh-http advertise (specialization of dns_sd::advertise) ────────────────

#[cfg(all(test, feature = "mdns"))]
fn select_advertise_address(
    ip_addrs: &[SocketAddr],
    bound_sockets: &[SocketAddr],
) -> Option<String> {
    select_advertise_addresses(ip_addrs, bound_sockets)
        .into_iter()
        .next()
}

/// Select every dialable endpoint candidate for the `address` TXT boundary.
///
/// Loopback, unspecified, and link-local addresses are excluded. Real ports
/// (including reflexive QAD ports) remain authoritative. Only a `:0`/`:1`
/// placeholder borrows a same-family bound QUIC port; a placeholder with no
/// matching listener is skipped. Order is preserved and duplicates collapse,
/// retaining simultaneous LAN, VPN, IPv4, and IPv6 paths.
#[cfg(feature = "mdns")]
fn select_advertise_addresses(
    ip_addrs: &[SocketAddr],
    bound_sockets: &[SocketAddr],
) -> Vec<String> {
    let mut selected = Vec::new();
    for addr in ip_addrs
        .iter()
        .copied()
        .filter(|addr| is_routable_ip(&addr.ip()))
    {
        let addr = if is_placeholder_port(addr.port()) {
            let Some(bound) = bound_sockets.iter().find(|bound| {
                bound.is_ipv6() == addr.is_ipv6() && !is_placeholder_port(bound.port())
            }) else {
                continue;
            };
            SocketAddr::new(addr.ip(), bound.port())
        } else {
            addr
        };
        let addr = addr.to_string();
        if !selected.contains(&addr) {
            selected.push(addr);
        }
    }
    selected
}

/// Whether `port` is a non-dialable placeholder (`0` unspecified, or `1` — the
/// value iOS was observed enumerating for a local address, never a real QUIC
/// ephemeral port). Mirrors `is_placeholder_port` in `iroh-http-core`.
#[cfg(feature = "mdns")]
fn is_placeholder_port(port: u16) -> bool {
    port == 0 || port == 1
}

/// Whether `ip` is routable off-link: not loopback, unspecified, or link-local.
///
/// A link-local address (IPv4 `169.254.0.0/16`, IPv6 `fe80::/10`) is only valid
/// on its own segment, so advertising it as a dialable address makes a browsing
/// peer's direct dial fail (#350). `Ipv6Addr::is_unicast_link_local` is still
/// unstable, so the `fe80::/10` prefix is matched by hand.
#[cfg(feature = "mdns")]
fn is_routable_ip(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

#[cfg(feature = "mdns")]
fn peer_service_config(
    service_name: &str,
    instance_name: &str,
    bound_sockets: &[SocketAddr],
    node_addr: &iroh::EndpointAddr,
) -> Result<ServiceConfig, DiscoveryError> {
    let interface_ips = operational_interface_ips(
        if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .map(|interface| {
                let usable = mdns_interface_is_usable(
                    &interface.name,
                    interface.is_oper_up(),
                    interface.is_p2p(),
                );
                (interface.ip(), usable)
            }),
    );
    peer_service_config_with_interfaces(
        service_name,
        instance_name,
        bound_sockets,
        node_addr,
        &interface_ips,
    )
}

#[cfg(feature = "mdns")]
fn mdns_interface_is_usable(name: &str, is_oper_up: bool, is_p2p: bool) -> bool {
    // Keep this predicate aligned with mdns-sd's default interface inventory.
    // Choosing a listener family that the daemon later suppresses would make
    // the advertised SRV port unreachable even when another family is usable.
    is_oper_up && !is_p2p && !name.starts_with("awdl") && !name.starts_with("llw")
}

#[cfg(feature = "mdns")]
fn operational_interface_ips(interfaces: impl IntoIterator<Item = (IpAddr, bool)>) -> Vec<IpAddr> {
    interfaces
        .into_iter()
        .filter_map(|(ip, is_oper_up)| is_oper_up.then_some(ip))
        .collect()
}

#[cfg(feature = "mdns")]
fn peer_service_config_with_interfaces(
    service_name: &str,
    instance_name: &str,
    bound_sockets: &[SocketAddr],
    node_addr: &iroh::EndpointAddr,
    interface_ips: &[IpAddr],
) -> Result<ServiceConfig, DiscoveryError> {
    let socket_addrs: Vec<SocketAddr> = node_addr.ip_addrs().copied().collect();
    let direct = select_advertise_addresses(&socket_addrs, bound_sockets);
    let direct_sockets: Vec<SocketAddr> = direct
        .iter()
        .filter_map(|address| address.parse::<SocketAddr>().ok())
        .collect();
    let interface_is_eligible = |ip: &IpAddr, bound: &SocketAddr| {
        !ip.is_loopback()
            && !ip.is_unspecified()
            && bound.is_ipv6() == ip.is_ipv6()
            && (bound.ip().is_unspecified() || bound.ip() == *ip)
            && !direct_sockets
                .iter()
                .any(|candidate| candidate.ip() == *ip && candidate.port() != bound.port())
    };
    // DNS-SD has one SRV port. Choose the first bound family/port group that
    // has an eligible live interface. This preserves IPv6-only LAN discovery
    // when a required IPv4 wildcard exists but the host has no usable IPv4.
    // Reflexive/NAT ports remain authoritative in TXT only.
    let port = bound_sockets
        .iter()
        .filter(|bound| !is_placeholder_port(bound.port()))
        .find(|bound| {
            interface_ips
                .iter()
                .any(|ip| interface_is_eligible(ip, bound))
        })
        .map(SocketAddr::port)
        .or_else(|| {
            bound_sockets
                .iter()
                .filter(|bound| !is_placeholder_port(bound.port()))
                .find(|bound| {
                    direct_sockets.iter().any(|candidate| {
                        candidate.port() == bound.port()
                            && candidate.is_ipv6() == bound.is_ipv6()
                            && (bound.ip().is_unspecified() || bound.ip() == candidate.ip())
                    })
                })
                .map(SocketAddr::port)
        })
        .or_else(|| {
            bound_sockets
                .iter()
                .find(|bound| !is_placeholder_port(bound.port()))
                .map(SocketAddr::port)
        })
        .ok_or_else(|| {
            DiscoveryError::Setup("endpoint has no dialable bound socket".to_string())
        })?;
    let mut addrs = Vec::new();
    for ip in interface_ips.iter().copied().filter(|ip| {
        bound_sockets
            .iter()
            .any(|bound| bound.port() == port && interface_is_eligible(ip, bound))
    }) {
        if !addrs.contains(&ip) {
            addrs.push(ip);
        }
    }
    if addrs.is_empty() {
        // Interface enumeration can fail on a restricted host. Retain a safe
        // endpoint-backed fallback, but never synthesize a port or family.
        for address in direct_sockets.iter().filter(|address| {
            address.port() == port
                && bound_sockets.iter().any(|bound| {
                    bound.port() == address.port()
                        && bound.is_ipv6() == address.is_ipv6()
                        && (bound.ip().is_unspecified() || bound.ip() == address.ip())
                })
        }) {
            let ip = address.ip();
            if !addrs.contains(&ip) {
                addrs.push(ip);
            }
        }
    }

    let mut txt = vec![(TXT_PK.to_string(), instance_name.to_string())];
    if let Some(relay) = node_addr.relay_urls().next() {
        txt.push((TXT_RELAY.to_string(), relay.to_string()));
    }
    if let Some(direct) = fit_txt_values(TXT_ADDRESS, &direct) {
        txt.push((TXT_ADDRESS.to_string(), direct));
    }

    Ok(ServiceConfig {
        service_name: service_name.to_string(),
        instance_name: instance_name.to_string(),
        port,
        addrs,
        txt,
        protocol: Protocol::Udp,
    })
}

/// Start advertising this node on the local network via DNS-SD.
///
/// Publishes `_<service_name>._udp.local` with the instance name and `pk` TXT
/// set to this node's base32 endpoint id. The single SRV port and its A/AAAA
/// records are selected only from compatible direct candidates; candidates
/// with different per-family or reflexive ports remain authoritative in the
/// plural `address` TXT value. Direct-address or relay changes re-register the
/// same DNS-SD identity; refresh stops on endpoint close or when the returned
/// [`AdvertiseSession`] is dropped.
///
/// For non-iroh services (custom instance names, ports, or TXT), use
/// [`dns_sd::advertise`] directly.
#[cfg(feature = "mdns")]
pub fn advertise_peer(
    ep: &iroh::Endpoint,
    service_name: &str,
) -> Result<AdvertiseSession, DiscoveryError> {
    if ep.is_closed() {
        return Err(DiscoveryError::Setup("endpoint is closed".to_string()));
    }
    let instance = node_id_label(&ep.id());
    let bound_sockets: Vec<SocketAddr> = ep.bound_sockets();
    let mut watcher = ep.watch_addr();
    let initial = watcher.get();
    let config = peer_service_config(service_name, &instance, &bound_sockets, &initial)?;
    // The config already enumerates only endpoint-backed interface families
    // and ports. mdns-sd's unconstrained auto-expansion could add an unbound
    // family, so it stays disabled for the peer specialization.
    let mut session = dns_sd::advertise_selected_addrs(config, false)?;
    let updater = session.updater();
    let refresh_updater = updater.clone();
    let service_name = service_name.to_string();
    let closed = ep.closed();
    let refresh = async move {
        while let Ok(node_addr) = watcher.updated().await {
            let config = match peer_service_config(
                &service_name,
                &instance,
                &bound_sockets,
                &node_addr,
            ) {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!(%error, "iroh-http-discovery: cannot refresh peer advertisement");
                    continue;
                }
            };
            if let Err(error) = refresh_updater.update(config) {
                tracing::warn!(%error, "iroh-http-discovery: peer advertisement refresh failed");
                // A live handle must never silently represent stale TXT. Ending
                // the worker runs its unregister finalizer; a caller can then
                // explicitly start a fresh advertisement.
                break;
            }
        }
    };
    let finish = updater.clone();
    let worker = dns_sd::spawn_refresh_until_closed(closed, refresh, move || {
        // Endpoint close and unexpected watcher termination both retract the
        // registration even if the caller keeps the session object alive.
        finish.unregister();
    })?;
    session.set_refresh_worker(worker);
    // Close may race registration and worker installation. Reject when close
    // won before this start operation's final linearization point; dropping
    // the session cancels the worker and unregisters idempotently.
    if ep.is_closed() {
        drop(session);
        return Err(DiscoveryError::Setup("endpoint is closed".to_string()));
    }
    Ok(session)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_discovery_event_active_construction() {
        let ev = PeerDiscoveryEvent {
            is_active: true,
            node_id: "node123".to_string(),
            addrs: vec![
                "127.0.0.1:4000".to_string(),
                "relay://r.example.com".to_string(),
            ],
        };
        assert!(ev.is_active);
        assert_eq!(ev.node_id, "node123");
        assert_eq!(ev.addrs.len(), 2);
        assert!(ev.addrs.iter().any(|a| a.contains("127.0.0.1")));
    }

    #[test]
    fn peer_discovery_event_expired_construction() {
        let ev = PeerDiscoveryEvent {
            is_active: false,
            node_id: "expired_node".to_string(),
            addrs: vec![],
        };
        assert!(!ev.is_active);
        assert_eq!(ev.node_id, "expired_node");
        assert!(ev.addrs.is_empty(), "expired events carry no addresses");
    }

    #[test]
    fn peer_discovery_event_clone_preserves_all_fields() {
        let original = PeerDiscoveryEvent {
            is_active: true,
            node_id: "abc".to_string(),
            addrs: vec!["10.0.0.1:1234".to_string()],
        };
        let cloned = original.clone();
        assert_eq!(cloned.is_active, original.is_active);
        assert_eq!(cloned.node_id, original.node_id);
        assert_eq!(cloned.addrs, original.addrs);
    }

    /// Verify mpsc channel semantics that `BrowseSession` relies on:
    /// when the sender is dropped the receiver's `recv()` returns `None`.
    /// This is the core invariant that `next_event()` depends on for clean shutdown.
    #[tokio::test]
    async fn channel_close_on_sender_drop() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        drop(tx);
        assert!(
            rx.recv().await.is_none(),
            "recv() must return None when all senders are dropped"
        );
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn endpoint_lookup_provider_is_singleton_per_live_registry_and_key_reuse_is_fresh() {
        use iroh::{address_lookup::AddressLookupServices, SecretKey};

        let endpoint_id = SecretKey::from_bytes(&[11; 32]).public();
        let services = AddressLookupServices::default();
        let reused_key = &services as *const AddressLookupServices as usize;
        let lookup = endpoint_lookup_for_key(reused_key, &services);
        let same_lookup = endpoint_lookup_for_key(reused_key, &services);
        assert_eq!(services.len(), 1, "repeated browse setup adds one provider");

        let source = lookup.new_source();
        source
            .upsert(
                "same-node",
                &endpoint_id.to_string(),
                &["10.0.0.1:4433".to_string()],
            )
            .unwrap();
        assert!(same_lookup.resolve(endpoint_id).is_some());

        let other_services = AddressLookupServices::default();
        let other_lookup = endpoint_lookup(&other_services);
        assert_eq!(other_services.len(), 1);
        assert!(
            other_lookup.resolve(endpoint_id).is_none(),
            "two simultaneously live endpoints with the same node id must not share discovery state"
        );

        drop(services);
        let replacement_services = AddressLookupServices::default();
        let replacement_lookup = endpoint_lookup_for_key(reused_key, &replacement_services);
        assert_eq!(replacement_services.len(), 1);
        assert!(
            replacement_lookup.resolve(endpoint_id).is_none(),
            "allocator key reuse must install a fresh provider even while an old browse source exists"
        );
        source.retire();
    }

    #[cfg(feature = "mdns")]
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires local UDP socket binding; run explicitly on a network-capable host"]
    async fn actual_same_id_endpoints_have_distinct_singleton_providers() {
        use iroh::{
            address_lookup::AddressLookup, endpoint::presets, Endpoint, RelayMode, SecretKey,
        };

        let secret = SecretKey::from_bytes(&[14; 32]);
        let endpoint_a = Endpoint::builder(presets::Minimal)
            .secret_key(secret.clone())
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let endpoint_b = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        assert_eq!(endpoint_a.id(), endpoint_b.id());

        let services_a = endpoint_a.address_lookup().unwrap();
        let services_b = endpoint_b.address_lookup().unwrap();
        let baseline_a = services_a.len();
        let baseline_b = services_b.len();
        assert!(matches!(
            browse_peers(&endpoint_a, "invalid.service").await,
            Err(DiscoveryError::InvalidServiceName(_))
        ));
        assert_eq!(
            services_a.len(),
            baseline_a,
            "failed browse setup must not install a provider"
        );

        let lookup_a = endpoint_lookup(services_a);
        let same_lookup_a = endpoint_lookup(services_a);
        let lookup_b = endpoint_lookup(services_b);
        assert_eq!(
            services_a.len(),
            baseline_a + 1,
            "repeated setup on one actual endpoint reuses its provider"
        );
        assert_eq!(services_b.len(), baseline_b + 1);

        let discovered_id = SecretKey::from_bytes(&[15; 32]).public();
        let source_a = lookup_a.new_source();
        source_a
            .upsert(
                "peer",
                &discovered_id.to_string(),
                &["10.0.0.15:4433".to_string()],
            )
            .unwrap();
        assert!(same_lookup_a.resolve(discovered_id).is_some());
        assert!(
            lookup_b.resolve(discovered_id).is_none(),
            "same-id live endpoints must not share provider state"
        );

        source_a.retire();
        endpoint_a.close().await;
        endpoint_b.close().await;
    }

    #[cfg(feature = "mdns")]
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires local UDP socket binding; run explicitly on a network-capable host"]
    async fn peer_advertise_rejects_an_already_closed_endpoint() {
        use iroh::{endpoint::presets, Endpoint, RelayMode};

        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        endpoint.close().await;

        assert!(matches!(
            advertise_peer(&endpoint, "closed-endpoint"),
            Err(DiscoveryError::Setup(message)) if message == "endpoint is closed"
        ));
    }

    #[cfg(feature = "mdns")]
    #[tokio::test]
    async fn endpoint_close_wakes_peer_next_and_retires_its_source() {
        use iroh::{address_lookup::AddressLookup, SecretKey};

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let endpoint_id = SecretKey::from_bytes(&[12; 32]).public();
        source
            .upsert(
                "peer-instance",
                &endpoint_id.to_string(),
                &["10.0.0.12:4433".to_string()],
            )
            .unwrap();
        let (inner, _finish) = dns_sd::controlled_test_browse();
        let shared = PeerBrowseShared::new(inner, source);
        let (endpoint_close_tx, endpoint_close_rx) = futures::channel::oneshot::channel();
        let weak = Arc::downgrade(&shared);
        let endpoint_task = tokio::spawn(async move {
            let _ = endpoint_close_rx.await;
            if let Some(shared) = weak.upgrade() {
                shared.close();
            }
        });
        shared.install_endpoint_task(endpoint_task.abort_handle());
        drop(endpoint_task);
        let session = Arc::new(BrowseSession { shared });
        let waiting = tokio::spawn({
            let session = session.clone();
            async move { session.next_event().await }
        });
        tokio::task::yield_now().await;

        endpoint_close_tx.send(()).unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("endpoint close must wake peer next_event")
            .unwrap();
        assert!(result.is_none());
        assert!(
            lookup.resolve(endpoint_id).is_none(),
            "endpoint close must synchronously retract browse contributions"
        );
        session.close();
    }

    #[cfg(feature = "mdns")]
    #[tokio::test]
    async fn unexpected_service_terminal_retires_peer_source() {
        use iroh::{address_lookup::AddressLookup, SecretKey};

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let endpoint_id = SecretKey::from_bytes(&[13; 32]).public();
        source
            .upsert(
                "peer-instance",
                &endpoint_id.to_string(),
                &["10.0.0.13:4433".to_string()],
            )
            .unwrap();
        let (inner, finish) = dns_sd::controlled_test_browse();
        let session = Arc::new(BrowseSession {
            shared: PeerBrowseShared::new(inner, source),
        });
        finish.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while lookup.resolve(endpoint_id).is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("service terminal must retire the source without a next_event poll");
        assert!(lookup.resolve(endpoint_id).is_none());
        assert!(session.next_event().await.is_none());
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn peer_event_from_record_prefers_pk_txt_over_instance() {
        let rec = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "fallback-instance".to_string(),
            host: Some("host.local.".to_string()),
            port: 4433,
            addrs: vec!["192.168.1.5:4433".parse().unwrap()],
            txt: vec![
                (TXT_PK.to_string(), "pk-node-id".to_string()),
                (TXT_RELAY.to_string(), "https://relay.example".to_string()),
            ],
        };
        let ev = peer_event_from_record(&rec).expect("event");
        assert!(ev.is_active);
        assert_eq!(ev.node_id, "pk-node-id");
        assert!(ev.addrs.iter().any(|a| a == "192.168.1.5:4433"));
        assert!(ev.addrs.iter().any(|a| a == "https://relay.example"));
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn peer_event_from_record_surfaces_address_txt() {
        // Regression: #346 — an advertiser (iOS native) whose SRV port is not
        // the QUIC port publishes the dialable `ip:port` in the `address` TXT.
        // The browse event must surface it so the peer can be dialed over LAN.
        let rec = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "inst".to_string(),
            host: None,
            port: 0,
            addrs: vec![],
            txt: vec![
                (TXT_PK.to_string(), "pk-node-id".to_string()),
                (TXT_ADDRESS.to_string(), "192.168.50.227:59234".to_string()),
            ],
        };
        let ev = peer_event_from_record(&rec).expect("event");
        assert!(
            ev.addrs.iter().any(|a| a == "192.168.50.227:59234"),
            "address TXT must be surfaced as a direct address, got {:?}",
            ev.addrs
        );
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn peer_event_splits_plural_address_txt_without_losing_fallbacks() {
        let rec = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "inst".to_string(),
            host: Some("peer.local.".to_string()),
            port: 6000,
            addrs: vec!["10.0.0.10:6000".parse().unwrap()],
            txt: vec![
                (TXT_PK.to_string(), "pk-node-id".to_string()),
                (
                    TXT_ADDRESS.to_string(),
                    " 192.168.1.2:4433,invalid,[fd00::2]:4434,192.168.1.9:0,192.168.1.2:4433 "
                        .to_string(),
                ),
                (TXT_RELAY.to_string(), "https://relay.example".to_string()),
            ],
        };

        let ev = peer_event_from_record(&rec).expect("event");
        assert_eq!(
            ev.addrs,
            [
                "10.0.0.10:6000",
                "192.168.1.2:4433",
                "[fd00::2]:4434",
                "https://relay.example",
            ],
            "valid TXT members must be canonicalized and deduplicated while SRV and relay fallbacks survive"
        );
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn authoritative_txt_port_suppresses_a_conflicting_srv_path_in_the_lookup() {
        use futures::StreamExt;
        use iroh::{address_lookup::AddressLookup, SecretKey, TransportAddr};

        let endpoint_id = SecretKey::from_bytes(&[16; 32]).public();
        let record = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "peer".to_string(),
            host: Some("peer.local.".to_string()),
            port: 59234,
            addrs: vec!["10.8.0.7:59234".parse().unwrap()],
            txt: vec![
                (TXT_PK.to_string(), endpoint_id.to_string()),
                (TXT_ADDRESS.to_string(), "10.8.0.7:45555".to_string()),
            ],
        };
        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let event = apply_one_peer_record(&source, &record);
        assert_eq!(event.addrs, ["10.8.0.7:45555"]);

        let mut resolved = lookup.resolve(endpoint_id).expect("lookup result");
        let item = futures::executor::block_on(async { resolved.next().await })
            .expect("one item")
            .expect("successful item");
        let direct: Vec<_> = item
            .endpoint_info()
            .addrs()
            .filter_map(|address| match address {
                TransportAddr::Ip(address) => Some(address.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(direct, ["10.8.0.7:45555"]);
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn conflicting_ipv6_ports_are_compared_with_interface_scope() {
        let record = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "peer".to_string(),
            host: Some("peer.local.".to_string()),
            port: 5353,
            addrs: vec!["[fe80::1%4]:5353".parse().unwrap()],
            txt: vec![
                (TXT_PK.to_string(), "peer".to_string()),
                (TXT_ADDRESS.to_string(), "[fe80::1%5]:4433".to_string()),
            ],
        };

        let event = peer_event_from_record(&record).expect("peer record");

        assert_eq!(event.addrs, ["[fe80::1%4]:5353", "[fe80::1%5]:4433"]);
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn unscoped_link_local_txt_cannot_suppress_a_scoped_srv_path() {
        let record = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "peer".to_string(),
            host: Some("peer.local.".to_string()),
            port: 5353,
            addrs: vec!["[fe80::1%4]:5353".parse().unwrap()],
            txt: vec![
                (TXT_PK.to_string(), "peer".to_string()),
                (TXT_ADDRESS.to_string(), "[fe80::1]:4433".to_string()),
            ],
        };

        let event = peer_event_from_record(&record).expect("peer record");

        assert_eq!(event.addrs, ["[fe80::1%4]:5353"]);
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn incidental_scope_does_not_split_a_ula_host_identity() {
        let record = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "peer".to_string(),
            host: Some("peer.local.".to_string()),
            port: 5353,
            addrs: vec!["[fd00::1%4]:5353".parse().unwrap()],
            txt: vec![
                (TXT_PK.to_string(), "peer".to_string()),
                (TXT_ADDRESS.to_string(), "[fd00::1]:4433".to_string()),
            ],
        };

        let event = peer_event_from_record(&record).expect("peer record");

        assert_eq!(event.addrs, ["[fd00::1]:4433"]);
    }

    #[cfg(feature = "mdns")]
    fn apply_one_peer_record(
        source: &AddressLookupSource,
        record: &ServiceRecord,
    ) -> PeerDiscoveryEvent {
        let mut events = apply_peer_record(source, record);
        assert_eq!(events.len(), 1, "expected exactly one logical peer event");
        events.pop().expect("one logical peer event")
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn peer_browse_state_is_scoped_by_instance_and_browse_lifetime() {
        use futures::StreamExt;
        use iroh::{address_lookup::AddressLookup, SecretKey, TransportAddr};
        use iroh_http_core::SourceScopedAddressLookup;

        fn direct_addrs(
            lookup: &SourceScopedAddressLookup,
            endpoint_id: iroh::EndpointId,
        ) -> Vec<String> {
            let Some(mut resolved) = lookup.resolve(endpoint_id) else {
                return Vec::new();
            };
            let item = futures::executor::block_on(async { resolved.next().await })
                .expect("one item")
                .expect("successful lookup");
            item.endpoint_info()
                .addrs()
                .filter_map(|addr| match addr {
                    TransportAddr::Ip(addr) => Some(addr.to_string()),
                    _ => None,
                })
                .collect()
        }

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let endpoint_id = SecretKey::from_bytes(&[8; 32]).public();
        let node_id = endpoint_id.to_string();
        let active = |instance: &str, addr: &str| ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: instance.to_string(),
            host: Some(format!("{instance}.local.")),
            port: addr.parse::<SocketAddr>().unwrap().port(),
            addrs: vec![addr.parse().unwrap()],
            txt: vec![(TXT_PK.to_string(), node_id.clone())],
        };

        let old = apply_one_peer_record(&source, &active("old-instance", "10.1.0.1:4433"));
        assert_eq!(old.addrs, ["10.1.0.1:4433"]);
        let new = apply_one_peer_record(&source, &active("new-instance", "10.1.0.2:4433"));
        let mut new_addrs = new.addrs;
        new_addrs.sort();
        assert_eq!(
            new_addrs,
            ["10.1.0.1:4433", "10.1.0.2:4433"],
            "active events expose the effective union for snapshot-style consumers"
        );
        let remaining = apply_one_peer_record(
            &source,
            &ServiceRecord {
                is_active: false,
                service_type: "_iroh-http._udp.local.".to_string(),
                instance_name: "old-instance".to_string(),
                host: None,
                port: 0,
                addrs: Vec::new(),
                txt: Vec::new(),
            },
        );

        assert_eq!(
            remaining.node_id,
            iroh_http_core::base32_encode(endpoint_id.as_bytes())
        );
        assert!(
            remaining.is_active,
            "one expired instance must not expire a peer with another live instance"
        );
        assert_eq!(remaining.addrs, ["10.1.0.2:4433"]);
        assert_eq!(direct_addrs(&lookup, endpoint_id), ["10.1.0.2:4433"]);
        drop(source);
        assert!(lookup.resolve(endpoint_id).is_none());
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn invalid_instance_update_retracts_stale_lookup_and_emits_expiry() {
        use iroh::{address_lookup::AddressLookup, SecretKey};

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let endpoint_id = SecretKey::from_bytes(&[10; 32]).public();
        let active = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "stable-instance".to_string(),
            host: Some("peer.local.".to_string()),
            port: 4433,
            addrs: vec!["10.1.0.3:4433".parse().unwrap()],
            txt: vec![(TXT_PK.to_string(), endpoint_id.to_string())],
        };
        apply_one_peer_record(&source, &active);

        let invalid = ServiceRecord {
            txt: vec![(TXT_PK.to_string(), "invalid-node-id".to_string())],
            ..active
        };
        let removal = apply_one_peer_record(&source, &invalid);
        assert_eq!(
            removal.node_id,
            iroh_http_core::base32_encode(endpoint_id.as_bytes())
        );
        assert!(!removal.is_active);
        assert!(removal.addrs.is_empty());
        assert!(
            lookup.resolve(endpoint_id).is_none(),
            "malformed authoritative update must retract the stale instance"
        );
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn invalid_instance_update_emits_the_remaining_effective_union() {
        use iroh::SecretKey;

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let endpoint_id = SecretKey::from_bytes(&[11; 32]).public();
        let active = |instance_name: &str, addr: &str| ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: instance_name.to_string(),
            host: Some("peer.local.".to_string()),
            port: 4433,
            addrs: vec![addr.parse().unwrap()],
            txt: vec![(TXT_PK.to_string(), endpoint_id.to_string())],
        };
        apply_one_peer_record(&source, &active("stable-instance", "10.1.0.3:4433"));
        apply_one_peer_record(&source, &active("other-instance", "10.1.0.4:4433"));

        let invalid = ServiceRecord {
            txt: vec![(TXT_PK.to_string(), "invalid-node-id".to_string())],
            ..active("stable-instance", "10.1.0.5:4433")
        };
        let replacement = apply_one_peer_record(&source, &invalid);

        assert_eq!(
            replacement.node_id,
            iroh_http_core::base32_encode(endpoint_id.as_bytes())
        );
        assert!(replacement.is_active);
        assert_eq!(replacement.addrs, ["10.1.0.4:4433"]);
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn valid_instance_identity_change_emits_old_transition_then_new_union() {
        use iroh::SecretKey;

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let old_id = SecretKey::from_bytes(&[12; 32]).public();
        let new_id = SecretKey::from_bytes(&[13; 32]).public();
        let active = |node_id: iroh::EndpointId, addr: &str| ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "stable-instance".to_string(),
            host: Some("peer.local.".to_string()),
            port: 4433,
            addrs: vec![addr.parse().unwrap()],
            txt: vec![(TXT_PK.to_string(), node_id.to_string())],
        };
        apply_one_peer_record(&source, &active(old_id, "10.1.0.12:4433"));

        let events = apply_peer_record(&source, &active(new_id, "10.1.0.13:4433"));

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            PeerDiscoveryEvent {
                is_active: false,
                node_id: iroh_http_core::base32_encode(old_id.as_bytes()),
                addrs: Vec::new(),
            }
        );
        assert_eq!(
            events[1],
            PeerDiscoveryEvent {
                is_active: true,
                node_id: iroh_http_core::base32_encode(new_id.as_bytes()),
                addrs: vec!["10.1.0.13:4433".to_string()],
            }
        );
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn encoding_only_node_id_update_keeps_one_canonical_peer_key() {
        use iroh::SecretKey;

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let endpoint_id = SecretKey::from_bytes(&[17; 32]).public();
        let canonical = iroh_http_core::base32_encode(endpoint_id.as_bytes());
        let active = |node_id: String, addr: &str| ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "stable-instance".to_string(),
            host: Some("peer.local.".to_string()),
            port: 4433,
            addrs: vec![addr.parse().unwrap()],
            txt: vec![(TXT_PK.to_string(), node_id)],
        };

        let first =
            apply_one_peer_record(&source, &active(endpoint_id.to_string(), "10.1.0.17:4433"));
        let second = apply_peer_record(
            &source,
            &active(canonical.to_ascii_uppercase(), "10.1.0.18:4433"),
        );

        assert_eq!(first.node_id, canonical);
        assert_eq!(second.len(), 1, "semantic A→A must not synthesize expiry");
        assert_eq!(second[0].node_id, canonical);
        assert_eq!(second[0].addrs, ["10.1.0.18:4433"]);
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn active_events_do_not_leak_contributions_from_other_browse_sources() {
        use iroh::SecretKey;

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source_a = lookup.new_source();
        let source_b = lookup.new_source();
        let endpoint_id = SecretKey::from_bytes(&[18; 32]).public();
        let active = |instance: &str, addr: &str| ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: instance.to_string(),
            host: Some("peer.local.".to_string()),
            port: 4433,
            addrs: vec![addr.parse().unwrap()],
            txt: vec![(TXT_PK.to_string(), endpoint_id.to_string())],
        };

        apply_one_peer_record(&source_a, &active("source-a", "10.1.0.18:4433"));
        let source_b_event =
            apply_one_peer_record(&source_b, &active("source-b", "10.1.0.19:4433"));

        assert_eq!(source_b_event.addrs, ["10.1.0.19:4433"]);
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn valid_identity_change_keeps_an_overlapping_old_node_active() {
        use iroh::SecretKey;

        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let source = lookup.new_source();
        let old_id = SecretKey::from_bytes(&[14; 32]).public();
        let new_id = SecretKey::from_bytes(&[15; 32]).public();
        let active = |instance: &str, node_id: iroh::EndpointId, addr: &str| ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: instance.to_string(),
            host: Some("peer.local.".to_string()),
            port: 4433,
            addrs: vec![addr.parse().unwrap()],
            txt: vec![(TXT_PK.to_string(), node_id.to_string())],
        };
        apply_one_peer_record(
            &source,
            &active("stable-instance", old_id, "10.1.0.14:4433"),
        );
        apply_one_peer_record(
            &source,
            &active("other-instance", old_id, "10.1.0.140:4433"),
        );

        let events = apply_peer_record(
            &source,
            &active("stable-instance", new_id, "10.1.0.15:4433"),
        );

        assert_eq!(events.len(), 2);
        assert!(events[0].is_active);
        assert_eq!(
            events[0].node_id,
            iroh_http_core::base32_encode(old_id.as_bytes())
        );
        assert_eq!(events[0].addrs, ["10.1.0.140:4433"]);
        assert_eq!(
            events[1].node_id,
            iroh_http_core::base32_encode(new_id.as_bytes())
        );
    }

    #[cfg(feature = "mdns")]
    #[tokio::test]
    async fn closing_peer_browse_discards_queued_identity_transition() {
        let (inner, _finish) = dns_sd::controlled_test_browse();
        let lookup = SourceScopedAddressLookup::new("test-mdns");
        let shared = PeerBrowseShared::new(inner, lookup.new_source());
        shared
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_back(PeerDiscoveryEvent {
                is_active: true,
                node_id: "queued-new-node".to_string(),
                addrs: vec!["10.1.0.16:4433".to_string()],
            });
        let session = BrowseSession { shared };

        session.close();

        assert!(session.next_event().await.is_none());
        assert!(session
            .shared
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty());
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn peer_event_from_record_falls_back_to_instance_name() {
        let rec = ServiceRecord {
            is_active: true,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "instance-id".to_string(),
            host: None,
            port: 0,
            addrs: vec![],
            txt: vec![],
        };
        let ev = peer_event_from_record(&rec).expect("event");
        assert_eq!(ev.node_id, "instance-id");
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn peer_event_from_record_expired_carries_no_addresses() {
        let rec = ServiceRecord {
            is_active: false,
            service_type: "_iroh-http._udp.local.".to_string(),
            instance_name: "gone".to_string(),
            host: None,
            port: 0,
            addrs: vec![],
            txt: vec![],
        };
        let ev = peer_event_from_record(&rec).expect("event");
        assert!(!ev.is_active);
        assert_eq!(ev.node_id, "gone");
        assert!(ev.addrs.is_empty());
    }

    #[test]
    fn select_advertise_address_pairs_routable_ip_with_bound_port() {
        // Regression: #350 review W1 — a desktop advertiser must publish a
        // complete dialable `ip:port` in the `address` TXT so mobile
        // browsers (iOS NWBrowser can't resolve SRV/A at all; Android
        // browse_peers reads only the `address`/`relay` attrs) can direct-dial
        // it over the LAN instead of falling back to relay-only.
        let ip_addrs = vec![
            "127.0.0.1:1".parse().unwrap(),
            "192.168.1.42:1".parse().unwrap(),
        ];
        let bound = vec!["0.0.0.0:59234".parse().unwrap()];
        let addr = select_advertise_address(&ip_addrs, &bound)
            .expect("a routable address must be selected");
        assert_eq!(addr, "192.168.1.42:59234");
    }

    #[test]
    fn select_advertise_address_preserves_real_reflexive_port() {
        // A reflexive QAD candidate carries the public port observed by the
        // relay probe. It intentionally differs from the local listener port
        // and must be advertised verbatim.
        let ip_addrs = vec!["192.26.168.188:40349".parse().unwrap()];
        let bound = vec!["0.0.0.0:62546".parse().unwrap()];

        assert_eq!(
            select_advertise_address(&ip_addrs, &bound).as_deref(),
            Some("192.26.168.188:40349")
        );
    }

    #[test]
    fn select_advertise_addresses_keeps_all_dialable_interfaces() {
        let ip_addrs = vec![
            "127.0.0.1:1".parse().unwrap(),
            "192.168.1.42:1".parse().unwrap(),
            "10.8.0.7:1".parse().unwrap(),
            "[fd00::7]:1".parse().unwrap(),
            "198.51.100.10:40349".parse().unwrap(),
            "192.168.1.42:1".parse().unwrap(),
        ];
        let bound = vec![
            "0.0.0.0:59234".parse().unwrap(),
            "[::]:60000".parse().unwrap(),
        ];

        assert_eq!(
            select_advertise_addresses(&ip_addrs, &bound),
            [
                "192.168.1.42:59234",
                "10.8.0.7:59234",
                "[fd00::7]:60000",
                "198.51.100.10:40349",
            ],
            "LAN, VPN, IPv6 ULA, and reflexive candidates must remain plural"
        );
    }

    #[test]
    fn peer_service_config_tracks_plural_endpoint_snapshots() {
        use iroh::{EndpointAddr, SecretKey, TransportAddr};

        let endpoint_id = SecretKey::from_bytes(&[9; 32]).public();
        let bound = vec![
            "0.0.0.0:59234".parse().unwrap(),
            "[::]:60000".parse().unwrap(),
        ];
        let initial = EndpointAddr::from_parts(
            endpoint_id,
            [
                TransportAddr::Ip("192.168.1.42:1".parse().unwrap()),
                TransportAddr::Ip("10.8.0.7:45555".parse().unwrap()),
                TransportAddr::Relay("https://relay-one.example".parse().unwrap()),
            ],
        );
        let updated = EndpointAddr::from_parts(
            endpoint_id,
            [
                TransportAddr::Ip("[fd00::7]:1".parse().unwrap()),
                TransportAddr::Relay("https://relay-two.example".parse().unwrap()),
            ],
        );

        let interfaces = vec![
            "192.168.1.42".parse().unwrap(),
            "10.8.0.7".parse().unwrap(),
            "fd00::7".parse().unwrap(),
        ];
        let initial =
            peer_service_config_with_interfaces("iroh-http", "peer", &bound, &initial, &interfaces)
                .unwrap();
        let updated =
            peer_service_config_with_interfaces("iroh-http", "peer", &bound, &updated, &interfaces)
                .unwrap();
        let initial_direct: std::collections::BTreeSet<_> = initial
            .txt
            .iter()
            .find(|(key, _)| key == TXT_ADDRESS)
            .expect("address TXT")
            .1
            .split(',')
            .collect();
        assert_eq!(
            initial_direct,
            ["192.168.1.42:59234", "10.8.0.7:45555"]
                .into_iter()
                .collect()
        );
        let initial_srv_sockets: Vec<_> = initial
            .addrs
            .iter()
            .map(|ip| SocketAddr::new(*ip, initial.port).to_string())
            .collect();
        assert!(!initial_srv_sockets.is_empty());
        assert!(initial_srv_sockets
            .iter()
            .all(|socket| initial_direct.contains(socket.as_str())));
        assert!(!initial_srv_sockets.contains(&"10.8.0.7:59234".to_string()));
        assert!(!initial_srv_sockets.contains(&"192.168.1.42:45555".to_string()));
        assert!(initial
            .txt
            .iter()
            .any(|(key, value)| key == TXT_RELAY && value == "https://relay-one.example/"));
        assert_eq!(
            updated
                .txt
                .iter()
                .find(|(key, _)| key == TXT_ADDRESS)
                .map(|(_, value)| value.as_str()),
            Some("[fd00::7]:60000")
        );
        assert_eq!(updated.port, 59234);
        assert_eq!(
            updated.addrs,
            [
                "192.168.1.42".parse::<IpAddr>().unwrap(),
                "10.8.0.7".parse::<IpAddr>().unwrap(),
            ]
        );
        assert_ne!(initial, updated);
    }

    #[test]
    fn peer_service_config_keeps_sorting_earlier_reflexive_paths_txt_only() {
        use iroh::{EndpointAddr, SecretKey, TransportAddr};

        let endpoint_id = SecretKey::from_bytes(&[19; 32]).public();
        let bound = vec![
            "0.0.0.0:62546".parse().unwrap(),
            "[::]:60000".parse().unwrap(),
        ];
        let node_addr = EndpointAddr::from_parts(
            endpoint_id,
            [
                TransportAddr::Ip("1.2.3.4:40349".parse().unwrap()),
                TransportAddr::Ip("192.168.1.42:62546".parse().unwrap()),
            ],
        );

        let config = peer_service_config_with_interfaces(
            "iroh-http",
            "peer",
            &bound,
            &node_addr,
            &["192.168.1.42".parse().unwrap()],
        )
        .unwrap();
        let direct = config
            .txt
            .iter()
            .find(|(key, _)| key == TXT_ADDRESS)
            .map(|(_, value)| value.as_str())
            .expect("address TXT");

        assert_eq!(config.port, 62546);
        assert_eq!(
            config.addrs,
            ["192.168.1.42".parse::<IpAddr>().unwrap()],
            "only a same-family local listener path belongs in the SRV/A group"
        );
        assert!(direct.contains("1.2.3.4:40349"));
        assert!(direct.contains("192.168.1.42:62546"));
    }

    #[test]
    fn ipv4_only_peer_listener_never_advertises_host_ipv6_interfaces() {
        use iroh::{EndpointAddr, SecretKey, TransportAddr};

        let endpoint_id = SecretKey::from_bytes(&[20; 32]).public();
        let bound = vec!["0.0.0.0:62546".parse().unwrap()];
        let node_addr = EndpointAddr::from_parts(
            endpoint_id,
            [TransportAddr::Ip("192.168.1.42:62546".parse().unwrap())],
        );

        let config = peer_service_config_with_interfaces(
            "iroh-http",
            "peer",
            &bound,
            &node_addr,
            &["192.168.1.42".parse().unwrap(), "fd00::20".parse().unwrap()],
        )
        .unwrap();

        assert_eq!(config.addrs, ["192.168.1.42".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn split_listener_selects_ipv6_srv_group_on_an_ipv6_only_lan() {
        use iroh::{EndpointAddr, SecretKey, TransportAddr};

        let endpoint_id = SecretKey::from_bytes(&[21; 32]).public();
        let bound = vec![
            "0.0.0.0:62546".parse().unwrap(),
            "[::]:60000".parse().unwrap(),
        ];
        let node_addr = EndpointAddr::from_parts(
            endpoint_id,
            [TransportAddr::Ip("[fd00::21]:60000".parse().unwrap())],
        );

        let config = peer_service_config_with_interfaces(
            "iroh-http",
            "peer",
            &bound,
            &node_addr,
            &["fd00::21".parse().unwrap()],
        )
        .unwrap();

        assert_eq!(config.port, 60000);
        assert_eq!(config.addrs, ["fd00::21".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn down_ipv4_interface_cannot_hide_an_operational_ipv6_srv_group() {
        use iroh::{EndpointAddr, SecretKey, TransportAddr};

        let endpoint_id = SecretKey::from_bytes(&[22; 32]).public();
        let bound = vec![
            "0.0.0.0:62546".parse().unwrap(),
            "[::]:60000".parse().unwrap(),
        ];
        let node_addr = EndpointAddr::from_parts(
            endpoint_id,
            [TransportAddr::Ip("[fd00::22]:60000".parse().unwrap())],
        );
        let interfaces = operational_interface_ips([
            ("192.168.1.22".parse().unwrap(), false),
            ("fd00::22".parse().unwrap(), true),
        ]);

        let config = peer_service_config_with_interfaces(
            "iroh-http",
            "peer",
            &bound,
            &node_addr,
            &interfaces,
        )
        .unwrap();

        assert_eq!(config.port, 60000);
        assert_eq!(config.addrs, ["fd00::22".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn peer_interface_inventory_matches_mdns_daemon_filters() {
        assert!(mdns_interface_is_usable("en0", true, false));
        assert!(!mdns_interface_is_usable("en0", false, false));
        assert!(!mdns_interface_is_usable("utun3", true, true));
        assert!(!mdns_interface_is_usable("awdl0", true, false));
        assert!(!mdns_interface_is_usable("llw0", true, false));
    }

    #[test]
    fn srv_selection_skips_placeholder_bound_ports() {
        use iroh::{EndpointAddr, SecretKey, TransportAddr};

        let endpoint_id = SecretKey::from_bytes(&[23; 32]).public();
        let bound = vec!["0.0.0.0:1".parse().unwrap(), "[::]:60000".parse().unwrap()];
        let node_addr = EndpointAddr::from_parts(
            endpoint_id,
            [TransportAddr::Ip("[fd00::23]:60000".parse().unwrap())],
        );

        let config = peer_service_config_with_interfaces(
            "iroh-http",
            "peer",
            &bound,
            &node_addr,
            &["fd00::23".parse().unwrap()],
        )
        .unwrap();

        assert_eq!(config.port, 60000);
        assert_eq!(config.addrs, ["fd00::23".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn address_txt_subset_honors_the_exact_dns_sd_entry_boundary() {
        let exact = vec!["a".repeat(120), "b".repeat(126), "z".to_string()];
        let value = fit_txt_values(TXT_ADDRESS, &exact).unwrap();
        assert_eq!(value.len(), 247);
        assert_eq!(TXT_ADDRESS.len() + 1 + value.len(), 255);
        assert!(
            !value.contains('z'),
            "the overflowing member is omitted whole"
        );

        let oversized_then_fitting = vec!["x".repeat(248), "ok".to_string()];
        assert_eq!(
            fit_txt_values(TXT_ADDRESS, &oversized_then_fitting).as_deref(),
            Some("ok"),
            "selection skips an oversized member without truncating it or hiding later fits"
        );
    }

    #[test]
    fn select_advertise_address_skips_loopback_and_unspecified() {
        let ip_addrs = vec![
            "127.0.0.1:5000".parse().unwrap(),
            "0.0.0.0:5000".parse().unwrap(),
            "[::1]:5000".parse().unwrap(),
        ];
        let bound = vec!["0.0.0.0:59234".parse().unwrap()];
        assert_eq!(select_advertise_address(&ip_addrs, &bound), None);
    }

    #[test]
    fn select_advertise_address_brackets_ipv6() {
        let ip_addrs = vec!["[2001:db8::1]:1".parse().unwrap()];
        let bound = vec!["[::]:443".parse().unwrap()];
        let addr = select_advertise_address(&ip_addrs, &bound).expect("address");
        assert_eq!(addr, "[2001:db8::1]:443");
    }

    #[test]
    fn select_advertise_address_skips_link_local() {
        // #350 review L3 — an IPv4 link-local (169.254/16) or IPv6 link-local
        // (fe80::/10) is unroutable off-link; a browsing peer handed it as the
        // dialable `address` fails the direct dial. Prefer a real LAN address.
        let ip_addrs = vec![
            "169.254.10.1:1".parse().unwrap(),
            "[fe80::1]:1".parse().unwrap(),
            "192.168.1.9:1".parse().unwrap(),
        ];
        let bound = vec!["0.0.0.0:443".parse().unwrap()];
        let addr = select_advertise_address(&ip_addrs, &bound).expect("address");
        assert_eq!(addr, "192.168.1.9:443");
    }

    #[test]
    fn select_advertise_address_none_when_only_link_local() {
        let ip_addrs = vec![
            "169.254.10.1:1".parse().unwrap(),
            "[fe80::1]:1".parse().unwrap(),
        ];
        let bound = vec!["0.0.0.0:443".parse().unwrap()];
        assert_eq!(select_advertise_address(&ip_addrs, &bound), None);
    }

    // Regression: #350 F14 — a dual-stack node can bind IPv6 on one port and
    // IPv4 on another. The advertised IP must be paired with a bound port of
    // the SAME family; pairing an IPv4 IP with the IPv6 socket's port publishes
    // an undialable address.
    #[test]
    fn select_advertise_address_pairs_same_family_port() {
        let ip_addrs = vec!["192.168.1.42:1".parse().unwrap()];
        // IPv6 socket listed first, IPv4 second — must pick the IPv4 port.
        let bound = vec![
            "[::]:60000".parse().unwrap(),
            "0.0.0.0:59234".parse().unwrap(),
        ];
        let addr = select_advertise_address(&ip_addrs, &bound).expect("address");
        assert_eq!(addr, "192.168.1.42:59234");
    }

    // Regression: #350 F14 — when no same-family bound port exists for a
    // routable IP, publish nothing rather than a cross-family (undialable)
    // pairing.
    #[test]
    fn select_advertise_address_none_when_no_same_family_port() {
        let ip_addrs = vec!["192.168.1.42:1".parse().unwrap()];
        let bound = vec!["[::]:60000".parse().unwrap()];
        assert_eq!(select_advertise_address(&ip_addrs, &bound), None);
    }

    // Regression: #350 F5/F14 — a placeholder bound port (0/1) must never be
    // borrowed; fall through to a real same-family socket.
    #[test]
    fn select_advertise_address_skips_placeholder_bound_port() {
        let ip_addrs = vec!["192.168.1.42:1".parse().unwrap()];
        let bound = vec![
            "0.0.0.0:1".parse().unwrap(),
            "0.0.0.0:59234".parse().unwrap(),
        ];
        let addr = select_advertise_address(&ip_addrs, &bound).expect("address");
        assert_eq!(addr, "192.168.1.42:59234");
    }
}
