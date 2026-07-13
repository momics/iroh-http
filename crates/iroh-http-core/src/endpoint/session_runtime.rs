//! SessionRuntime subsystem — serve-loop state, lifecycle signals,
//! transport events, and per-peer path subscriptions.
//!
//! Slice E (#187) is complete: session.rs moved into mod ffi, not http/.
//! SessionRuntime intentionally stays here alongside IrohEndpoint (tight
//! lifecycle coupling; no further move is planned).

use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Mutex;

use dashmap::DashMap;
use tokio::sync::{mpsc, watch};

use crate::http::events::TransportEvent;
use crate::http::server::ServeHandle;

use super::stats::PathInfo;

/// Server-side runtime: the `serve()` task, lifecycle signals, and
/// observability fan-out (transport events, per-peer path subscriptions).
pub(in crate::endpoint) struct SessionRuntime {
    /// Active serve handle, if `serve()` has been called.
    pub(in crate::endpoint) serve_handle: Mutex<Option<ServeHandle>>,
    /// Monotonic generation of the serve cycle currently starting. Bumped by
    /// `set_local_service` (the first observable step of a `serve()` cycle),
    /// under the `serve_handle` lock. `set_serve_handle` reads it as the
    /// generation of the handle it is registering.
    pub(in crate::endpoint) serve_started_gen: AtomicU64,
    /// Highest serve generation for which a `stop_serve()` has been requested.
    /// Set by `stop_serve` under the `serve_handle` lock. `set_serve_handle`
    /// shuts down its new handle when `serve_stopped_gen >= serve_started_gen`,
    /// so a stop that races a restart — targeting either the not-yet-registered
    /// new handle or the stale old one — is honoured against the correct
    /// generation instead of being lost (F7). This generalizes the old
    /// early-stop boolean: it covers both the pre-registration stop and the
    /// restart-with-concurrent-stop race.
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
    /// Per-peer path-change subscriptions. Key: `node_id_str`. Populated
    /// lazily when `subscribe_path_changes` is called.
    pub(in crate::endpoint) path_subs: DashMap<String, mpsc::UnboundedSender<PathInfo>>,
    /// Number of live path-change watcher tasks.
    pub(in crate::endpoint) active_path_watchers: AtomicUsize,
}
