//! Source-scoped address discovery for adapters with browse-session lifetimes.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use futures::stream;
use iroh::{
    address_lookup::{AddressLookup, EndpointData, EndpointInfo, Error, Item},
    EndpointId, RelayUrl, TransportAddr,
};

/// An in-process iroh address lookup whose records retain browse-source and
/// DNS-SD service-instance identity.
///
/// Clone this value to register it with an endpoint. Create an
/// [`AddressLookupSource`] for every independent browse session.
///
/// Retiring a source removes it from all future [`AddressLookup::resolve`]
/// snapshots. Iroh 1.0.2 can separately retain paths it already consumed in
/// its private remote-path cache; that cache has no public invalidation API and
/// is cleared only with the endpoint itself.
#[derive(Debug, Clone)]
pub struct SourceScopedAddressLookup {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    provenance: &'static str,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    next_source_id: u64,
    contributions: BTreeMap<(u64, String), Contribution>,
    effective: HashMap<EndpointId, EndpointData>,
}

#[derive(Debug)]
struct Contribution {
    endpoint_id: EndpointId,
    node_id: String,
    data: EndpointData,
}

/// The contribution namespace owned by one browse session.
#[derive(Debug, Clone)]
pub struct AddressLookupSource {
    lease: Arc<SourceLease>,
}

/// Result of removing one service-instance contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressLookupRemoval {
    /// Canonical lowercase-base32 node id of the removed instance.
    pub node_id: String,
    /// Whether another live instance in this browse source still advertises
    /// this node, independent of whether that contribution currently has a
    /// dialable address.
    pub has_remaining_contributions: bool,
    /// Effective addresses still contributed for that node by other live
    /// instances in this browse source.
    pub remaining_addrs: Vec<String>,
}

/// Effective state changes produced by a successful contribution upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressLookupUpsert {
    /// Canonical lowercase-base32 node id of the inserted contribution.
    pub node_id: String,
    /// This browse source's effective address union for the newly advertised
    /// node after the update.
    pub effective_addrs: Vec<String>,
    /// Transition for the previously advertised node when this service
    /// instance changed endpoint identity. This is `None` for a new instance
    /// or an update that retains the same parsed endpoint id.
    pub replaced: Option<AddressLookupRemoval>,
}

/// Failure to apply an address-lookup contribution.
///
/// A malformed authoritative replacement retracts the prior contribution for
/// the same service instance. In that case [`Self::into_removal`] exposes the
/// removed peer and its remaining effective address union so adapters can
/// publish the corresponding state transition instead of leaving consumers
/// with a stale active peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressLookupUpsertError {
    message: String,
    removal: Option<AddressLookupRemoval>,
}

impl AddressLookupUpsertError {
    /// Returns the state transition caused while rejecting this update, if any.
    pub fn removal(&self) -> Option<&AddressLookupRemoval> {
        self.removal.as_ref()
    }

    /// Consumes the error and returns the state transition caused while
    /// rejecting this update, if any.
    pub fn into_removal(self) -> Option<AddressLookupRemoval> {
        self.removal
    }
}

impl std::fmt::Display for AddressLookupUpsertError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AddressLookupUpsertError {}

#[derive(Debug)]
struct SourceLease {
    lookup: SourceScopedAddressLookup,
    source_id: u64,
    retired: AtomicBool,
}

impl SourceScopedAddressLookup {
    /// Creates an empty lookup. `provenance` is attached to every resolved item.
    pub fn new(provenance: &'static str) -> Self {
        Self {
            shared: Arc::new(Shared {
                provenance,
                state: Mutex::new(State {
                    next_source_id: 1,
                    ..State::default()
                }),
            }),
        }
    }

    /// Creates an independent contribution namespace for one browse session.
    pub fn new_source(&self) -> AddressLookupSource {
        let source_id = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let source_id = state.next_source_id;
            state.next_source_id = state
                .next_source_id
                .checked_add(1)
                .expect("address-lookup source id space exhausted");
            source_id
        };
        AddressLookupSource {
            lease: Arc::new(SourceLease {
                lookup: self.clone(),
                source_id,
                retired: AtomicBool::new(false),
            }),
        }
    }
}

