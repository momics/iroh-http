//! `ServeHandle` — the join handle / shutdown switch returned by
//! [`crate::http::server::serve_with_events`].
//!
//! Split out of `mod.rs` per Slice C.7 of #182.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Stable identity of one serve cycle on an endpoint.
///
/// The identity, rather than a separately-mutated generation pair, lets the
/// endpoint distinguish an adapter confirming the current handle from a late
/// hand-off of a handle that has already been replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServeToken(u64);

impl ServeToken {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

struct ServeHandleInner {
    token: ServeToken,
    /// Installed immediately after the endpoint registers this cycle. Keeping
    /// the join in a shared slot lets `stop_serve` win the race before the task
    /// is spawned without losing the stop request.
    join: Mutex<Option<tokio::task::JoinHandle<()>>>,
    abort_requested: AtomicBool,
    shutdown_requested: AtomicBool,
    shutdown_notify: Arc<tokio::sync::Notify>,
    /// Set to `true` and paired with `close_connections` to force the accept
    /// loop's active per-connection tasks to close their QUIC connections.
    close_flag: Arc<AtomicBool>,
    /// Woken to tell active connection tasks to re-check `close_flag`.
    close_connections: Arc<tokio::sync::Notify>,
    drain_timeout: std::time::Duration,
    /// Resolves to `true` once the serve task has fully exited.
    done_rx: tokio::sync::watch::Receiver<bool>,
}

/// Control handle for one endpoint serve cycle.
///
/// Clones control the same cycle and carry the same internal token. The common
/// server path stores one clone in the endpoint before returning another to an
/// adapter, making adapter registration an idempotent confirmation.
#[derive(Clone)]
pub struct ServeHandle {
    inner: Arc<ServeHandleInner>,
}

impl ServeHandle {
    pub(super) fn pending(
        token: ServeToken,
        shutdown_notify: Arc<tokio::sync::Notify>,
        close_flag: Arc<AtomicBool>,
        close_connections: Arc<tokio::sync::Notify>,
        drain_timeout: std::time::Duration,
        done_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self {
            inner: Arc::new(ServeHandleInner {
                token,
                join: Mutex::new(None),
                abort_requested: AtomicBool::new(false),
                shutdown_requested: AtomicBool::new(false),
                shutdown_notify,
                close_flag,
                close_connections,
                drain_timeout,
                done_rx,
            }),
        }
    }

    /// Attach the spawned accept task to a cycle that is already registered.
    /// If an immediate close raced ahead, abort the task before it can run.
    pub(super) fn attach_join(&self, join: tokio::task::JoinHandle<()>) {
        let mut slot = self
            .inner
            .join
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.inner.abort_requested.load(Ordering::Acquire) {
            join.abort();
        } else {
            *slot = Some(join);
        }
    }

    pub(crate) fn token(&self) -> ServeToken {
        self.inner.token
    }

    pub(crate) fn is_same_cycle(&self, other: &Self) -> bool {
        self.token() == other.token()
    }

    pub(crate) fn is_shutdown_requested(&self) -> bool {
        self.inner.shutdown_requested.load(Ordering::Acquire)
    }

    /// Gracefully stop the serve loop: stop accepting new connections, drain
    /// in-flight requests (up to `drain_timeout`), then close the remaining
    /// active connections so no request is served after the loop has stopped.
    pub fn shutdown(&self) {
        self.inner.shutdown_requested.store(true, Ordering::Release);
        self.inner.shutdown_notify.notify_one();
    }

    /// Stop the accept loop **and** force every active connection closed
    /// *immediately*, without waiting for the graceful drain.
    ///
    /// Unlike [`shutdown`], which drains in-flight requests before closing
    /// connections, this severs the connections up front so remote peers must
    /// reconnect right away. Used when a serve loop is being *replaced* on the
    /// same endpoint (#336).
    pub fn shutdown_and_close(&self) {
        self.inner.shutdown_requested.store(true, Ordering::Release);
        self.inner.close_flag.store(true, Ordering::Release);
        self.inner.shutdown_notify.notify_one();
        self.inner.close_connections.notify_waiters();
    }

    pub async fn drain(self) {
        self.shutdown();

        let join = self
            .inner
            .join
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(join) = join {
            let _ = join.await;
        } else {
            // `close()` can race the short register-before-spawn window. The
            // stored Notify permit stops the task when it starts; wait on its
            // completion signal when there is not a join handle yet.
            let mut done = self.subscribe_done();
            let _ = done.wait_for(|value| *value).await;
        }
    }

    pub fn abort(&self) {
        self.inner.abort_requested.store(true, Ordering::Release);
        if let Some(join) = self
            .inner
            .join
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            join.abort();
        }
    }

    pub fn drain_timeout(&self) -> std::time::Duration {
        self.inner.drain_timeout
    }

    /// Subscribe to the serve-loop-done signal.
    ///
    /// The returned receiver resolves (changes to `true`) once the serve task
    /// has fully exited, including the drain phase.
    pub fn subscribe_done(&self) -> tokio::sync::watch::Receiver<bool> {
        self.inner.done_rx.clone()
    }
}
