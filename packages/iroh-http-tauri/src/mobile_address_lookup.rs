//! Endpoint-local address lookup registry for native mobile DNS-SD discovery.
//!
//! Android and iOS surface DNS-SD events through their native adapters. Each
//! endpoint receives its own [`SourceScopedAddressLookup`], and each peer browse
//! receives an [`AddressLookupSource`] from that lookup. This preserves both
//! endpoint and browse-session identity: closing one source retracts only its
//! contributions, while overlapping sources continue to resolve their union.
//!
//! This module remains host-compilable so the ownership rules are unit-tested
//! without requiring an Android or iOS target.

use std::{collections::HashMap, sync::Mutex};

use iroh_http_core::{AddressLookupSource, SourceScopedAddressLookup};

const PROVENANCE: &str = "mobile-mdns";

/// Registry of the lookup installed on each live mobile endpoint.
#[derive(Debug, Default)]
pub struct MobileAddressLookup {
    endpoints: Mutex<HashMap<u64, SourceScopedAddressLookup>>,
}

impl MobileAddressLookup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct the lookup to register on a newly bound endpoint.
    pub fn new_endpoint_lookup(&self) -> SourceScopedAddressLookup {
        SourceScopedAddressLookup::new(PROVENANCE)
    }

    /// Associate an already-registered lookup with its public endpoint handle.
    pub fn insert_endpoint(&self, handle: u64, lookup: SourceScopedAddressLookup) {
        self.endpoints
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(handle, lookup);
    }

    /// Remove the lookup for a closed or replaced endpoint.
    pub fn remove_endpoint(&self, handle: u64) {
        self.endpoints
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&handle);
    }

    /// Drop every endpoint association during last-WebView teardown.
    pub fn clear(&self) {
        self.endpoints
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }

    /// Create the independent contribution namespace for one peer browse.
    pub fn new_source(&self, endpoint_handle: u64) -> Option<AddressLookupSource> {
        self.endpoints
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&endpoint_handle)
            .map(SourceScopedAddressLookup::new_source)
    }

    #[cfg(test)]
    fn lookup(&self, handle: u64) -> Option<SourceScopedAddressLookup> {
        self.endpoints
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&handle)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use iroh::{address_lookup::AddressLookup, EndpointId, SecretKey};

    use super::*;

    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn endpoint_lookups_are_isolated() {
        let registry = MobileAddressLookup::new();
        let lookup_a = registry.new_endpoint_lookup();
        let lookup_b = registry.new_endpoint_lookup();
        registry.insert_endpoint(10, lookup_a.clone());
        registry.insert_endpoint(20, lookup_b.clone());

        let id = endpoint_id(1);
        let source = registry.new_source(10).expect("first endpoint");
        source
            .upsert(
                "peer.local.",
                &id.to_string(),
                &["192.168.1.2:4433".to_string()],
            )
            .expect("valid contribution");

        assert!(lookup_a.resolve(id).is_some());
        assert!(lookup_b.resolve(id).is_none());
    }

    #[test]
    fn removing_endpoint_prevents_new_browse_sources_only_for_that_endpoint() {
        let registry = MobileAddressLookup::new();
        registry.insert_endpoint(10, registry.new_endpoint_lookup());
        registry.insert_endpoint(20, registry.new_endpoint_lookup());

        registry.remove_endpoint(10);

        assert!(registry.new_source(10).is_none());
        assert!(registry.new_source(20).is_some());
        assert!(registry.lookup(10).is_none());
        assert!(registry.lookup(20).is_some());
    }

    #[test]
    fn retiring_one_browse_keeps_an_overlapping_browse_contribution() {
        let registry = MobileAddressLookup::new();
        let lookup = registry.new_endpoint_lookup();
        registry.insert_endpoint(10, lookup.clone());
        let first = registry.new_source(10).unwrap();
        let second = registry.new_source(10).unwrap();
        let id = endpoint_id(2);
        let node_id = id.to_string();

        first
            .upsert("first-instance", &node_id, &["10.0.0.1:4433".to_string()])
            .unwrap();
        second
            .upsert("second-instance", &node_id, &["10.0.0.2:4433".to_string()])
            .unwrap();

        first.retire();

        assert!(lookup.resolve(id).is_some());
        second.retire();
        assert!(lookup.resolve(id).is_none());
    }
}