impl SourceLease {
    fn retire(&self) {
        if self.retired.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut state = self
            .lookup
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut affected = HashSet::new();
        state.contributions.retain(|(source_id, _), contribution| {
            if *source_id == self.source_id {
                affected.insert(contribution.endpoint_id);
                false
            } else {
                true
            }
        });
        for endpoint_id in affected {
            recompute_endpoint(&mut state, endpoint_id);
        }
    }
}

impl Drop for SourceLease {
    fn drop(&mut self) {
        self.retire();
    }
}

impl AddressLookupSource {
    /// Synchronously removes every contribution owned by this source.
    ///
    /// Retirement is shared by all clones, is idempotent, and permanently
    /// rejects later updates. It is also performed automatically when the last
    /// source clone is dropped.
    pub fn retire(&self) {
        self.lease.retire();
    }

    /// Inserts or replaces one DNS-SD service-instance contribution.
    ///
    /// The returned snapshot lets adapters publish the new node's full
    /// effective union and, when the instance changed identity, the old node's
    /// transition before the new active event.
    pub fn upsert(
        &self,
        instance_name: &str,
        node_id: &str,
        addrs: &[String],
    ) -> Result<AddressLookupUpsert, AddressLookupUpsertError> {
        let parsed_endpoint_id = node_id
            .parse::<EndpointId>()
            .map_err(|error| format!("invalid node id {node_id:?}: {error}"));
        let key = (self.lease.source_id, instance_name.to_string());

        let mut state = self
            .lease
            .lookup
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.lease.retired.load(Ordering::Acquire) {
            return Err(AddressLookupUpsertError {
                message: "address-lookup source has been retired".to_string(),
                removal: None,
            });
        }
        let endpoint_id = match parsed_endpoint_id {
            Ok(endpoint_id) => endpoint_id,
            Err(message) => {
                let removal = remove_contribution(&mut state, &key);
                return Err(AddressLookupUpsertError { message, removal });
            }
        };
        let contribution = Contribution {
            endpoint_id,
            node_id: crate::base32_encode(endpoint_id.as_bytes()),
            data: endpoint_data(addrs),
        };
        let old = state.contributions.insert(key, contribution);
        if let Some(old_endpoint_id) = old
            .as_ref()
            .map(|old| old.endpoint_id)
            .filter(|old_endpoint_id| *old_endpoint_id != endpoint_id)
        {
            recompute_endpoint(&mut state, old_endpoint_id);
        }
        recompute_endpoint(&mut state, endpoint_id);
        let effective_addrs = source_effective_addrs(&state, self.lease.source_id, endpoint_id);
        let replaced = old.and_then(|old| {
            (old.endpoint_id != endpoint_id).then(|| {
                removal_snapshot(&state, self.lease.source_id, old.endpoint_id, old.node_id)
            })
        });
        Ok(AddressLookupUpsert {
            node_id: crate::base32_encode(endpoint_id.as_bytes()),
            effective_addrs,
            replaced,
        })
    }

    /// Removes this source's contribution for `instance_name`.
    ///
    /// The returned value is the removed record's canonical lowercase-base32
    /// node id. This lets an expiry event recover its peer identity even though
    /// DNS-SD removal callbacks carry only the service instance name.
    pub fn remove(&self, instance_name: &str) -> Option<String> {
        self.remove_with_snapshot(instance_name)
            .map(|removal| removal.node_id)
    }

    /// Removes one instance and returns the node's remaining effective
    /// snapshot atomically with that removal.
    pub fn remove_with_snapshot(&self, instance_name: &str) -> Option<AddressLookupRemoval> {
        let mut state = self
            .lease
            .lookup
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.lease.retired.load(Ordering::Acquire) {
            return None;
        }
        let key = (self.lease.source_id, instance_name.to_string());
        remove_contribution(&mut state, &key)
    }
}

fn remove_contribution(state: &mut State, key: &(u64, String)) -> Option<AddressLookupRemoval> {
    let contribution = state.contributions.remove(key)?;
    recompute_endpoint(state, contribution.endpoint_id);
    Some(removal_snapshot(
        state,
        key.0,
        contribution.endpoint_id,
        contribution.node_id,
    ))
}

