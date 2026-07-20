//! Typed lifecycle objects for the serve loop (Slice C.4 of #182, closes #178).
//!
//! [`ConnectionTracker`] folds the previous inline `PeerConnectionGuard` +
//! `TotalGuard` into a single Drop-owning object. [`RequestTracker`] owns the
//! public active-request statistic, while [`DeliveryTracker`] owns the private
//! graceful-drain boundary through transport acknowledgement.
//!
//! Counter mutations and connect/disconnect-event firing happen exactly once
//! in `acquire` / `Drop`, so a future change can no longer drift between
//! sites.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::ConnectionEvent;

use super::ConnectionEventFn;

// ── ConnectionTracker ─────────────────────────────────────────────────────────

/// Per-connection lifecycle: enforces the per-peer cap, increments the
/// total-connection counter, and fires connect/disconnect events on the
/// 0→1 / 1→0 per-peer transitions. Drop releases all three.
pub(crate) struct ConnectionTracker {
    counts: Arc<Mutex<HashMap<iroh::PublicKey, usize>>>,
    peer: iroh::PublicKey,
    peer_id_str: String,
    on_event: Option<ConnectionEventFn>,
    total: Arc<AtomicUsize>,
}

impl ConnectionTracker {
    /// Try to register a new connection from `peer`. Returns `None` when
    /// the peer is already at `max_per_peer` connections — the caller
    /// must reject the new connection. On success `total` is incremented
    /// and the connect event (if any) has fired.
    pub(crate) fn acquire(
        counts: &Arc<Mutex<HashMap<iroh::PublicKey, usize>>>,
        peer: iroh::PublicKey,
        peer_id_str: String,
        max_per_peer: usize,
        on_event: Option<ConnectionEventFn>,
        total: Arc<AtomicUsize>,
    ) -> Option<Self> {
        let mut map = counts.lock().unwrap_or_else(|e| e.into_inner());
        let count = map.entry(peer).or_insert(0);
        if *count >= max_per_peer {
            return None;
        }
        let was_zero = *count == 0;
        *count = count.saturating_add(1);
        drop(map);

        total.fetch_add(1, Ordering::Relaxed);

        // Fire connected event on 0 → 1 transition.
        if was_zero {
            if let Some(cb) = &on_event {
                cb(ConnectionEvent {
                    peer_id: peer_id_str.clone(),
                    connected: true,
                });
            }
        }

        Some(ConnectionTracker {
            counts: counts.clone(),
            peer,
            peer_id_str,
            on_event,
            total,
        })
    }
}

impl Drop for ConnectionTracker {
    fn drop(&mut self) {
        self.total.fetch_sub(1, Ordering::Relaxed);

        let mut map = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = map.get_mut(&self.peer) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                map.remove(&self.peer);
                // Fire disconnected event on 1 → 0 transition.
                if let Some(cb) = &self.on_event {
                    cb(ConnectionEvent {
                        peer_id: self.peer_id_str.clone(),
                        connected: false,
                    });
                }
            }
        }
    }
}

// ── RequestTracker ────────────────────────────────────────────────────────────

/// Per-request processing lifecycle exposed through endpoint statistics.
pub(crate) struct RequestTracker {
    counter: Arc<AtomicUsize>,
}

impl RequestTracker {
    /// Own a request slot already incremented by the accept loop.
    pub(crate) fn new(counter: Arc<AtomicUsize>) -> Self {
        RequestTracker { counter }
    }
}

impl Drop for RequestTracker {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

// ── DeliveryTracker ──────────────────────────────────────────────────────────

/// Private graceful-drain lifecycle. A request remains delivery-in-flight until
/// the response stream is transport-acknowledged, stopped, or its connection
/// fails. This deliberately does not affect public active-request statistics.
pub(crate) struct DeliveryTracker {
    in_flight: Arc<AtomicUsize>,
    drain_notify: Arc<tokio::sync::Notify>,
}

impl DeliveryTracker {
    /// Own a delivery slot already incremented by the accept loop.
    pub(crate) fn new(in_flight: Arc<AtomicUsize>, drain_notify: Arc<tokio::sync::Notify>) -> Self {
        DeliveryTracker {
            in_flight,
            drain_notify,
        }
    }
}

impl Drop for DeliveryTracker {
    fn drop(&mut self) {
        if self.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Last in-flight delivery completed — signal drain.
            self.drain_notify.notify_waiters();
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_peer() -> iroh::PublicKey {
        iroh::SecretKey::generate().public()
    }

    #[test]
    fn connection_tracker_increments_and_decrements_total() {
        let total = Arc::new(AtomicUsize::new(0));
        let counts = Arc::new(Mutex::new(HashMap::new()));
        let peer = dummy_peer();
        {
            let _t =
                ConnectionTracker::acquire(&counts, peer, "p".to_string(), 4, None, total.clone())
                    .expect("acquire should succeed under cap");
            assert_eq!(total.load(Ordering::Relaxed), 1);
        }
        assert_eq!(total.load(Ordering::Relaxed), 0);
        assert!(counts.lock().unwrap().is_empty());
    }

    #[test]
    fn connection_tracker_enforces_per_peer_cap() {
        let total = Arc::new(AtomicUsize::new(0));
        let counts = Arc::new(Mutex::new(HashMap::new()));
        let peer = dummy_peer();
        let _a =
            ConnectionTracker::acquire(&counts, peer, "p".into(), 1, None, total.clone()).unwrap();
        let b = ConnectionTracker::acquire(&counts, peer, "p".into(), 1, None, total.clone());
        assert!(b.is_none(), "second acquire over cap must fail");
        assert_eq!(total.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn delivery_tracker_notifies_when_in_flight_reaches_zero() {
        let counter = Arc::new(AtomicUsize::new(1));
        let in_flight = Arc::new(AtomicUsize::new(1));
        let drain = Arc::new(tokio::sync::Notify::new());

        let waiter = drain.clone();
        let waited = tokio::spawn(async move {
            waiter.notified().await;
        });

        // Give the waiter a chance to register.
        tokio::task::yield_now().await;

        let request = RequestTracker::new(counter.clone());
        let delivery = DeliveryTracker::new(in_flight.clone(), drain.clone());

        drop(request);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        assert_eq!(in_flight.load(Ordering::Relaxed), 1);

        drop(delivery);

        tokio::time::timeout(std::time::Duration::from_millis(100), waited)
            .await
            .expect("waiter must be notified")
            .unwrap();

        assert_eq!(in_flight.load(Ordering::Relaxed), 0);
    }
}
