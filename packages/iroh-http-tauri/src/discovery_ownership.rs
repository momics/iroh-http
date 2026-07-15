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

#[cfg(any(mobile, test))]
use std::sync::Arc;
#[cfg(any(mobile, test))]
use tokio::sync::Notify;

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
#[cfg(any(test, all(feature = "discovery", not(mobile))))]
pub(crate) struct PeerOwnerToken {
    endpoint_handle: u64,
    endpoint_generation: u64,
    webview_label: String,
    webview_generation: u64,
}

#[derive(Debug, Clone)]
#[cfg(any(test, all(feature = "discovery", not(mobile))))]
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
    #[cfg(any(mobile, test))]
    starts: HashMap<u64, PendingStart>,
    #[cfg(any(mobile, test))]
    next_start: u64,
    endpoints: HashMap<u64, OwnerGeneration>,
    webviews: HashMap<String, OwnerGeneration>,
}

#[cfg(any(mobile, test))]
struct PendingStart {
    owner: DiscoveryOwner,
    completion: StartCompletion,
}

#[cfg(any(mobile, test))]
struct StartCompletionInner {
    outcome: Mutex<Option<Result<(), String>>>,
    notify: Notify,
}

/// Completion fence retained by retirement while a native start owns cleanup.
#[derive(Clone)]
#[cfg(any(mobile, test))]
pub(crate) struct StartCompletion(Arc<StartCompletionInner>);

#[cfg(any(mobile, test))]
impl StartCompletion {
    fn new() -> Self {
        Self(Arc::new(StartCompletionInner {
            outcome: Mutex::new(None),
            notify: Notify::new(),
        }))
    }

    fn finish(&self, outcome: Result<(), String>) {
        let mut current = self
            .0
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current.is_none() {
            *current = Some(outcome);
            self.0.notify.notify_waiters();
        }
    }

    pub(crate) async fn wait(&self) -> Result<(), String> {
        loop {
            let notified = self.0.notify.notified();
            if let Some(outcome) = self
                .0
                .outcome
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
            {
                return outcome;
            }
            notified.await;
        }
    }
}

/// Sessions and in-flight native starts invalidated by one owner transition.
#[derive(Default)]
pub(crate) struct DiscoveryRetirement {
    pub(crate) sessions: Vec<TrackedDiscovery>,
    #[cfg(any(mobile, test))]
    pub(crate) starts: Vec<StartCompletion>,
}

#[cfg(any(mobile, test))]
impl DiscoveryRetirement {
    pub(crate) fn is_empty(&self) -> bool {
        self.sessions.is_empty() && self.starts.is_empty()
    }
}

/// RAII ownership of the capture -> native start -> track interval.
///
/// A failed late start remains fenced until this lease is dropped, so callers
/// naturally keep the next page generation blocked through native cleanup.
#[cfg(any(mobile, test))]
pub(crate) struct StartLease {
    id: u64,
    owner: DiscoveryOwner,
    completion: StartCompletion,
    active: bool,
    native_started: bool,
}

#[cfg(any(mobile, test))]
impl StartLease {
    /// Mark that this lease now owns a native resource requiring async cleanup.
    pub(crate) fn arm_native_cleanup(&mut self) {
        self.native_started = true;
    }

    pub(crate) fn is_current(&self) -> bool {
        let state = state().lock().unwrap_or_else(|error| error.into_inner());
        owner_is_current(&state, &self.owner)
    }

    /// Atomically publish the resource and release the start fence.
    /// On failure the lease deliberately remains active through caller cleanup.
    pub(crate) fn track(&mut self, kind: DiscoveryKind, handle: u64) -> Result<(), ()> {
        let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
        if !owner_is_current(&state, &self.owner) {
            return Err(());
        }
        debug_assert!(kind_matches_owner(kind, &self.owner));
        state.sessions.insert(
            (kind, handle),
            TrackedDiscovery {
                kind,
                handle,
                owner: self.owner.clone(),
            },
        );
        state.starts.remove(&self.id);
        self.active = false;
        self.completion.finish(Ok(()));
        Ok(())
    }

    /// Release a stale start fence only after its native cleanup has finished.
    #[cfg_attr(all(test, not(mobile)), allow(dead_code))]
    pub(crate) fn finish_cleanup(mut self, outcome: Result<(), String>) {
        state()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .starts
            .remove(&self.id);
        self.active = false;
        self.completion.finish(outcome);
    }

    /// Reconcile native cleanup in a detached task which owns the lease.
    /// Dropping a caller awaiting the returned handle does not cancel cleanup.
    pub(crate) fn reconcile_cleanup<F>(
        self,
        cleanup: F,
    ) -> tokio::task::JoinHandle<Result<(), String>>
    where
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        tokio::spawn(async move {
            let outcome = cleanup.await;
            self.finish_cleanup(outcome.clone());
            outcome
        })
    }
}