fn removal_snapshot(
    state: &State,
    source_id: u64,
    endpoint_id: EndpointId,
    advertised_node_id: String,
) -> AddressLookupRemoval {
    let has_remaining_contributions =
        state
            .contributions
            .iter()
            .any(|((remaining_source_id, _), remaining)| {
                *remaining_source_id == source_id && remaining.endpoint_id == endpoint_id
            });
    AddressLookupRemoval {
        node_id: advertised_node_id,
        has_remaining_contributions,
        remaining_addrs: source_effective_addrs(state, source_id, endpoint_id),
    }
}

fn source_effective_addrs(state: &State, source_id: u64, endpoint_id: EndpointId) -> Vec<String> {
    let data = EndpointData::from_iter(
        state
            .contributions
            .iter()
            .filter(|((remaining_source_id, _), contribution)| {
                *remaining_source_id == source_id && contribution.endpoint_id == endpoint_id
            })
            .flat_map(|(_, contribution)| contribution.data.addrs().cloned()),
    );
    data.addrs()
        // Adapter events carry the wire values themselves, while
        // `TransportAddr::Display` deliberately prefixes them with their
        // transport kind (`ip:` / `relay:`). Preserve the raw socket/URL
        // representation accepted by `upsert`.
        .filter_map(|addr| match addr {
            TransportAddr::Ip(addr) => Some(addr.to_string()),
            TransportAddr::Relay(url) => Some(url.to_string()),
            _ => None,
        })
        .collect()
}

fn endpoint_data(addrs: &[String]) -> EndpointData {
    let mut data = EndpointData::default();
    for addr in addrs {
        let addr = addr.trim();
        if let Ok(addr) = addr.parse::<SocketAddr>() {
            if addr.port() <= 1 {
                continue;
            }
            data.add_addrs([TransportAddr::Ip(addr)]);
        } else if let Ok(relay) = addr.parse::<RelayUrl>() {
            data.add_relay_url(relay);
        }
    }
    data
}

fn recompute_endpoint(state: &mut State, endpoint_id: EndpointId) {
    let data = EndpointData::from_iter(
        state
            .contributions
            .values()
            .filter(|contribution| contribution.endpoint_id == endpoint_id)
            .flat_map(|contribution| contribution.data.addrs().cloned()),
    );
    if data.has_addrs() {
        state.effective.insert(endpoint_id, data);
    } else {
        state.effective.remove(&endpoint_id);
    }
}

impl AddressLookup for SourceScopedAddressLookup {
    fn resolve(
        &self,
        endpoint_id: EndpointId,
    ) -> Option<futures::stream::BoxStream<'static, Result<Item, Error>>> {
        let data = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .effective
            .get(&endpoint_id)
            .cloned()?;
        let item = Item::new(
            EndpointInfo::from_parts(endpoint_id, data),
            self.shared.provenance,
            None,
        );
        Some(Box::pin(stream::once(async move { Ok(item) })))
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use iroh::{address_lookup::AddressLookup, EndpointId, SecretKey, TransportAddr};

    use super::SourceScopedAddressLookup;

    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn direct_addrs(lookup: &SourceScopedAddressLookup, id: EndpointId) -> Vec<String> {
        let Some(mut resolved) = lookup.resolve(id) else {
            return Vec::new();
        };
        let item = futures::executor::block_on(async { resolved.next().await })
            .expect("one lookup item")
            .expect("successful lookup");
        let mut addrs: Vec<String> = item
            .endpoint_info()
            .addrs()
            .filter_map(|addr| match addr {
                TransportAddr::Ip(addr) => Some(addr.to_string()),
                _ => None,
            })
            .collect();
        addrs.sort();
        addrs
    }

    #[test]
    fn replacing_an_instance_retracts_its_old_node_atomically() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let source = lookup.new_source();
        let old_id = endpoint_id(1);
        let new_id = endpoint_id(2);

