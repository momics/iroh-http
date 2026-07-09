//! In-process iroh `AddressLookup` backed by natively-discovered mDNS peers.
//!
//! # Why this exists
//!
//! On desktop, `iroh-http-discovery` registers an `MdnsAddressLookup` on the
//! endpoint via `ep.address_lookup().add(..)`, so iroh's dialer transparently
//! resolves mDNS-discovered peers by node-id — a `fetch(nodeId)` with no
//! caller-supplied address just works.
//!
//! On mobile (iOS/Android) raw multicast is restricted, so discovery is bridged
//! to the platform's native APIs (NWBrowser / NsdManager) and surfaced as
//! [`crate::mobile_mdns::MobileDiscoveryEvent`]s. Those events reach the JS
//! browse API, but nothing fed them back into an in-process `AddressLookup`, so
//! a discovered peer was only a UI list entry — the dialer had no address.
//!
//! [`MobileAddressLookup`] closes that gap: it keeps a `node-id → EndpointData`
//! map, driven by the mobile browse event stream (`discovered` inserts/updates,
//! `expired` evicts), and implements [`iroh::address_lookup::AddressLookup`] so
//! iroh's dialer resolves discovered peers by node-id — parity with desktop.
//!
//! Scope is iroh-only, additive: it does not change the browse/advertise API or
//! the `MobileDiscoveryEvent`→JS surface. See ADR-016.
//!
//! This module is platform-independent (no `#[cfg(mobile)]` gate) so the host
//! build compiles, clippy-checks, and unit-tests the map/parse/resolve core.
//! Only the registration + event-feed wiring in `commands.rs` / `lib.rs` is
//! mobile-gated.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use iroh::{
    address_lookup::{
        AddressLookup, EndpointData, EndpointInfo, Error as AddressLookupError, Item,
    },
    EndpointId, RelayUrl, TransportAddr,
};
use n0_future::{boxed::BoxStream, stream};

/// `provenance` string tagged onto resolved [`Item`]s, identifying this lookup.
const PROVENANCE: &str = "mobile-mdns";

/// An in-process [`AddressLookup`] populated from natively-discovered mDNS peers.
///
/// Cheap to clone: the map is shared behind an `Arc<Mutex<_>>`. One instance is
/// managed per app and registered on each endpoint; the mobile browse event
/// pump updates the shared map.
#[derive(Debug, Default, Clone)]
pub struct MobileAddressLookup {
    map: Arc<Mutex<HashMap<EndpointId, EndpointData>>>,
}

impl MobileAddressLookup {
    /// Create an empty lookup.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the addresses known for `node_id`.
    ///
    /// `node_id` is a base32 iroh endpoint id; `addrs` are `ip:port` strings
    /// and/or relay URLs (exactly what `MobileDiscoveryEvent` carries).
    ///
    /// Returns `Err` if `node_id` is not a valid endpoint id. Individual
    /// unparseable address strings are skipped rather than failing the whole
    /// update.
    pub fn upsert(&self, node_id: &str, addrs: &[String]) -> Result<(), String> {
        let endpoint_id: EndpointId = node_id
            .parse()
            .map_err(|e| format!("invalid node id {node_id:?}: {e}"))?;
        let data = build_endpoint_data(addrs);
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(endpoint_id, data);
        Ok(())
    }

    /// Remove any addresses known for `node_id`. A no-op if the id is unknown
    /// or unparseable.
    pub fn remove(&self, node_id: &str) {
        if let Ok(endpoint_id) = node_id.parse::<EndpointId>() {
            self.map
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&endpoint_id);
        }
    }

    /// Apply a mobile discovery event: `"discovered"` upserts, `"expired"`
    /// evicts. Unknown kinds are ignored.
    pub fn apply_event(&self, kind: &str, node_id: &str, addrs: &[String]) {
        match kind {
            "discovered" => {
                // A bad node-id from the native layer must not abort the pump;
                // just skip it.
                let _ = self.upsert(node_id, addrs);
            }
            "expired" => self.remove(node_id),
            _ => {}
        }
    }

    /// Number of node-ids currently held. Test/introspection helper.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Build [`EndpointData`] from discovered address strings.
///
/// `ip:port` strings become direct [`TransportAddr::Ip`] addresses; anything
/// else is tried as a [`RelayUrl`]. Unparseable entries are dropped.
fn build_endpoint_data(addrs: &[String]) -> EndpointData {
    let mut ip_addrs: Vec<TransportAddr> = Vec::new();
    let mut relay_urls: Vec<RelayUrl> = Vec::new();

    for addr in addrs {
        if let Ok(sock) = addr.parse::<SocketAddr>() {
            ip_addrs.push(TransportAddr::Ip(sock));
        } else if let Ok(url) = addr.parse::<RelayUrl>() {
            relay_urls.push(url);
        }
        // else: unrecognised address form — skip it.
    }

    let mut data = EndpointData::from_iter(ip_addrs);
    for url in relay_urls {
        data.add_relay_url(url);
    }
    data
}

