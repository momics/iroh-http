//! `iroh-http-discovery` — local mDNS peer discovery for iroh-http.
//!
//! Implements standard DNS-SD (RFC 6763) mDNS so nodes on the same local
//! network can find each other without a relay server.
//!
//! Use [`start_browse`] and [`start_advertise`] to start discovery sessions.
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
mod address_lookup;

#[cfg(feature = "mdns")]
use std::net::{IpAddr, SocketAddr};

#[cfg(feature = "mdns")]
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};

#[cfg(feature = "mdns")]
use address_lookup::MdnsSdAddressLookup;

// ── DiscoveryError ────────────────────────────────────────────────────────────

/// Structured error returned by [`start_browse`] and [`start_advertise`].
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

// ── Service-name / TXT helpers ────────────────────────────────────────────────

/// TXT property carrying the peer's base32 endpoint id.
#[cfg(feature = "mdns")]
const TXT_PK: &str = "pk";
/// TXT property carrying the peer's home relay URL, if any.
#[cfg(feature = "mdns")]
const TXT_RELAY: &str = "relay";

/// Encode an endpoint id as lowercase RFC 4648 base32 (no padding) — a 52-char
/// label that fits a DNS-SD instance name and matches the node-id form used by
/// the rest of iroh-http. iroh's own `Display` is 64-char hex, which exceeds the
/// 63-byte DNS label limit and is rejected by `mdns-sd`.
#[cfg(feature = "mdns")]
fn node_id_label(id: &iroh::EndpointId) -> String {
    base32::encode(base32::Alphabet::Rfc4648Lower { padding: false }, id.as_bytes())
}

/// Validate a service name and build the fully-qualified DNS-SD service type
/// `_<service_name>._udp.local.`.
///
/// The name must be a single DNS label: non-empty and ASCII alphanumeric or
/// `-` (no leading `_`, no dot).
#[cfg(feature = "mdns")]
fn service_type(service_name: &str) -> Result<String, DiscoveryError> {
    if service_name.is_empty() {
        return Err(DiscoveryError::InvalidServiceName(
            "service name must not be empty".into(),
        ));
    }
    if !service_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(DiscoveryError::InvalidServiceName(format!(
            "{service_name:?} must contain only ASCII letters, digits, and '-'"
        )));
    }
    Ok(format!("_{service_name}._udp.local."))
}

/// Extract the service instance label (the base32 endpoint id) from a DNS-SD
/// fullname like `<instance>._iroh-http._udp.local.`.
#[cfg(feature = "mdns")]
fn instance_from_fullname(fullname: &str) -> Option<String> {
    let instance = fullname.split("._").next()?;
    if instance.is_empty() {
        None
    } else {
        Some(instance.to_string())
    }
}

/// Convert a resolved DNS-SD service into a [`PeerDiscoveryEvent`].
///
/// The node id comes from the `pk` TXT property, falling back to the instance
/// label. Direct addresses are reconstructed from each resolved IP paired with
/// the SRV port; a `relay` TXT property is appended if present.
#[cfg(feature = "mdns")]
fn resolved_to_event(rs: &ResolvedService) -> Option<PeerDiscoveryEvent> {
    let node_id = rs
        .txt_properties
        .get_property_val_str(TXT_PK)
        .map(str::to_string)
        .or_else(|| instance_from_fullname(&rs.fullname))?;

    let mut addrs: Vec<String> = rs
        .addresses
        .iter()
        .map(|scoped| SocketAddr::new(scoped.to_ip_addr(), rs.port).to_string())
        .collect();

    if let Some(relay) = rs.txt_properties.get_property_val_str(TXT_RELAY) {
        addrs.push(relay.to_string());
    }

    Some(PeerDiscoveryEvent {
        is_active: true,
        node_id,
        addrs,
    })
}

/// Create a fresh mDNS daemon for a single browse or advertise session.
///
/// Each session owns its own daemon (rather than sharing one process-wide) so
/// that its lifetime is tied to the session: dropping the session shuts the
/// daemon down. It also means an advertise and a browse in the *same* process
/// run on separate daemons and can discover each other over loopback multicast
/// — `mdns-sd` does not deliver a daemon's own registrations back to its own
/// browsers.
#[cfg(feature = "mdns")]
fn new_daemon() -> Result<ServiceDaemon, DiscoveryError> {
    ServiceDaemon::new().map_err(|e| DiscoveryError::Setup(e.to_string()))
}

// ── Browse session ───────────────────────────────────────────────────────────

/// An active browse session that yields discovery events.
///
/// Drop to stop receiving events; this stops the underlying DNS-SD browse,
/// shuts the session's mDNS daemon down, and stops the in-process
/// address-lookup pump.
#[cfg(feature = "mdns")]
pub struct BrowseSession {
    rx: tokio::sync::mpsc::Receiver<PeerDiscoveryEvent>,
    daemon: ServiceDaemon,
    service_type: String,
    pump: tokio::task::JoinHandle<()>,
}