        source
            .upsert(
                "stable-instance",
                &old_id.to_string(),
                &["192.168.1.2:4433".to_string()],
            )
            .expect("valid first record");
        let update = source
            .upsert(
                "stable-instance",
                &new_id.to_string(),
                &["192.168.1.3:4434".to_string()],
            )
            .expect("valid replacement record");

        assert_eq!(update.node_id, crate::base32_encode(new_id.as_bytes()));
        assert_eq!(update.effective_addrs, ["192.168.1.3:4434"]);
        let replaced = update.replaced.expect("old-node transition");
        assert_eq!(replaced.node_id, crate::base32_encode(old_id.as_bytes()));
        assert!(!replaced.has_remaining_contributions);
        assert!(replaced.remaining_addrs.is_empty());

        assert!(
            lookup.resolve(old_id).is_none(),
            "the replaced node must be retracted"
        );
        assert_eq!(direct_addrs(&lookup, new_id), ["192.168.1.3:4434"]);
    }

    #[test]
    fn changing_instance_identity_keeps_other_sources_out_of_its_event_snapshot() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let replacing_source = lookup.new_source();
        let overlapping_source = lookup.new_source();
        let old_id = endpoint_id(10);
        let new_id = endpoint_id(11);

        replacing_source
            .upsert(
                "stable-instance",
                &old_id.to_string(),
                &["192.168.10.1:4433".to_string()],
            )
            .unwrap();
        overlapping_source
            .upsert(
                "other-instance",
                &old_id.to_string(),
                &["192.168.10.2:4433".to_string()],
            )
            .unwrap();

        let update = replacing_source
            .upsert(
                "stable-instance",
                &new_id.to_string(),
                &["192.168.11.1:4433".to_string()],
            )
            .unwrap();
        let replaced = update.replaced.expect("old-node transition");

        assert!(!replaced.has_remaining_contributions);
        assert!(replaced.remaining_addrs.is_empty());
        assert_eq!(direct_addrs(&lookup, old_id), ["192.168.10.2:4433"]);
        assert_eq!(update.effective_addrs, ["192.168.11.1:4433"]);
        assert_eq!(direct_addrs(&lookup, new_id), ["192.168.11.1:4433"]);
    }

    #[test]
    fn replacing_an_instance_on_the_same_node_drops_stale_addresses() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let source = lookup.new_source();
        let id = endpoint_id(7);

        source
            .upsert(
                "stable-instance",
                &id.to_string(),
                &["192.168.1.7:4433".to_string()],
            )
            .unwrap();
        source
            .upsert(
                "stable-instance",
                &id.to_string(),
                &["192.168.1.8:4434".to_string()],
            )
            .unwrap();

        assert_eq!(direct_addrs(&lookup, id), ["192.168.1.8:4434"]);
    }

    #[test]
    fn invalid_instance_replacement_retracts_the_previous_valid_record() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let source = lookup.new_source();
        let id = endpoint_id(8);

        source
            .upsert(
                "stable-instance",
                &id.to_string(),
                &["192.168.1.8:4433".to_string()],
            )
            .unwrap();
        let error = source
            .upsert(
                "stable-instance",
                "not-a-valid-node-id",
                &["192.168.1.9:4434".to_string()],
            )
            .expect_err("malformed authoritative update must be rejected");

        assert!(error.to_string().contains("invalid node id"));
        let removal = error
            .into_removal()
            .expect("the rejected update removed its old contribution");
        assert_eq!(removal.node_id, crate::base32_encode(id.as_bytes()));
        assert!(!removal.has_remaining_contributions);
        assert!(removal.remaining_addrs.is_empty());
        assert!(
            lookup.resolve(id).is_none(),
            "a malformed replacement must not preserve the stale dial target"
        );
    }

    #[test]
    fn removing_one_source_keeps_the_other_source_in_the_global_lookup_only() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let source_a = lookup.new_source();
        let source_b = lookup.new_source();
        let id = endpoint_id(3);
        let node_id = id.to_string();

        source_a
            .upsert(
                "peer-instance",
                &node_id,
                &["10.0.0.1:4433".to_string(), "10.0.0.2:4433".to_string()],
            )
            .unwrap();
        source_b
            .upsert(
                "peer-instance",
                &node_id,
                &["10.0.0.2:4433".to_string(), "10.0.0.3:4433".to_string()],
            )
            .unwrap();

        assert_eq!(
            direct_addrs(&lookup, id),
            ["10.0.0.1:4433", "10.0.0.2:4433", "10.0.0.3:4433"],
            "live contributions must be unioned and deduplicated"
        );
        let removal = source_a
            .remove_with_snapshot("peer-instance")
            .expect("source A contribution");
        assert_eq!(removal.node_id, crate::base32_encode(id.as_bytes()));
        assert!(!removal.has_remaining_contributions);
        assert!(
            removal.remaining_addrs.is_empty(),
            "one browse must not publish another browse's snapshot"
        );
        assert_eq!(
            direct_addrs(&lookup, id),
            ["10.0.0.2:4433", "10.0.0.3:4433"],
            "removing source A must not erase source B"
        );
    }

    #[test]
    fn removal_preserves_presence_when_the_remaining_instance_is_not_dialable() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let source = lookup.new_source();
        let id = endpoint_id(9);
        let node_id = id.to_string();

        source
            .upsert(
                "dialable-instance",
                &node_id,
                &["10.0.0.9:4433".to_string()],
            )
            .unwrap();
        source
            .upsert(
                "present-without-path",
                &node_id,
                &["10.0.0.10:1".to_string()],
            )
            .unwrap();

        let removal = source
            .remove_with_snapshot("dialable-instance")
            .expect("dialable contribution");
        assert!(removal.has_remaining_contributions);
        assert!(removal.remaining_addrs.is_empty());
        assert!(lookup.resolve(id).is_none());
    }

    #[test]
    fn undialable_or_empty_updates_never_resolve() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let source = lookup.new_source();
        let id = endpoint_id(4);
        let node_id = id.to_string();

        source
            .upsert(
                "peer-instance",
                &node_id,
                &[
                    "192.168.1.9:0".to_string(),
                    "192.168.1.9:1".to_string(),
                    "not-an-address".to_string(),
                ],
            )
            .unwrap();
        assert!(
            lookup.resolve(id).is_none(),
            "port 0/1 and malformed inputs are not dialable"
        );

        source
            .upsert("peer-instance", &node_id, &["192.168.1.9:4433".to_string()])
            .unwrap();
        assert_eq!(direct_addrs(&lookup, id), ["192.168.1.9:4433"]);

        source.upsert("peer-instance", &node_id, &[]).unwrap();
        assert!(
            lookup.resolve(id).is_none(),
            "an empty update must retract the previous dialable contribution"
        );
    }

    #[test]
    fn only_the_last_source_clone_retires_its_contributions() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let source = lookup.new_source();
        let source_clone = source.clone();
        let id = endpoint_id(5);

        source
            .upsert(
                "peer-instance",
                &id.to_string(),
                &["172.16.0.4:4433".to_string()],
            )
            .unwrap();
        drop(source);
        assert_eq!(direct_addrs(&lookup, id), ["172.16.0.4:4433"]);

        drop(source_clone);
        assert!(
            lookup.resolve(id).is_none(),
            "dropping the final lease clone must retire the source"
        );
    }

    #[test]
    fn explicit_retirement_is_idempotent_and_prevents_resurrection() {
        let lookup = SourceScopedAddressLookup::new("test-discovery");
        let source = lookup.new_source();
        let source_clone = source.clone();
        let id = endpoint_id(6);

        source
            .upsert(
                "peer-instance",
                &id.to_string(),
                &["172.16.0.5:4433".to_string()],
            )
            .unwrap();
        source.retire();
        source.retire();

        assert!(lookup.resolve(id).is_none());
        assert!(
            source_clone
                .upsert(
                    "late-instance",
                    &id.to_string(),
                    &["172.16.0.6:4433".to_string()],
                )
                .is_err(),
            "a late callback must not resurrect a retired browse source"
        );
        assert_eq!(source_clone.remove("peer-instance"), None);
        assert!(lookup.resolve(id).is_none());
    }
}
