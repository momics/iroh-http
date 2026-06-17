//! `iroh-http-discovery` — local mDNS peer discovery for iroh-http.
//!
//! Implements Iroh's address-lookup trait using mDNS so nodes on the same
//! local network can find each other without a relay server.
//!
//! Use [`start_browse`] and [`start_advertise`] to start discovery sessions.
//!
//! # Platform notes
//!
//! - Desktop (macOS, Linux, Windows): enabled with the `mdns` feature (default).
//! - iOS / Android (Tauri mobile): use the platform's native service discovery.
#![deny(unsafe_code)]

#[cfg(feature = "mdns")]
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
#[cfg(feature = "mdns")]
use std::sync::Arc;

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

// ── Browse session ───────────────────────────────────────────────────────────

/// An active browse session that yields discovery events.
///
/// Drop to stop receiving events.  Note: the underlying mDNS lookup
/// remains registered on the endpoint because the iroh API does not
/// support removal.  Avoid calling `start_browse` repeatedly without
/// restarting the endpoint if accumulation is a concern.
#[cfg(feature = "mdns")]
pub struct BrowseSession {
    rx: tokio::sync::mpsc::Receiver<DiscoveryEvent>,
    _mdns: Arc<MdnsAddressLookup>,
}

#[cfg(feature = "mdns")]
impl BrowseSession {
    /// Returns the next event, or `None` when the session is closed.
    pub async fn next_event(&mut self) -> Option<PeerDiscoveryEvent> {
        use iroh::TransportAddr;

        let ev = self.rx.recv().await?;
        Some(match ev {
            DiscoveryEvent::Discovered { endpoint_info, .. } => {
                let node_id = endpoint_info.endpoint_id.to_string();
                let mut addrs = Vec::new();
                for a in endpoint_info.data.addrs() {
                    match a {
                        TransportAddr::Ip(sock) => addrs.push(sock.to_string()),
                        TransportAddr::Relay(url) => addrs.push(url.to_string()),
                        other => addrs.push(format!("{:?}", other)),
                    }
                }
                PeerDiscoveryEvent {
                    is_active: true,
                    node_id,
                    addrs,
                }
            }
            DiscoveryEvent::Expired { endpoint_id } => PeerDiscoveryEvent {
                is_active: false,
                node_id: endpoint_id.to_string(),
                addrs: Vec::new(),
            },
            _ => return None,
        })
    }
}

/// Start a browse session: discover peers on the local network via mDNS.
///
/// Creates an `MdnsAddressLookup` with `advertise(false)`, registers it on the
/// endpoint, and subscribes to discovery events.
#[cfg(feature = "mdns")]
pub async fn start_browse(
    ep: &iroh::Endpoint,
    service_name: &str,
) -> Result<BrowseSession, DiscoveryError> {
    let mdns = Arc::new(
        MdnsAddressLookup::builder()
            .advertise(false)
            .service_name(service_name)
            .build(ep.id())
            .map_err(|e| DiscoveryError::Setup(e.to_string()))?,
    );
    ep.address_lookup()
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?
        .add(Arc::clone(&mdns));

    // subscribe() returns impl Stream — we manually drive it into an mpsc channel
    // so BrowseSession has a concrete Receiver type.
    use futures::StreamExt;
    let mut stream = mdns.subscribe().await;
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(ev) = stream.next().await {
            if tx.send(ev).await.is_err() {
                break;
            }
        }
    });

    Ok(BrowseSession { rx, _mdns: mdns })
}

// ── Advertise session ────────────────────────────────────────────────────────

/// An active advertise session.
///
/// Drop to stop advertising.  Note: the underlying mDNS lookup remains
/// registered on the endpoint (same caveat as [`BrowseSession`]).
#[cfg(feature = "mdns")]
pub struct AdvertiseSession {
    _mdns: Arc<MdnsAddressLookup>,
}

/// Start advertising this node on the local network via mDNS.
///
/// The node remains advertised until the returned `AdvertiseSession` is dropped.
#[cfg(feature = "mdns")]
pub fn start_advertise(
    ep: &iroh::Endpoint,
    service_name: &str,
) -> Result<AdvertiseSession, DiscoveryError> {
    let mdns = Arc::new(
        MdnsAddressLookup::builder()
            .advertise(true)
            .service_name(service_name)
            .build(ep.id())
            .map_err(|e| DiscoveryError::Setup(e.to_string()))?,
    );
    ep.address_lookup()
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?
        .add(Arc::clone(&mdns));
    Ok(AdvertiseSession { _mdns: mdns })
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
}
