//! QUIC connection pool backed by `moka` async cache.
//!
//! `moka::future::Cache::try_get_with` provides built-in single-flight semantics:
//! concurrent callers for the same `(node_id, ALPN)` key wait on the same
//! in-progress connect future rather than each starting a new handshake.
//! On failure the error is returned to all concurrent waiters (wrapped in Arc).
//!
//! Liveness checking: before returning a cached connection we call
//! `close_reason()`.  Closed connections are invalidated and one retry is
//! attempted so the caller always gets a live connection on the first call.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::PublicKey;

/// Process-wide monotonic dial counter. Each `PooledConnection` gets a unique,
/// never-reused id from this. See [`PooledConnection::dial_seq`].
static DIAL_SEQ: AtomicU64 = AtomicU64::new(0);

/// A pooled QUIC connection (clone-cheap).
#[derive(Clone)]
pub(crate) struct PooledConnection {
    pub conn: Connection,
    /// Cached base32-encoded remote node ID, computed once per connection.
    pub remote_id_str: String,
    /// Monotonic per-dial sequence number, unique for the lifetime of the
    /// process (L1). Eviction is scoped to this rather than
    /// [`Connection::stable_id`]: `stable_id` is a heap pointer (the address of
    /// the connection's inner state), unique only among *live* connections, so
    /// after a connection is dropped the allocator may reuse that address for a
    /// fresh connection cached under the same key — an ABA hazard that could
    /// make eviction false-match and close a healthy replacement (the exact
    /// thing P2 prevents). A monotonic counter never repeats, so
    /// [`ConnectionPool::evict_and_close`] can identify the exact connection
    /// instance that failed with certainty.
    pub dial_seq: u64,
}

impl PooledConnection {
    pub fn new(conn: Connection) -> Self {
        let remote_id_str = crate::base32_encode(conn.remote_id().as_bytes());
        let dial_seq = DIAL_SEQ.fetch_add(1, Ordering::Relaxed);
        Self {
            conn,
            remote_id_str,
            dial_seq,
        }
    }
}

/// Key for a pooled connection: `(node_id, ALPN bytes)`.
#[derive(Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    node_id: PublicKey,
    alpn: &'static [u8],
}

#[cfg(test)]
#[derive(Default)]
struct StaleObservation {
    reached: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

/// Thread-safe QUIC connection pool backed by moka.
pub(crate) struct ConnectionPool {
    cache: moka::future::Cache<PoolKey, Arc<PooledConnection>>,
    event_tx: Option<tokio::sync::mpsc::Sender<crate::events::TransportEvent>>,
    /// Deterministic concurrency seam for the stale-replacement regression.
    #[cfg(test)]
    stale_observation: std::sync::Mutex<Option<Arc<StaleObservation>>>,
}

impl ConnectionPool {
    /// Create a new pool.
    ///
    /// - `max_idle`: max cached connections (None = 512).
    /// - `idle_timeout`: connections idle longer than this are evicted.
    /// - `event_tx`: optional sender for transport-level events.
    pub fn new(
        max_idle: Option<usize>,
        idle_timeout: Option<std::time::Duration>,
        event_tx: Option<tokio::sync::mpsc::Sender<crate::events::TransportEvent>>,
    ) -> Self {
        let cap = max_idle.unwrap_or(512) as u64;
        let mut builder = moka::future::Cache::builder().max_capacity(cap);
        if let Some(tti) = idle_timeout {
            builder = builder.time_to_idle(tti);
        }
        Self {
            cache: builder.build(),
            event_tx,
            #[cfg(test)]
            stale_observation: std::sync::Mutex::new(None),
        }
    }

    fn emit(&self, event: crate::events::TransportEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(event);
        }
    }

    #[cfg(test)]
    async fn observe_stale_for_test(&self) {
        let observation = self
            .stale_observation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(observation) = observation {
            observation.reached.notify_one();
            observation.resume.notified().await;
        }
    }