impl AddressLookup for MobileAddressLookup {
    fn resolve(
        &self,
        endpoint_id: EndpointId,
    ) -> Option<BoxStream<Result<Item, AddressLookupError>>> {
        let data = self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&endpoint_id)
            .cloned()?;

        let info = EndpointInfo::from_parts(endpoint_id, data);
        let item = Item::new(info, PROVENANCE, None);
        Some(Box::pin(stream::once(Ok(item))))
    }
}

// ── Unit tests (host-compilable core: map + parse + resolve) ──────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use n0_future::StreamExt;

    /// Deterministic endpoint id from a fixed 32-byte seed.
    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// Drain a `resolve` stream into a single `Item`, asserting exactly one.
    async fn resolve_one(lookup: &MobileAddressLookup, id: EndpointId) -> Option<Item> {
        let mut stream = lookup.resolve(id)?;
        let first = stream.next().await;
        // A well-behaved single-shot stream ends after one item.
        assert!(stream.next().await.is_none(), "expected exactly one item");
        first.map(|r| r.expect("resolve item should be Ok"))
    }

    #[tokio::test]
    async fn upsert_then_resolve_yields_matching_item() {
        let lookup = MobileAddressLookup::new();
        let id = endpoint_id(1);
        lookup
            .upsert(&id.to_string(), &["192.168.1.5:4433".to_string()])
            .expect("valid node id");

        let item = resolve_one(&lookup, id).await.expect("resolves");
        assert_eq!(item.endpoint_id(), id);
        let addrs: Vec<_> = item.endpoint_info().addrs().collect();
        assert!(
            addrs
                .iter()
                .any(|a| matches!(a, TransportAddr::Ip(s) if s.to_string() == "192.168.1.5:4433")),
            "resolved item should carry the discovered direct address, got {addrs:?}"
        );
        assert_eq!(item.provenance(), PROVENANCE);
    }

    #[tokio::test]
    async fn expired_event_evicts_node() {
        let lookup = MobileAddressLookup::new();
        let id = endpoint_id(2);
        lookup.apply_event(
            "discovered",
            &id.to_string(),
            &["10.0.0.1:1234".to_string()],
        );
        assert!(resolve_one(&lookup, id).await.is_some());

        lookup.apply_event("expired", &id.to_string(), &[]);
        assert!(
            lookup.resolve(id).is_none(),
            "expired peer must no longer resolve"
        );
        assert_eq!(lookup.len(), 0);
    }

    #[tokio::test]
    async fn discovered_event_updates_addrs() {
        let lookup = MobileAddressLookup::new();
        let id = endpoint_id(3);
        lookup.apply_event(
            "discovered",
            &id.to_string(),
            &["10.0.0.1:1111".to_string()],
        );
        lookup.apply_event(
            "discovered",
            &id.to_string(),
            &["10.0.0.2:2222".to_string()],
        );

        let item = resolve_one(&lookup, id).await.expect("resolves");
        let addrs: Vec<String> = item
            .endpoint_info()
            .addrs()
            .filter_map(|a| match a {
                TransportAddr::Ip(s) => Some(s.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            addrs,
            vec!["10.0.0.2:2222".to_string()],
            "upsert replaces addrs"
        );
        assert_eq!(lookup.len(), 1);
    }

    #[test]
    fn mixed_ip_and_relay_addrs_parse() {
        let data = build_endpoint_data(&[
            "127.0.0.1:5000".to_string(),
            "https://relay.example.com./".to_string(),
            "not-an-address".to_string(),
        ]);
        let has_ip = data
            .addrs()
            .any(|a| matches!(a, TransportAddr::Ip(s) if s.to_string() == "127.0.0.1:5000"));
        let has_relay = data.addrs().any(|a| matches!(a, TransportAddr::Relay(_)));
        assert!(has_ip, "ip:port should parse as a direct address");
        assert!(has_relay, "relay URL should parse as a relay address");
    }

    #[test]
    fn invalid_node_id_returns_err_without_panic() {
        let lookup = MobileAddressLookup::new();
        let err = lookup.upsert("not-a-valid-node-id", &["127.0.0.1:1:1".to_string()]);
        assert!(
            err.is_err(),
            "invalid node id should be a recoverable error"
        );
        assert_eq!(lookup.len(), 0);
    }

    #[test]
    fn apply_event_tolerates_bad_node_id() {
        let lookup = MobileAddressLookup::new();
        // Must not panic even though the id is invalid.
        lookup.apply_event("discovered", "bogus", &["10.0.0.1:9000".to_string()]);
        assert_eq!(lookup.len(), 0);
    }

    #[test]
    fn resolve_unknown_id_returns_none() {
        let lookup = MobileAddressLookup::new();
        assert!(lookup.resolve(endpoint_id(9)).is_none());
    }
}