#[cfg(feature = "mdns")]
impl BrowseSession {
    /// Returns the next event, or `None` when the session is closed.
    pub async fn next_event(&mut self) -> Option<PeerDiscoveryEvent> {
        self.rx.recv().await
    }
}

#[cfg(feature = "mdns")]
impl Drop for BrowseSession {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(&self.service_type);
        let _ = self.daemon.shutdown();
        self.pump.abort();
    }
}

/// Start a browse session: discover peers on the local network via DNS-SD.
///
/// Browses `_<service_name>._udp.local`, and registers an in-process
/// [`AddressLookup`](iroh::address_lookup::AddressLookup) on the endpoint fed by
/// the discovered peers, so `fetch(nodeId)` auto-resolves LAN peers by node id.
#[cfg(feature = "mdns")]
pub async fn start_browse(
    ep: &iroh::Endpoint,
    service_name: &str,
) -> Result<BrowseSession, DiscoveryError> {
    let service_type = service_type(service_name)?;
    let daemon = new_daemon()?;
    let receiver = daemon
        .browse(&service_type)
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?;

    let lookup = MdnsSdAddressLookup::new();
    ep.address_lookup()
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?
        .add(lookup.clone());

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let pump = tokio::spawn(async move {
        while let Ok(event) = receiver.recv_async().await {
            match event {
                ServiceEvent::ServiceResolved(resolved) => {
                    if let Some(ev) = resolved_to_event(&resolved) {
                        // A bad node id from the wire must not abort the pump.
                        let _ = lookup.upsert(&ev.node_id, &ev.addrs);
                        if tx.send(ev).await.is_err() {
                            break;
                        }
                    }
                }
                ServiceEvent::ServiceRemoved(_ty, fullname) => {
                    if let Some(node_id) = instance_from_fullname(&fullname) {
                        lookup.remove(&node_id);
                        let ev = PeerDiscoveryEvent {
                            is_active: false,
                            node_id,
                            addrs: Vec::new(),
                        };
                        if tx.send(ev).await.is_err() {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    });

    Ok(BrowseSession {
        rx,
        daemon,
        service_type,
        pump,
    })
}

// ── Advertise session ────────────────────────────────────────────────────────

/// An active advertise session.
///
/// Drop to stop advertising; this unregisters the DNS-SD service.
#[cfg(feature = "mdns")]
pub struct AdvertiseSession {
    daemon: ServiceDaemon,
    fullname: String,
}

#[cfg(feature = "mdns")]
impl Drop for AdvertiseSession {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Start advertising this node on the local network via DNS-SD.
///
/// Publishes `_<service_name>._udp.local` with the instance name and `pk` TXT
/// set to this node's base32 endpoint id, the SRV port taken from the
/// endpoint's bound socket, and A/AAAA records for the host's addresses (kept
/// up to date as interfaces change). The node remains advertised until the
/// returned [`AdvertiseSession`] is dropped.
#[cfg(feature = "mdns")]
pub fn start_advertise(
    ep: &iroh::Endpoint,
    service_name: &str,
) -> Result<AdvertiseSession, DiscoveryError> {
    let service_type = service_type(service_name)?;
    let daemon = new_daemon()?;

    let instance = node_id_label(&ep.id());
    let host_name = format!("{instance}.local.");

    let port = ep
        .bound_sockets()
        .first()
        .map(|s| s.port())
        .ok_or_else(|| DiscoveryError::Setup("endpoint has no bound socket".into()))?;

    let node_addr = ep.addr();
    let ips: Vec<IpAddr> = node_addr.ip_addrs().map(|s| s.ip()).collect();

    let mut props: Vec<(String, String)> = vec![(TXT_PK.to_string(), instance.clone())];
    if let Some(relay) = node_addr.relay_urls().next() {
        props.push((TXT_RELAY.to_string(), relay.to_string()));
    }

    let info = ServiceInfo::new(
        &service_type,
        &instance,
        &host_name,
        &ips[..],
        port,
        &props[..],
    )
    .map_err(|e| DiscoveryError::Setup(e.to_string()))?
    .enable_addr_auto();

    let fullname = info.get_fullname().to_string();
    daemon
        .register(info)
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?;

    Ok(AdvertiseSession { daemon, fullname })
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
    fn service_type_builds_udp_local_domain() {
        assert_eq!(service_type("iroh-http").unwrap(), "_iroh-http._udp.local.");
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn service_type_rejects_empty_and_illegal_names() {
        assert!(matches!(
            service_type(""),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
        assert!(matches!(
            service_type("has space"),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
        assert!(matches!(
            service_type("has.dot"),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn instance_from_fullname_extracts_base32_label() {
        let full = "abcdef234567._iroh-http._udp.local.";
        assert_eq!(instance_from_fullname(full).as_deref(), Some("abcdef234567"));
        assert_eq!(instance_from_fullname("._iroh-http._udp.local."), None);
    }
}