    /// Get an existing live connection, or establish a new one.
    ///
    /// `connect_fn` is called at most once per concurrent batch of callers on
    /// the same `(node_id, ALPN)` pair.  Concurrent callers wait for the
    /// single in-progress connect and receive the same result.
    ///
    /// The returned `bool` is `true` when the connection was **reused** from
    /// the pool (a live cached hit) and `false` when it was freshly dialed.
    /// Callers use this to decide whether a subsequent transport failure is
    /// worth a transparent retry: a reused connection may have been closed by
    /// the peer (e.g. a serve-loop replace, #336) between pooling and use, so
    /// resending an idempotent request on a fresh connection is safe and
    /// expected (RFC 9110 §9.2.2). A freshly-dialed connection that fails is
    /// not retried — redialing again would not help.
    pub async fn get_or_connect<F, Fut>(
        &self,
        node_id: PublicKey,
        alpn: &'static [u8],
        connect_fn: F,
    ) -> Result<(PooledConnection, bool), String>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<Connection, String>> + Send,
    {
        let key = PoolKey { node_id, alpn };

        // Phase 1: check for a live cached connection (no lock held while
        // awaiting). A stale observation is removed by identity. If another
        // caller already replaced it, loop back and use that replacement
        // instead of invalidating it or starting a redundant dial.
        loop {
            let Some(pooled) = self.cache.get(&key).await else {
                break;
            };
            if pooled.conn.close_reason().is_none() {
                tracing::debug!(peer = %pooled.remote_id_str, "iroh-http: pool hit");
                self.emit(crate::events::TransportEvent::pool_hit(
                    pooled.remote_id_str.clone(),
                ));
                return Ok(((*pooled).clone(), true));
            }
            // Stale — remove this exact identity and fall through to connect.
            tracing::debug!(peer = %pooled.remote_id_str, "iroh-http: pool stale, reconnecting");
            #[cfg(test)]
            self.observe_stale_for_test().await;
            if let Some(removed) = self.compare_and_remove(key.clone(), pooled.dial_seq).await {
                self.emit(crate::events::TransportEvent::pool_evict(
                    removed.remote_id_str.clone(),
                ));
                break;
            }
        }

        // Phase 2: single-flight connect via try_get_with.
        // If multiple callers race, only one invokes connect_fn; the rest wait.
        let peer_id_str = crate::base32_encode(key.node_id.as_bytes());
        tracing::debug!(peer = %peer_id_str, "iroh-http: pool miss, connecting");
        self.emit(crate::events::TransportEvent::pool_miss(peer_id_str));
        let result = self
            .cache
            .try_get_with(key.clone(), async move {
                connect_fn()
                    .await
                    .map(|conn| Arc::new(PooledConnection::new(conn)))
            })
            .await;

        match result {
            Err(e) => {
                // try_get_with wraps the error in Arc; clone the message out.
                Err((*e).clone())
            }
            Ok(pooled) => {
                // Guard against a connection that was closed immediately.
                if pooled.conn.close_reason().is_some() {
                    #[cfg(test)]
                    self.observe_stale_for_test().await;

                    // Scope cleanup to the exact result we observed. A
                    // concurrent caller may already have removed it and
                    // installed a healthy connection under the same key.
                    if self
                        .compare_and_remove(key.clone(), pooled.dial_seq)
                        .await
                        .is_none()
                    {
                        if let Some(replacement) = self.cache.get(&key).await {
                            if replacement.conn.close_reason().is_none() {
                                tracing::debug!(
                                    peer = %replacement.remote_id_str,
                                    "iroh-http: pool hit after immediate-close replacement",
                                );
                                self.emit(crate::events::TransportEvent::pool_hit(
                                    replacement.remote_id_str.clone(),
                                ));
                                return Ok(((*replacement).clone(), true));
                            }
                        }
                    }
                    return Err("pooled connection closed immediately after connect".to_string());
                }
                Ok(((*pooled).clone(), false))
            }
        }
    }

    /// Return an existing live connection for `(node_id, alpn)` without
    /// establishing a new one.  Returns `None` if the connection is not
    /// cached or has been closed.
    pub async fn get_existing(
        &self,
        node_id: PublicKey,
        alpn: &'static [u8],
    ) -> Option<PooledConnection> {
        let key = PoolKey { node_id, alpn };
        let pooled = self.cache.get(&key).await?;
        if pooled.conn.close_reason().is_none() {
            Some((*pooled).clone())
        } else {
            None
        }
    }

