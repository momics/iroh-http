//! SessionRuntime subsystem — serve-loop state, lifecycle signals,
//! transport events, and per-peer path subscriptions.
//!
//! Slice E (#187) is complete: session.rs moved into mod ffi, not http/.
//! SessionRuntime intentionally stays here alongside IrohEndpoint (tight
//! lifecycle coupling; no further move is planned).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Mutex;

use tokio::sync::{mpsc, watch};

use crate::http::events::TransportEvent;
use crate::http::server::handle::ServeToken;
use crate::http::server::ServeHandle;

use super::stats::PathInfo;

pub(in crate::endpoint) struct PathSubscriptions {
    pub(in crate::endpoint) senders: Mutex<HashMap<u32, mpsc::UnboundedSender<PathInfo>>>,
}

/// Server-side runtime: the `serve()` task, lifecycle signals, and
/// observability fan-out (transport events, per-peer path subscriptions).
pub(in crate::endpoint) struct SessionRuntime {
    /// Identity-scoped active serve slot. The handle carries the token for the
    /// concrete cycle currently controlled by lifecycle operations.
    pub(in crate::endpoint) serve_handle: Mutex<Option<ServeHandle>>,
    /// Per-endpoint allocator for stable serve tokens. This counter only
    /// creates identities; lifecycle decisions never infer state by comparing
    /// independently-mutated generations.
    pub(in crate::endpoint) serve_started_gen: AtomicU64,
    /// Last token for which `stop_serve()` was requested. Kept as diagnostic
    /// bookkeeping for the existing runtime layout; it never controls whether
    /// a future cycle starts or stops.
    pub(in crate::endpoint) serve_stopped_gen: AtomicU64,
    /// Done-signal receiver from the active serve task. Stored separately
    /// so `wait_serve_stop()` can await without holding the `serve_handle` lock.
    pub(in crate::endpoint) serve_done_rx: Mutex<Option<watch::Receiver<bool>>>,
    /// Signals `true` when the endpoint has fully closed (either explicitly
    /// or because the serve loop exited due to native shutdown).
    pub(in crate::endpoint) closed_tx: watch::Sender<bool>,
    pub(in crate::endpoint) closed_rx: watch::Receiver<bool>,
    /// Sender for transport-level events (pool hits/misses, path changes, sweep).
    pub(in crate::endpoint) event_tx: mpsc::Sender<TransportEvent>,
    /// Receiver for transport-level events. Wrapped in Mutex+Option so
    /// `subscribe_events()` can take it exactly once for the platform drain task.
    pub(in crate::endpoint) event_rx: Mutex<Option<mpsc::Receiver<TransportEvent>>>,
    /// Per-peer path-change subscriptions. The inner key is an adapter-owned
    /// subscription ID, allowing one watcher to fan out without one iterator's
    /// cancellation terminating another.
    pub(in crate::endpoint) path_subs: Mutex<HashMap<String, std::sync::Arc<PathSubscriptions>>>,
    /// Number of live path-change watcher tasks.
    pub(in crate::endpoint) active_path_watchers: AtomicUsize,
}

impl SessionRuntime {
    /// Mint the identity for one serve cycle. Tokens are endpoint-local and
    /// compared only while their handles can coexist, so wrapping cannot create
    /// an ABA match in practice.
    pub(in crate::endpoint) fn next_serve_token(&self) -> ServeToken {
        let token = self
            .serve_started_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        ServeToken::new(token)
    }

    /// Record a stop for diagnostics only. Lifecycle control uses the handle in
    /// the locked serve slot, never this historical value.
    pub(in crate::endpoint) fn record_stopped_token(&self, token: ServeToken) {
        self.serve_stopped_gen
            .store(token.get(), std::sync::atomic::Ordering::Relaxed);
    }
}