#[cfg(any(mobile, test))]
impl Drop for StartLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        state()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .starts
            .remove(&self.id);
        self.active = false;
        self.completion.finish(if self.native_started {
            Err("native discovery start was abandoned before cleanup completed".to_string())
        } else {
            Ok(())
        });
    }
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

#[cfg(any(mobile, test))]
fn next_start(state: &mut OwnershipState) -> u64 {
    let id = state.next_start;
    state.next_start = state
        .next_start
        .checked_add(1)
        .expect("discovery start lease space exhausted");
    id
}

#[cfg(any(mobile, test))]
fn owner_is_current(state: &OwnershipState, owner: &DiscoveryOwner) -> bool {
    match owner {
        DiscoveryOwner::Peer {
            endpoint_handle,
            endpoint_generation,
            webview_label,
            webview_generation,
        } => {
            state
                .endpoints
                .get(endpoint_handle)
                .is_some_and(|current| current.active && current.generation == *endpoint_generation)
                && state.webviews.get(webview_label).is_some_and(|current| {
                    current.active && current.generation == *webview_generation
                })
        }
        DiscoveryOwner::Webview {
            webview_label,
            webview_generation,
        } => state
            .webviews
            .get(webview_label)
            .is_some_and(|current| current.active && current.generation == *webview_generation),
    }
}

#[cfg(any(mobile, test))]
fn kind_matches_owner(kind: DiscoveryKind, owner: &DiscoveryOwner) -> bool {
    matches!(
        (kind, owner),
        (
            DiscoveryKind::BrowsePeers | DiscoveryKind::AdvertisePeer,
            DiscoveryOwner::Peer { .. }
        ) | (
            DiscoveryKind::GenericAdvertise | DiscoveryKind::GenericBrowse,
            DiscoveryOwner::Webview { .. }
        )
    )
}

#[cfg(any(mobile, test))]
fn insert_start(state: &mut OwnershipState, owner: DiscoveryOwner) -> StartLease {
    let id = next_start(state);
    let completion = StartCompletion::new();
    state.starts.insert(
        id,
        PendingStart {
            owner: owner.clone(),
            completion: completion.clone(),
        },
    );
    StartLease {
        id,
        owner,
        completion,
        active: true,
        native_started: false,
    }
}

/// Mark a newly inserted endpoint handle as a fresh live generation.
pub(crate) fn activate_endpoint(endpoint_handle: u64) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let owner = state.endpoints.entry(endpoint_handle).or_default();
    owner.generation = next_generation(owner.generation);
    owner.active = true;
}

/// Capture ownership before starting a peer browse/advertisement.
#[cfg(any(test, all(feature = "discovery", not(mobile))))]
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
#[cfg(any(test, all(feature = "discovery", not(mobile))))]
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

/// Capture and fence a mobile peer start until it is tracked or fully cleaned.
#[cfg(any(mobile, test))]
pub(crate) fn begin_peer_start(endpoint_handle: u64, webview_label: &str) -> Option<StartLease> {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let endpoint = *state.endpoints.get(&endpoint_handle)?;
    let webview = *state
        .webviews
        .entry(webview_label.to_string())
        .or_insert(OwnerGeneration {
            generation: 1,
            active: true,
        });
    if !endpoint.active || !webview.active {
        return None;
    }
    Some(insert_start(
        &mut state,
        DiscoveryOwner::Peer {
            endpoint_handle,
            endpoint_generation: endpoint.generation,
            webview_label: webview_label.to_string(),
            webview_generation: webview.generation,
        },
    ))
}

/// Capture and fence a mobile generic start until it is tracked or cleaned.
#[cfg(any(mobile, test))]
pub(crate) fn begin_webview_start(webview_label: &str) -> Option<StartLease> {
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
    let generation = webview.generation;
    Some(insert_start(
        &mut state,
        DiscoveryOwner::Webview {
            webview_label: webview_label.to_string(),
            webview_generation: generation,
        },
    ))
}

#[cfg(any(test, all(feature = "discovery", not(mobile))))]
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

#[cfg(any(test, all(feature = "discovery", not(mobile))))]
fn webview_owner_is_current(state: &OwnershipState, token: &WebviewOwnerToken) -> bool {
    state
        .webviews
        .get(&token.webview_label)
        .is_some_and(|owner| owner.active && owner.generation == token.webview_generation)
}

/// Conditionally attach a peer resource to the exact captured generations.
#[cfg(any(test, all(feature = "discovery", not(mobile))))]
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
#[cfg(any(test, all(feature = "discovery", not(mobile))))]
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