    /// Remove and close the cached connection for `(node_id, alpn)` **only if
    /// it is the same connection that failed**, identified by
    /// `expected_dial_seq`.
    ///
    /// Used after request-level transport failures where QUIC may not have
    /// observed a close yet, but reusing the connection would keep future
    /// requests stuck on the same dead path.
    ///
    /// P2: eviction is connection-scoped. A concurrent request may have already
    /// evicted the dead connection and dialed a fresh replacement (on the new
    /// serve loop) under the same key; closing *that* healthy connection would
    /// make the concurrent request — or the next reuse — fail spuriously. So we
    /// close+invalidate only when the currently-cached connection's
    /// [`PooledConnection::dial_seq`] matches the one that failed. `None` means
    /// the caller captured no specific connection (e.g. the connect itself
    /// failed, or a timeout fired before any connection was obtained); in that
    /// case we must not touch a possibly-healthy cached connection, so it is a
    /// no-op.
    ///
    /// L1: the match uses the monotonic `dial_seq` rather than
    /// `Connection::stable_id` so the guarantee is absolute rather than
    /// ABA-probabilistic (a dropped connection's `stable_id` address can be
    /// reused by a fresh connection; `dial_seq` never repeats).
    ///
    /// F16: the compare-and-remove is a single atomic Moka `and_compute_with`
    /// operation (see [`compare_and_remove`](Self::compare_and_remove)) rather
    /// than a `get` followed by a separate `invalidate`. That closes a TOCTOU
    /// window in which a concurrent task could replace the connection between
    /// the two calls and have the delayed invalidate remove the fresh,
    /// never-compared replacement.
    pub(crate) async fn evict_and_close(
        &self,
        node_id: PublicKey,
        alpn: &'static [u8],
        reason: &'static [u8],
        expected_dial_seq: Option<u64>,
    ) {
        let Some(seq) = expected_dial_seq else {
            // No identity captured (connect failed / timed out before dialing)
            // — must not touch a possibly-healthy cached connection.
            return;
        };
        let key = PoolKey { node_id, alpn };
        if let Some(pooled) = self.compare_and_remove(key, seq).await {
            tracing::debug!(peer = %pooled.remote_id_str, "iroh-http: pool evict and close");
            self.emit(crate::events::TransportEvent::pool_evict(
                pooled.remote_id_str.clone(),
            ));
            pooled.conn.close(0u32.into(), reason);
        }
    }

    /// Remove the cached connection for `(node_id, alpn)` **without closing
    /// it**, only if it is the same connection identified by
    /// `expected_dial_seq`.
    ///
    /// F6: used when a request must stop reusing a pooled connection (for
    /// example after a local response-head timeout) but the QUIC connection may
    /// still be multiplexing other in-flight requests on independent
    /// bi-streams. Closing it — as [`evict_and_close`](Self::evict_and_close)
    /// does — resets *every* stream on the shared connection, cascading one
    /// request's timeout into unrelated (and possibly non-idempotent) request
    /// failures. Invalidating only the pool entry makes future requests redial
    /// while leaving live connection clones and their streams untouched; the
    /// timed-out request's own stream is reset when its dropped future releases
    /// the send/recv handles.
    ///
    /// The `expected_dial_seq` scoping matches
    /// [`evict_and_close`](Self::evict_and_close): a mismatched id (a fresher
    /// replacement) or `None` (no captured identity) is a no-op, and the
    /// compare-and-remove is atomic (F16).
    pub(crate) async fn evict_only(
        &self,
        node_id: PublicKey,
        alpn: &'static [u8],
        expected_dial_seq: Option<u64>,
    ) {
        let Some(seq) = expected_dial_seq else {
            return;
        };
        let key = PoolKey { node_id, alpn };
        if let Some(pooled) = self.compare_and_remove(key, seq).await {
            tracing::debug!(
                peer = %pooled.remote_id_str,
                "iroh-http: pool evict-only (invalidate without close)",
            );
            self.emit(crate::events::TransportEvent::pool_evict(
                pooled.remote_id_str.clone(),
            ));
        }
    }

    /// Atomically remove the cached connection for `key` **iff** its
    /// `dial_seq` matches `expected_dial_seq`, returning the removed connection
    /// (or `None` if the key was absent or held a different connection).
    ///
    /// F16: the compare and the remove happen inside a single Moka per-key
    /// `and_compute_with` closure, which runs under the entry lock, so no
    /// concurrent task can slip a fresh replacement in between the two — the
    /// exact TOCTOU that a `get` + later `invalidate` pair allows.
    async fn compare_and_remove(
        &self,
        key: PoolKey,
        expected_dial_seq: u64,
    ) -> Option<Arc<PooledConnection>> {
        use moka::ops::compute::{CompResult, Op};
        let result = self
            .cache
            .entry(key)
            .and_compute_with(|maybe_entry| {
                let op = match maybe_entry {
                    Some(ref entry) if entry.value().dial_seq == expected_dial_seq => Op::Remove,
                    _ => Op::Nop,
                };
                std::future::ready(op)
            })
            .await;
        match result {
            CompResult::Removed(entry) => Some(entry.into_value()),
            _ => None,
        }
    }

