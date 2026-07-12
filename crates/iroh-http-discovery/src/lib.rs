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
mod address_lookup;

#[cfg(feature = "mdns")]
use std::net::{IpAddr, SocketAddr};

#[cfg(feature = "mdns")]
use address_lookup::MdnsSdAddressLookup;

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
#[derive(Debug, Clone)]
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
/// TXT property carrying a direct `ip:port` address, if any.
///
/// Advertisers whose SRV port is not the real QUIC port — notably the iOS
/// native `NWListener`, whose UDP port is unrelated to the QUIC socket —
/// publish the dialable `ip:<bound QUIC port>` here instead. Browsers surface
/// it as a direct address so the peer can be dialed over the LAN. See #346.
pub const TXT_ADDRESS: &str = "address";

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

    let mut addrs: Vec<String> = rec.addrs.iter().map(SocketAddr::to_string).collect();
    // A direct `ip:port` in the `address` TXT is dialable even when the record's
    // SRV port is not the real QUIC port (iOS native advertiser, #346). Surface
    // it in addition to any SRV/A-derived addresses.
    if let Some((_, address)) = rec.txt.iter().find(|(k, _)| k == TXT_ADDRESS) {
        if !address.is_empty() && !addrs.contains(address) {
            addrs.push(address.clone());
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

// ── iroh-http peer browse (specialization of dns_sd::browse) ─────────────────

/// An active iroh-http peer-browse session that yields [`PeerDiscoveryEvent`]s.
///
/// Wraps a generic [`dns_sd::browse`] session and, as records arrive, feeds the
/// endpoint's in-process address lookup so `fetch(nodeId)` resolves LAN peers.
/// Drop to stop receiving events; the underlying DNS-SD browse and its daemon
/// are shut down with the inner session.
#[cfg(feature = "mdns")]
pub struct BrowseSession {
    inner: ServiceBrowseSession,
    lookup: MdnsSdAddressLookup,
}

#[cfg(feature = "mdns")]
impl BrowseSession {
    /// Returns the next peer event, or `None` when the session is closed.
    ///
    /// Generic (non-iroh) records that carry no usable node id are skipped.
    pub async fn next_event(&mut self) -> Option<PeerDiscoveryEvent> {
        loop {
            let record = self.inner.next_record().await?;
            let Some(ev) = peer_event_from_record(&record) else {
                continue;
            };
            // A bad node id from the wire must not abort the stream.
            if ev.is_active {
                let _ = self.lookup.upsert(&ev.node_id, &ev.addrs);
            } else {
                self.lookup.remove(&ev.node_id);
            }
            return Some(ev);
        }
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
    let lookup = MdnsSdAddressLookup::new();
    ep.address_lookup()
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?
        .add(lookup.clone());

    let inner = dns_sd::browse(BrowseConfig::udp(service_name))?;
    Ok(BrowseSession { inner, lookup })
}

// ── iroh-http advertise (specialization of dns_sd::advertise) ────────────────

/// Select a dialable `ip:<port>` address to advertise from the endpoint's
/// direct addresses.
///
/// Picks the first routable IP (skipping loopback, unspecified and link-local
/// addresses) and pairs it with `bound_port` — the endpoint's real bound QUIC
/// port — since the ports reported by `ip_addrs()` can be placeholders
/// (`:0`/`:1`) rather than the socket the endpoint actually listens on (#346).
/// Returns `None` when no routable address is available (e.g. loopback- or
/// link-local-only), in which case no `address` TXT is published and peers fall
/// back to relay/SRV.
///
/// Interface ranking (preferring the LAN subnet of the browsing peer over a
/// VPN/egress NIC) is tracked separately in #348.
fn select_advertise_address(ip_addrs: &[SocketAddr], bound_port: u16) -> Option<String> {
    ip_addrs
        .iter()
        .map(SocketAddr::ip)
        .find(is_routable_ip)
        .map(|ip| SocketAddr::new(ip, bound_port).to_string())
}

/// Whether `ip` is routable off-link: not loopback, unspecified, or link-local.
///
/// A link-local address (IPv4 `169.254.0.0/16`, IPv6 `fe80::/10`) is only valid
/// on its own segment, so advertising it as a dialable address makes a browsing
/// peer's direct dial fail (#350). `Ipv6Addr::is_unicast_link_local` is still
/// unstable, so the `fe80::/10` prefix is matched by hand.
fn is_routable_ip(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

/// Start advertising this node on the local network via DNS-SD.
///
/// Publishes `_<service_name>._udp.local` with the instance name and `pk` TXT
/// set to this node's base32 endpoint id, the SRV port taken from the
/// endpoint's bound socket, and A/AAAA records for the host's addresses (kept
/// up to date as interfaces change). The node remains advertised until the
/// returned [`AdvertiseSession`] is dropped.
///
/// For non-iroh services (custom instance names, ports, or TXT), use
/// [`dns_sd::advertise`] directly.
#[cfg(feature = "mdns")]
pub fn advertise_peer(
    ep: &iroh::Endpoint,
    service_name: &str,
) -> Result<AdvertiseSession, DiscoveryError> {
    let instance = node_id_label(&ep.id());

    let port = ep
        .bound_sockets()
        .first()
        .map(|s| s.port())
        .ok_or_else(|| DiscoveryError::Setup("endpoint has no bound socket".into()))?;

    let node_addr = ep.addr();
    let socket_addrs: Vec<SocketAddr> = node_addr.ip_addrs().copied().collect();
    let addrs: Vec<IpAddr> = socket_addrs.iter().map(SocketAddr::ip).collect();

    let mut txt: Vec<(String, String)> = vec![(TXT_PK.to_string(), instance.clone())];
    if let Some(relay) = node_addr.relay_urls().next() {
        txt.push((TXT_RELAY.to_string(), relay.to_string()));
    }
    // #350 review W1: publish a dialable `ip:<bound QUIC port>` so mobile
    // browsers — which read only the `address`/`relay` TXT and never resolve
    // this record's SRV/A — can direct-dial this node over the LAN instead of
    // falling back to relay-only.
    if let Some(address) = select_advertise_address(&socket_addrs, port) {
        txt.push((TXT_ADDRESS.to_string(), address));
    }

    dns_sd::advertise(ServiceConfig {
        service_name: service_name.to_string(),
        instance_name: instance,
        port,
        addrs,
        txt,
        protocol: Protocol::Udp,
    })
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
        // dialable `ip:<bound QUIC port>` in the `address` TXT so mobile
        // browsers (iOS NWBrowser can't resolve SRV/A at all; Android
        // browse_peers reads only the `address`/`relay` attrs) can direct-dial
        // it over the LAN instead of falling back to relay-only.
        let ip_addrs = vec![
            "127.0.0.1:1".parse().unwrap(),
            "192.168.1.42:1".parse().unwrap(),
        ];
        let addr = select_advertise_address(&ip_addrs, 59234)
            .expect("a routable address must be selected");
        assert_eq!(addr, "192.168.1.42:59234");
    }

    #[test]
    fn select_advertise_address_skips_loopback_and_unspecified() {
        let ip_addrs = vec![
            "127.0.0.1:5000".parse().unwrap(),
            "0.0.0.0:5000".parse().unwrap(),
            "[::1]:5000".parse().unwrap(),
        ];
        assert_eq!(select_advertise_address(&ip_addrs, 59234), None);
    }

    #[test]
    fn select_advertise_address_brackets_ipv6() {
        let ip_addrs = vec!["[2001:db8::1]:1".parse().unwrap()];
        let addr = select_advertise_address(&ip_addrs, 443).expect("address");
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
        let addr = select_advertise_address(&ip_addrs, 443).expect("address");
        assert_eq!(addr, "192.168.1.9:443");
    }

    #[test]
    fn select_advertise_address_none_when_only_link_local() {
        let ip_addrs = vec![
            "169.254.10.1:1".parse().unwrap(),
            "[fe80::1]:1".parse().unwrap(),
        ];
        assert_eq!(select_advertise_address(&ip_addrs, 443), None);
    }
}
