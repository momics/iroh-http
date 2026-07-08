//! In-process iroh [`AddressLookup`] backed by DNS-SD–discovered peers.
//!
//! Mirrors the mobile `MobileAddressLookup`: it keeps a `node-id → EndpointData`
//! map driven by the browse event stream so iroh's dialer transparently resolves
//! mDNS-discovered peers by node-id — a `fetch(nodeId)` with no caller-supplied
//! address just works. See issue #329.

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
const PROVENANCE: &str = "mdns-sd";

/// An in-process [`AddressLookup`] populated from DNS-SD–discovered peers.
///
/// Cheap to clone: the map is shared behind an `Arc<Mutex<_>>`. Registered on
/// the endpoint by [`crate::start_browse`]; the browse pump updates the map.
#[derive(Debug, Default, Clone)]
pub(crate) struct MdnsSdAddressLookup {
    map: Arc<Mutex<HashMap<EndpointId, EndpointData>>>,
}

impl MdnsSdAddressLookup {
    /// Create an empty lookup.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the addresses known for `node_id`.
    ///
    /// `node_id` is a base32 iroh endpoint id; `addrs` are `ip:port` strings
    /// and/or relay URLs. Returns `Err` if `node_id` is not a valid endpoint
    /// id. Individual unparseable address strings are skipped.
    pub(crate) fn upsert(&self, node_id: &str, addrs: &[String]) -> Result<(), String> {
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
    pub(crate) fn remove(&self, node_id: &str) {
        if let Ok(endpoint_id) = node_id.parse::<EndpointId>() {
            self.map
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&endpoint_id);
        }
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

impl AddressLookup for MdnsSdAddressLookup {
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

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use n0_future::StreamExt;

    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    async fn resolve_one(lookup: &MdnsSdAddressLookup, id: EndpointId) -> Option<Item> {
        let mut stream = lookup.resolve(id)?;
        let first = stream.next().await;
        assert!(stream.next().await.is_none(), "expected exactly one item");
        first.map(|r| r.expect("resolve item should be Ok"))
    }

    #[tokio::test]
    async fn upsert_then_resolve_yields_matching_item() {
        let lookup = MdnsSdAddressLookup::new();
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
    async fn remove_evicts_node() {
        let lookup = MdnsSdAddressLookup::new();
        let id = endpoint_id(2);
        lookup
            .upsert(&id.to_string(), &["10.0.0.1:1234".to_string()])
            .unwrap();
        assert!(resolve_one(&lookup, id).await.is_some());
        lookup.remove(&id.to_string());
        assert!(
            lookup.resolve(id).is_none(),
            "removed node must not resolve"
        );
    }

    #[test]
    fn invalid_node_id_is_rejected() {
        let lookup = MdnsSdAddressLookup::new();
        assert!(lookup.upsert("not-a-node-id", &[]).is_err());
    }
}