    /// Number of live entries (for testing).
    #[cfg(test)]
    pub async fn len(&self) -> usize {
        self.cache.run_pending_tasks().await;
        self.cache.entry_count() as usize
    }

    /// Instantaneous entry count (for stats/observability).  Does not flush
    /// pending evictions — the value may be slightly higher than the true
    /// number of live connections.
    pub(crate) fn entry_count_approx(&self) -> u64 {
        self.cache.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pool_starts_empty() {
        let pool = ConnectionPool::new(None, None, None);
        assert_eq!(pool.len().await, 0);
    }

    /// A task that observed stale connection A must not unconditionally evict
    /// live replacement B if another task installed B before stale cleanup ran.
    /// Besides destroying healthy multiplexed streams, that race forces an
    /// unnecessary second dial for the same peer.
    #[tokio::test]
    async fn stale_cleanup_preserves_a_concurrent_replacement_and_avoids_redial() {
        let opts = || crate::NodeOptions {
            networking: crate::NetworkingOptions {
                disabled: true,
                bind_addrs: vec!["127.0.0.1:0".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let server = crate::IrohEndpoint::bind(opts()).await.unwrap();
        let client = crate::IrohEndpoint::bind(opts()).await.unwrap();
        let connect_addr = server.raw().addr();
        let node_id = connect_addr.id;

        // Accept and retain every raw connection so replacement dials remain
        // live for the duration of the test.
        let server_raw = server.raw().clone();
        let server_guard = tokio::spawn(async move {
            let mut connections = Vec::new();
            while let Some(incoming) = server_raw.accept().await {
                if let Ok(connection) = incoming.await {
                    connections.push(connection);
                }
            }
        });

        let initial_raw = client.raw().clone();
        let initial_addr = connect_addr.clone();
        let (stale, _) = client
            .pool()
            .get_or_connect(node_id, crate::ALPN, || async move {
                initial_raw
                    .connect(initial_addr, crate::ALPN)
                    .await
                    .map_err(|error| error.to_string())
            })
            .await
            .expect("initial connection should succeed");
        stale.conn.close(0u32.into(), b"test stale connection");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while stale.conn.close_reason().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("local close should mark the pooled connection stale");

        // Pause one public get_or_connect call after it has captured A but
        // before it removes A. This deterministically models the scheduler
        // interleaving that exposed the unscoped invalidate.
        let observation = Arc::new(StaleObservation::default());
        *client
            .pool()
            .stale_observation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(observation.clone());

        let replacement_dials = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let task_client = client.clone();
        let task_addr = connect_addr.clone();
        let task_dials = replacement_dials.clone();
        let stale_cleanup = tokio::spawn(async move {
            let task_raw = task_client.raw().clone();
            task_client
                .pool()
                .get_or_connect(node_id, crate::ALPN, move || {
                    task_dials.fetch_add(1, Ordering::SeqCst);
                    async move {
                        task_raw
                            .connect(task_addr, crate::ALPN)
                            .await
                            .map_err(|error| error.to_string())
                    }
                })
                .await
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            observation.reached.notified(),
        )
        .await
        .expect("stale cleanup should reach the deterministic pause");

        // Another task wins cleanup of A and performs the one legitimate
        // replacement dial, installing live connection B under the same key.
        client
            .pool()
            .compare_and_remove(
                PoolKey {
                    node_id,
                    alpn: crate::ALPN,
                },
                stale.dial_seq,
            )
            .await
            .expect("the stale connection should still occupy the cache");
        replacement_dials.fetch_add(1, Ordering::SeqCst);
        let fresh_conn = client
            .raw()
            .connect(connect_addr, crate::ALPN)
            .await
            .expect("replacement connection should succeed");
        let fresh = Arc::new(PooledConnection::new(fresh_conn));
        let fresh_seq = fresh.dial_seq;
        client
            .pool()
            .cache
            .insert(
                PoolKey {
                    node_id,
                    alpn: crate::ALPN,
                },
                fresh,
            )
            .await;

        *client
            .pool()
            .stale_observation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        observation.resume.notify_one();

        let (observed, reused) =
            tokio::time::timeout(std::time::Duration::from_secs(5), stale_cleanup)
                .await
                .expect("stale cleanup should finish")
                .expect("stale cleanup task should not panic")
                .expect("a live replacement is already available");
        assert_eq!(
            observed.dial_seq, fresh_seq,
            "stale cleanup evicted the identity-mismatched replacement",
        );
        assert!(reused, "the concurrent replacement should be a pool hit");
        assert_eq!(
            replacement_dials.load(Ordering::SeqCst),
            1,
            "stale cleanup performed a second dial after replacement was installed",
        );

        server_guard.abort();
    }

    /// The post-connect liveness guard has the same ownership rule as a stale
    /// cache hit: cleanup for closed connection A must not invalidate live B
    /// when another caller replaced the entry before cleanup acquired the key.
    #[tokio::test]
    async fn immediately_closed_cleanup_returns_a_concurrent_live_replacement() {
        let opts = || crate::NodeOptions {
            networking: crate::NetworkingOptions {
                disabled: true,
                bind_addrs: vec!["127.0.0.1:0".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let server = crate::IrohEndpoint::bind(opts()).await.unwrap();
        let client = crate::IrohEndpoint::bind(opts()).await.unwrap();
        let connect_addr = server.raw().addr();
        let node_id = connect_addr.id;

        let server_raw = server.raw().clone();
        let server_guard = tokio::spawn(async move {
            let mut connections = Vec::new();
            while let Some(incoming) = server_raw.accept().await {
                if let Ok(connection) = incoming.await {
                    connections.push(connection);
                }
            }
        });

        let observation = Arc::new(StaleObservation::default());
        *client
            .pool()
            .stale_observation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(observation.clone());

        // The initializer deliberately returns an already-closed connection,
        // then get_or_connect pauses in its post-connect liveness cleanup.
        let attempt_client = client.clone();
        let attempt_addr = connect_addr.clone();
        let immediately_closed = tokio::spawn(async move {
            let attempt_raw = attempt_client.raw().clone();
            attempt_client
                .pool()
                .get_or_connect(node_id, crate::ALPN, move || async move {
                    let connection = attempt_raw
                        .connect(attempt_addr, crate::ALPN)
                        .await
                        .map_err(|error| error.to_string())?;
                    connection.close(0u32.into(), b"test immediate close");
                    while connection.close_reason().is_none() {
                        tokio::task::yield_now().await;
                    }
                    Ok(connection)
                })
                .await
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            observation.reached.notified(),
        )
        .await
        .expect("post-connect cleanup should reach the deterministic pause");

        let key = PoolKey {
            node_id,
            alpn: crate::ALPN,
        };
        let closed = client
            .pool()
            .cache
            .get(&key)
            .await
            .expect("the immediately-closed result should be cached during cleanup");
        client
            .pool()
            .compare_and_remove(key.clone(), closed.dial_seq)
            .await
            .expect("the closed connection should still occupy the cache");

        let fresh_conn = client
            .raw()
            .connect(connect_addr, crate::ALPN)
            .await
            .expect("concurrent replacement connection should succeed");
        let fresh = Arc::new(PooledConnection::new(fresh_conn));
        let fresh_seq = fresh.dial_seq;
        client.pool().cache.insert(key.clone(), fresh).await;

        *client
            .pool()
            .stale_observation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        observation.resume.notify_one();

        let (observed, reused) =
            tokio::time::timeout(std::time::Duration::from_secs(5), immediately_closed)
                .await
                .expect("post-connect cleanup should finish")
                .expect("post-connect cleanup task should not panic")
                .expect("the concurrent replacement is live and should be consumed");
        assert_eq!(
            observed.dial_seq, fresh_seq,
            "post-connect cleanup invalidated the identity-mismatched replacement",
        );
        assert!(
            reused,
            "the concurrent replacement should be returned as a hit"
        );
        assert_eq!(
            client
                .pool()
                .cache
                .get(&key)
                .await
                .expect("live replacement must remain cached")
                .dial_seq,
            fresh_seq,
        );

        server_guard.abort();
    }

    /// Bind a server and a client, drive one raw QUIC accept on the server so
    /// the client can establish and pool a real connection. Returns
    /// `(client, node_id, pooled, _server_guard)`. The returned join handle
    /// keeps the server-side connection alive for the test's duration; drop it
    /// to let the connection close.
    async fn pooled_connection() -> (
        crate::IrohEndpoint,
        PublicKey,
        PooledConnection,
        tokio::task::JoinHandle<()>,
    ) {
        let opts = || crate::NodeOptions {
            networking: crate::NetworkingOptions {
                disabled: true,
                bind_addrs: vec!["127.0.0.1:0".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let server = crate::IrohEndpoint::bind(opts()).await.unwrap();
        let client = crate::IrohEndpoint::bind(opts()).await.unwrap();

        // Accept one raw connection on the server and hold it open so the
        // client-side pooled connection stays live. No serve/ffi layer needed —
        // this file is under `mod http`, which must not depend on `mod ffi`.
        let server_raw = server.raw().clone();
        let server_guard = tokio::spawn(async move {
            if let Some(incoming) = server_raw.accept().await {
                if let Ok(conn) = incoming.await {
                    // Hold the connection until the test drops this task.
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    drop(conn);
                }
            }
        });

        let connect_addr = server.raw().addr();
        let node_id = connect_addr.id;
        let ep_raw = client.raw().clone();
        let (pooled, _reused) = client
            .pool()
            .get_or_connect(node_id, crate::ALPN, || async move {
                ep_raw
                    .connect(connect_addr, crate::ALPN)
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
            .expect("client should connect to the server");

        (client, node_id, pooled, server_guard)
    }

    /// P2/L1: `evict_and_close` is connection-scoped — it must close+invalidate
    /// only when the currently-cached connection is the one that failed
    /// (matching `dial_seq`). A mismatched id (a fresher replacement dialed by
    /// a concurrent request) or `None` (no captured identity) must leave the
    /// cached connection untouched. Otherwise one request's cleanup destroys
    /// another request's healthy connection.
    #[tokio::test]
    async fn evict_and_close_only_closes_the_matching_connection() {
        let (client, node_id, pooled, _server_guard) = pooled_connection().await;
        let seq = pooled.dial_seq;

        // Mismatched dial_seq → must be a no-op (this is a fresher replacement).
        client
            .pool()
            .evict_and_close(node_id, crate::ALPN, b"test", Some(seq.wrapping_add(1)))
            .await;
        assert!(
            client
                .pool()
                .get_existing(node_id, crate::ALPN)
                .await
                .is_some(),
            "evict with a mismatched dial_seq removed a healthy connection (P2)",
        );
        assert!(
            pooled.conn.close_reason().is_none(),
            "evict with a mismatched dial_seq closed a healthy connection (P2)",
        );

        // No captured identity → must also be a no-op.
        client
            .pool()
            .evict_and_close(node_id, crate::ALPN, b"test", None)
            .await;
        assert!(
            client
                .pool()
                .get_existing(node_id, crate::ALPN)
                .await
                .is_some(),
            "evict with None identity removed a healthy connection (P2)",
        );
        assert!(pooled.conn.close_reason().is_none());

        // Matching dial_seq → must close + invalidate.
        client
            .pool()
            .evict_and_close(node_id, crate::ALPN, b"test", Some(seq))
            .await;
        assert!(
            client
                .pool()
                .get_existing(node_id, crate::ALPN)
                .await
                .is_none(),
            "evict with the matching dial_seq did not remove the failed connection",
        );
    }

    /// F6: `evict_only` must remove the pool entry (so future requests redial)
    /// while leaving the underlying QUIC connection open — otherwise a local
    /// timeout on one request would close the shared connection and reset every
    /// other multiplexed request's stream.
    #[tokio::test]
    async fn evict_only_removes_entry_without_closing_connection() {
        let (client, node_id, pooled, _server_guard) = pooled_connection().await;
        let seq = pooled.dial_seq;

        // Mismatched dial_seq / None → no-op, entry stays.
        client
            .pool()
            .evict_only(node_id, crate::ALPN, Some(seq.wrapping_add(1)))
            .await;
        client.pool().evict_only(node_id, crate::ALPN, None).await;
        assert!(
            client
                .pool()
                .get_existing(node_id, crate::ALPN)
                .await
                .is_some(),
            "evict_only with a non-matching id removed a healthy connection",
        );

        // Matching dial_seq → entry removed, but the connection stays OPEN so
        // any concurrent stream on it survives.
        client
            .pool()
            .evict_only(node_id, crate::ALPN, Some(seq))
            .await;
        assert!(
            client
                .pool()
                .get_existing(node_id, crate::ALPN)
                .await
                .is_none(),
            "evict_only did not remove the pool entry",
        );
        assert!(
            pooled.conn.close_reason().is_none(),
            "evict_only closed the shared connection (F6 cascade); it must only invalidate",
        );
    }
}
