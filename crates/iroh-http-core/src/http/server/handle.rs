//! `ServeHandle` — the join handle / shutdown switch returned by
//! [`crate::http::server::serve_with_events`].
//!
//! Split out of `mod.rs` per Slice C.7 of #182.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct ServeHandle {
    pub(super) join: tokio::task::JoinHandle<()>,
    pub(super) shutdown_notify: Arc<tokio::sync::Notify>,
    /// Set to `true` and paired with `close_connections` to force the accept
    /// loop's active per-connection tasks to close their QUIC connections
    /// immediately (used when a serve loop is replaced, not on graceful stop).
    pub(super) close_flag: Arc<AtomicBool>,
    /// Woken (via `notify_waiters`) to tell active connection tasks to
    /// re-check `close_flag` and tear their connection down.
    pub(super) close_connections: Arc<tokio::sync::Notify>,
    pub(super) drain_timeout: std::time::Duration,
    /// Resolves to `true` once the serve task has fully exited.
    pub(super) done_rx: tokio::sync::watch::Receiver<bool>,
}

impl ServeHandle {
    pub fn shutdown(&self) {
        self.shutdown_notify.notify_one();
    }
    /// Stop the accept loop **and** force every active connection closed.
    ///
    /// Unlike [`shutdown`], which lets in-flight requests drain on their
    /// existing (pooled) connections, this severs the connections so remote
    /// peers must reconnect. Used when a serve loop is being *replaced* on the
    /// same endpoint (#336): a client's pooled connection would otherwise keep
    /// being served by the old loop's handler, returning stale responses (the
    /// iOS foreground "blanket 200" symptom). Closing the connections forces
    /// the peer to redial and land on the new serve loop.
    pub fn shutdown_and_close(&self) {
        self.close_flag.store(true, Ordering::Release);
        self.shutdown_notify.notify_one();
        self.close_connections.notify_waiters();
    }
    pub async fn drain(self) {
        self.shutdown();
        let _ = self.join.await;
    }
    pub fn abort(&self) {
        self.join.abort();
    }
    pub fn drain_timeout(&self) -> std::time::Duration {
        self.drain_timeout
    }
    /// Subscribe to the serve-loop-done signal.
    ///
    /// The returned receiver resolves (changes to `true`) once the serve task
    /// has fully exited, including the drain phase.
    pub fn subscribe_done(&self) -> tokio::sync::watch::Receiver<bool> {
        self.done_rx.clone()
    }
}
