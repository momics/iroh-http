//! Per-endpoint request queues for the Deno serve bridge.
//!
//! Incoming [`RequestPayload`] values stay in a bounded `mpsc` channel. A
//! lifetime-stable Deno `UnsafeCallback` only notifies the TypeScript adapter
//! that work is ready; TypeScript then synchronously drains the queue. No
//! borrowed payload pointer crosses the callback boundary.
//!
//! Connection events (peer connect/disconnect) are similarly queued — the
//! TypeScript adapter polls them via `nextConnectionEvent`.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use tokio::sync::mpsc;

const QUEUE_CAPACITY: usize = 256;

/// A queued request ready to be drained by the TypeScript request loop.
pub type QueuedRequest = serde_json::Value;

/// A queued connection event (peer connect / disconnect).
pub type QueuedConnectionEvent = serde_json::Value;

/// Callback that wakes the Deno event loop with an opaque serve-generation token.
#[derive(Clone, Copy)]
pub(crate) struct RequestReadyWaker {
    callback: extern "C" fn(u64),
    token: u64,
}

impl RequestReadyWaker {
    pub(crate) fn new(callback: extern "C" fn(u64), token: u64) -> Self {
        Self { callback, token }
    }

    fn wake(self) {
        (self.callback)(self.token);
    }
}

/// Receiver half — held in the registry and drained by the TypeScript adapter.
pub struct ServeQueue {
    pub tx: mpsc::Sender<QueuedRequest>,
    pub rx: tokio::sync::Mutex<mpsc::Receiver<QueuedRequest>>,
    /// Connection event channel — pushed by the serve loop, polled by `nextConnectionEvent`.
    pub conn_tx: mpsc::Sender<QueuedConnectionEvent>,
    pub conn_rx: tokio::sync::Mutex<mpsc::Receiver<QueuedConnectionEvent>>,
    /// Persistent shutdown signal: `watch::Sender` is cloned into the registry;
    /// `nextRequest` holds a receiver and races `recv()` against this changing to `true`.
    /// Unlike a `Notify`, `watch` persists its last value, so callers that arrive
    /// after `shutdown()` is triggered still see the closed state immediately.
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
    request_ready_waker: Mutex<Option<RequestReadyWaker>>,
}

impl ServeQueue {
    /// Install the request-ready callback and wake it if work arrived first.
    pub fn set_request_ready_waker(&self, waker: RequestReadyWaker) {
        *self
            .request_ready_waker
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(waker);

        let request_waiting = self.rx.try_lock().map(|rx| !rx.is_empty()).unwrap_or(true);
        if *self.shutdown_rx.borrow() || request_waiting {
            waker.wake();
        }
    }

    /// Notify the TypeScript adapter that it should drain the request queue.
    pub fn wake_request_loop(&self) {
        let waker = *self
            .request_ready_waker
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

fn registry() -> &'static Mutex<HashMap<u64, std::sync::Arc<ServeQueue>>> {
    static R: OnceLock<Mutex<HashMap<u64, std::sync::Arc<ServeQueue>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Create and register a serve queue for an endpoint.
/// Returns a clone of the `Arc` so the serve loop can hold its own `tx` reference.
pub fn register(endpoint_handle: u64) -> std::sync::Arc<ServeQueue> {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let (conn_tx, conn_rx) = mpsc::channel(QUEUE_CAPACITY);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let queue = std::sync::Arc::new(ServeQueue {
        tx,
        rx: tokio::sync::Mutex::new(rx),
        conn_tx,
        conn_rx: tokio::sync::Mutex::new(conn_rx),
        shutdown_tx,
        shutdown_rx,
        request_ready_waker: Mutex::new(None),
    });
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(endpoint_handle, std::sync::Arc::clone(&queue));
    queue
}

/// Retrieve the queue for an endpoint (used by `nextRequest` / `nextConnectionEvent`).
pub fn get(endpoint_handle: u64) -> Option<std::sync::Arc<ServeQueue>> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&endpoint_handle)
        .cloned()
}

/// Signal shutdown to all pending `nextRequest` callers, then remove the queue.
///
/// ISS-012 / issue-12: sending `true` on the watch channel wakes any currently
/// blocked `recv()` in `nextRequest`, and any future callers will also observe
/// the shutdown state immediately (watch persists its last value).
pub fn remove(endpoint_handle: u64) {
    let queue = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&endpoint_handle);
    if let Some(queue) = queue {
        // Trigger shutdown and wake the event-driven synchronous drain loop.
        let _ = queue.shutdown_tx.send(true);
        queue.wake_request_loop();
    }
}

/// Signal shutdown without removing the queue from the registry.
///
/// This allows the JS request loop to observe shutdown immediately via the
/// watch channel and wake callback, while the caller can still drain queued
/// items before the queue is removed.
pub fn signal_shutdown(endpoint_handle: u64) {
    if let Some(queue) = get(endpoint_handle) {
        let _ = queue.shutdown_tx.send(true);
        queue.wake_request_loop();
    }
}

/// Signal shutdown to *every* registered serve queue and drain the registry.
///
/// Called from `iroh_http_close_all` so that a SIGINT path which bypasses
/// `closeEndpoint` still wakes JS request loops; otherwise the Deno process
/// would never exit (issue #155).
pub fn shutdown_all() {
    let drained: Vec<std::sync::Arc<ServeQueue>> = {
        let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
        map.drain().map(|(_, q)| q).collect()
    };
    for queue in drained {
        let _ = queue.shutdown_tx.send(true);
        queue.wake_request_loop();
    }
}