fn take_retirement(
    state: &mut OwnershipState,
    mut predicate: impl FnMut(&DiscoveryOwner) -> bool,
) -> DiscoveryRetirement {
    let sessions = take_matching(state, |session| predicate(&session.owner));
    #[cfg(any(mobile, test))]
    let starts = state
        .starts
        .values()
        .filter(|start| predicate(&start.owner))
        .map(|start| start.completion.clone())
        .collect();
    DiscoveryRetirement {
        sessions,
        #[cfg(any(mobile, test))]
        starts,
    }
}

/// Invalidate one endpoint generation and drain exactly its peer resources.
pub(crate) fn retire_endpoint(endpoint_handle: u64) -> DiscoveryRetirement {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let owner = state.endpoints.entry(endpoint_handle).or_default();
    owner.generation = next_generation(owner.generation);
    owner.active = false;
    take_retirement(&mut state, |owner| {
        matches!(
            owner,
            DiscoveryOwner::Peer { endpoint_handle: owner, .. }
                if *owner == endpoint_handle
        )
    })
}

/// Begin a fresh page generation and drain resources from the prior context.
pub(crate) fn advance_webview(webview_label: &str) -> DiscoveryRetirement {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let owner = state.webviews.entry(webview_label.to_string()).or_default();
    owner.generation = next_generation(owner.generation);
    owner.active = true;
    take_retirement(&mut state, |owner| owner_label(owner) == webview_label)
}

/// Invalidate a destroyed WebView and drain its peer and generic resources.
pub(crate) fn retire_webview(webview_label: &str) -> DiscoveryRetirement {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let owner = state.webviews.entry(webview_label.to_string()).or_default();
    owner.generation = next_generation(owner.generation);
    owner.active = false;
    take_retirement(&mut state, |owner| owner_label(owner) == webview_label)
}

fn owner_label(owner: &DiscoveryOwner) -> &str {
    match owner {
        DiscoveryOwner::Peer { webview_label, .. }
        | DiscoveryOwner::Webview { webview_label, .. } => webview_label,
    }
}

/// Invalidate every owner and drain every remaining resource.
pub(crate) fn retire_all() -> DiscoveryRetirement {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    for owner in state.endpoints.values_mut() {
        owner.generation = next_generation(owner.generation);
        owner.active = false;
    }
    for owner in state.webviews.values_mut() {
        owner.generation = next_generation(owner.generation);
        owner.active = false;
    }
    let sessions = state.sessions.drain().map(|(_, session)| session).collect();
    #[cfg(any(mobile, test))]
    let starts = state
        .starts
        .values()
        .map(|start| start.completion.clone())
        .collect();
    DiscoveryRetirement {
        sessions,
        #[cfg(any(mobile, test))]
        starts,
    }
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
        assert_eq!(retire_endpoint(20).sessions.len(), 1);
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
        assert_eq!(drained.sessions.len(), 1);
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

    #[tokio::test]
    async fn retirement_fences_a_native_start_until_its_late_cleanup_finishes() {
        let _guard = ownership_test_lock().lock().await;
        clear();
        advance_webview("main");
        let mut start = begin_webview_start("main").unwrap();

        let retired = advance_webview("main");
        assert!(retired.sessions.is_empty());
        assert_eq!(retired.starts.len(), 1);
        assert!(start.track(DiscoveryKind::GenericAdvertise, 41).is_err());

        let mut cleanup = tokio::spawn({
            let completion = retired.starts[0].clone();
            async move { completion.wait().await }
        });
        tokio::task::yield_now().await;
        assert!(!cleanup.is_finished());

        // Represents native stop completing after the stale start callback.
        drop(start);
        (&mut cleanup).await.unwrap().unwrap();
        clear();
    }

    #[tokio::test]
    async fn peer_native_starts_use_the_same_retirement_fence() {
        let _guard = ownership_test_lock().lock().await;
        clear();
        activate_endpoint(7);
        advance_webview("main");
        let start = begin_peer_start(7, "main").unwrap();

        let retired = retire_endpoint(7);
        assert_eq!(retired.starts.len(), 1);
        drop(start);
        retired.starts[0].wait().await.unwrap();
        clear();
    }

    #[tokio::test]
    async fn cancelling_cleanup_waiter_does_not_release_armed_start_early() {
        let _guard = ownership_test_lock().lock().await;
        clear();
        advance_webview("main");
        let mut start = begin_webview_start("main").unwrap();
        start.arm_native_cleanup();
        let retired = advance_webview("main");
        let (release, wait) = tokio::sync::oneshot::channel();
        let cleanup = start.reconcile_cleanup(async move {
            wait.await.unwrap();
            Ok(())
        });
        let caller = tokio::spawn(async move { cleanup.await });

        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let mut fence = tokio::spawn({
            let completion = retired.starts[0].clone();
            async move { completion.wait().await }
        });
        tokio::task::yield_now().await;
        assert!(!fence.is_finished());

        release.send(()).unwrap();
        (&mut fence).await.unwrap().unwrap();
        clear();
    }
}
