//! Generation-safe ownership for DNS-SD browse and advertisement sessions.
//!
//! A start command captures an owner token *before* creating its native or
//! desktop resource. Endpoint close, WebView destruction, and page navigation
//! advance the corresponding generation and atomically drain existing
//! sessions. A late start can then no longer attach itself to the new context:
//! conditional tracking rejects the stale token and the caller tears the new
//! resource down immediately.

#![cfg_attr(all(not(feature = "discovery"), not(mobile)), allow(dead_code))]

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DiscoveryKind {
    BrowsePeers,
    AdvertisePeer,
    GenericAdvertise,
    GenericBrowse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiscoveryOwner {
    Peer {
        endpoint_handle: u64,
        endpoint_generation: u64,
        webview_label: String,
        webview_generation: u64,
    },
    Webview {
        webview_label: String,
        webview_generation: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackedDiscovery {
    pub(crate) kind: DiscoveryKind,
    pub(crate) handle: u64,
    owner: DiscoveryOwner,
}

#[derive(Debug, Clone)]
pub(crate) struct PeerOwnerToken {
    endpoint_handle: u64,
    endpoint_generation: u64,
    webview_label: String,
    webview_generation: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct WebviewOwnerToken {
    webview_label: String,
    webview_generation: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct OwnerGeneration {
    generation: u64,
    active: bool,
}

#[derive(Default)]
struct OwnershipState {
    sessions: HashMap<(DiscoveryKind, u64), TrackedDiscovery>,
    endpoints: HashMap<u64, OwnerGeneration>,
    webviews: HashMap<String, OwnerGeneration>,
}

fn state() -> &'static Mutex<OwnershipState> {
    static STATE: OnceLock<Mutex<OwnershipState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(OwnershipState::default()))
}

fn next_generation(current: u64) -> u64 {
    current
        .checked_add(1)
        .expect("discovery owner generation exhausted")
}

/// Mark a newly inserted endpoint handle as a fresh live generation.
pub(crate) fn activate_endpoint(endpoint_handle: u64) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let owner = state.endpoints.entry(endpoint_handle).or_default();
    owner.generation = next_generation(owner.generation);
    owner.active = true;
}

/// Capture ownership before starting a peer browse/advertisement.
pub(crate) fn begin_peer(endpoint_handle: u64, webview_label: &str) -> Option<PeerOwnerToken> {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let endpoint = *state.endpoints.get(&endpoint_handle)?;
    if !endpoint.active {
        return None;
    }
    // The page-load hook normally creates this entry before JS can invoke a
    // command. Lazily initialize only the never-seen case for test/custom
    // runtimes; a retired context remains inactive until the next page load.
    let webview = state
        .webviews
        .entry(webview_label.to_string())
        .or_insert(OwnerGeneration {
            generation: 1,
            active: true,
        });
    if !webview.active {
        return None;
    }
    Some(PeerOwnerToken {
        endpoint_handle,
        endpoint_generation: endpoint.generation,
        webview_label: webview_label.to_string(),
        webview_generation: webview.generation,
    })
}

/// Capture ownership before starting a generic DNS-SD resource.
pub(crate) fn begin_webview(webview_label: &str) -> Option<WebviewOwnerToken> {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let webview = state
        .webviews
        .entry(webview_label.to_string())
        .or_insert(OwnerGeneration {
            generation: 1,
            active: true,
        });
    if !webview.active {
        return None;
    }
    Some(WebviewOwnerToken {
        webview_label: webview_label.to_string(),
        webview_generation: webview.generation,
    })
}

fn peer_owner_is_current(state: &OwnershipState, token: &PeerOwnerToken) -> bool {
    let endpoint_current = state
        .endpoints
        .get(&token.endpoint_handle)
        .is_some_and(|owner| owner.active && owner.generation == token.endpoint_generation);
    let webview_current = state
        .webviews
        .get(&token.webview_label)
        .is_some_and(|owner| owner.active && owner.generation == token.webview_generation);
    endpoint_current && webview_current
}

fn webview_owner_is_current(state: &OwnershipState, token: &WebviewOwnerToken) -> bool {
    state
        .webviews
        .get(&token.webview_label)
        .is_some_and(|owner| owner.active && owner.generation == token.webview_generation)
}

/// Revalidate a captured peer owner before an expensive asynchronous start.
#[cfg(any(mobile, test))]
pub(crate) fn is_peer_current(token: &PeerOwnerToken) -> bool {
    let state = state().lock().unwrap_or_else(|error| error.into_inner());
    peer_owner_is_current(&state, token)
}

/// Revalidate a captured WebView owner before an expensive asynchronous start.
#[cfg(any(mobile, test))]
pub(crate) fn is_webview_current(token: &WebviewOwnerToken) -> bool {
    let state = state().lock().unwrap_or_else(|error| error.into_inner());
    webview_owner_is_current(&state, token)
}

/// Conditionally attach a peer resource to the exact captured generations.
pub(crate) fn track_peer(
    token: PeerOwnerToken,
    kind: DiscoveryKind,
    handle: u64,
) -> Result<(), ()> {
    debug_assert!(matches!(
        kind,
        DiscoveryKind::BrowsePeers | DiscoveryKind::AdvertisePeer
    ));
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    if !peer_owner_is_current(&state, &token) {
        return Err(());
    }
    state.sessions.insert(
        (kind, handle),
        TrackedDiscovery {
            kind,
            handle,
            owner: DiscoveryOwner::Peer {
                endpoint_handle: token.endpoint_handle,
                endpoint_generation: token.endpoint_generation,
                webview_label: token.webview_label,
                webview_generation: token.webview_generation,
            },
        },
    );
    Ok(())
}

/// Conditionally attach a generic resource to one page generation.
pub(crate) fn track_generic(
    token: WebviewOwnerToken,
    kind: DiscoveryKind,
    handle: u64,
) -> Result<(), ()> {
    debug_assert!(matches!(
        kind,
        DiscoveryKind::GenericAdvertise | DiscoveryKind::GenericBrowse
    ));
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    if !webview_owner_is_current(&state, &token) {
        return Err(());
    }
    state.sessions.insert(
        (kind, handle),
        TrackedDiscovery {
            kind,
            handle,
            owner: DiscoveryOwner::Webview {
                webview_label: token.webview_label,
                webview_generation: token.webview_generation,
            },
        },
    );
    Ok(())
}

pub(crate) fn untrack(kind: DiscoveryKind, handle: u64) -> Option<TrackedDiscovery> {
    state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .sessions
        .remove(&(kind, handle))
}

fn take_matching(
    state: &mut OwnershipState,
    mut predicate: impl FnMut(&TrackedDiscovery) -> bool,
) -> Vec<TrackedDiscovery> {
    let keys: Vec<_> = state
        .sessions
        .iter()
        .filter_map(|(key, session)| predicate(session).then_some(*key))
        .collect();
    keys.into_iter()
        .filter_map(|key| state.sessions.remove(&key))
        .collect()
}

/// Invalidate one endpoint generation and drain exactly its peer resources.
pub(crate) fn retire_endpoint(endpoint_handle: u64) -> Vec<TrackedDiscovery> {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let owner = state.endpoints.entry(endpoint_handle).or_default();
    owner.generation = next_generation(owner.generation);
    owner.active = false;
    take_matching(&mut state, |session| {
        matches!(
            session.owner,
            DiscoveryOwner::Peer { endpoint_handle: owner, .. }
                if owner == endpoint_handle
        )
    })
}

/// Begin a fresh page generation and drain resources from the prior context.
pub(crate) fn advance_webview(webview_label: &str) -> Vec<TrackedDiscovery> {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let owner = state.webviews.entry(webview_label.to_string()).or_default();
    owner.generation = next_generation(owner.generation);
    owner.active = true;
    take_matching(&mut state, |session| owner_label(session) == webview_label)
}

/// Invalidate a destroyed WebView and drain its peer and generic resources.
pub(crate) fn retire_webview(webview_label: &str) -> Vec<TrackedDiscovery> {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let owner = state.webviews.entry(webview_label.to_string()).or_default();
    owner.generation = next_generation(owner.generation);
    owner.active = false;
    take_matching(&mut state, |session| owner_label(session) == webview_label)
}

fn owner_label(session: &TrackedDiscovery) -> &str {
    match &session.owner {
        DiscoveryOwner::Peer { webview_label, .. }
        | DiscoveryOwner::Webview { webview_label, .. } => webview_label,
    }
}

/// Invalidate every owner and drain every remaining resource.
pub(crate) fn retire_all() -> Vec<TrackedDiscovery> {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    for owner in state.endpoints.values_mut() {
        owner.generation = next_generation(owner.generation);
        owner.active = false;
    }
    for owner in state.webviews.values_mut() {
        owner.generation = next_generation(owner.generation);
        owner.active = false;
    }
    state.sessions.drain().map(|(_, session)| session).collect()
}

#[cfg(test)]
pub(crate) fn ownership_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear() {
        *state().lock().unwrap_or_else(|error| error.into_inner()) = OwnershipState::default();
    }

    #[tokio::test]
    async fn endpoint_retirement_is_scoped_and_rejects_a_late_start() {
        let _guard = ownership_test_lock().lock().await;
        clear();
        activate_endpoint(10);
        activate_endpoint(20);
        advance_webview("main");
        let late = begin_peer(10, "main").unwrap();
        let live = begin_peer(20, "main").unwrap();
        track_peer(live, DiscoveryKind::BrowsePeers, 2).unwrap();

        assert!(retire_endpoint(10).is_empty());
        assert!(track_peer(late, DiscoveryKind::BrowsePeers, 1).is_err());
        assert_eq!(retire_endpoint(20).len(), 1);
        clear();
    }

    #[tokio::test]
    async fn page_advance_drains_old_context_and_rejects_its_late_start() {
        let _guard = ownership_test_lock().lock().await;
        clear();
        activate_endpoint(10);
        advance_webview("main");
        let old_peer = begin_peer(10, "main").unwrap();
        let old_generic = begin_webview("main").unwrap();
        track_generic(old_generic.clone(), DiscoveryKind::GenericBrowse, 3).unwrap();

        let drained = advance_webview("main");
        assert_eq!(drained.len(), 1);
        assert!(track_peer(old_peer, DiscoveryKind::BrowsePeers, 4).is_err());
        assert!(track_generic(old_generic, DiscoveryKind::GenericBrowse, 5).is_err());
        assert!(begin_peer(10, "main").is_some());
        clear();
    }

    #[tokio::test]
    async fn kind_is_part_of_identity_and_explicit_close_is_idempotent() {
        let _guard = ownership_test_lock().lock().await;
        clear();
        activate_endpoint(10);
        advance_webview("main");
        track_peer(
            begin_peer(10, "main").unwrap(),
            DiscoveryKind::BrowsePeers,
            5,
        )
        .unwrap();
        track_generic(
            begin_webview("main").unwrap(),
            DiscoveryKind::GenericBrowse,
            5,
        )
        .unwrap();

        assert!(untrack(DiscoveryKind::BrowsePeers, 5).is_some());
        assert!(untrack(DiscoveryKind::BrowsePeers, 5).is_none());
        assert!(untrack(DiscoveryKind::GenericBrowse, 5).is_some());
        clear();
    }
}
