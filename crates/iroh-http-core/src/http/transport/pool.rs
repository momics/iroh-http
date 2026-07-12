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

use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::PublicKey;

/// A pooled QUIC connection (clone-cheap).
#[derive(Clone)]
pub(crate) struct PooledConnection {
    pub conn: Connection,
    /// Cached base32-encoded remote node ID, computed once per connection.
    pub remote_id_str: String,
}

impl PooledConnection {
    pub fn new(conn: Connection) -> Self {
        let remote_id_str = crate::base32_encode(conn.remote_id().as_bytes());
        Self {
            conn,
            remote_id_str,
        }
    }
}

/// Key for a pooled connection: `(node_id, ALPN bytes)`.
#[derive(Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    node_id: PublicKey,
    alpn: &'static [u8],
}

/// Thread-safe QUIC connection pool backed by moka.
pub(crate) struct ConnectionPool {
    cache: moka::future::Cache<PoolKey, Arc<PooledConnection>>,
    event_tx: Option<tokio::sync::mpsc::Sender<crate::events::TransportEvent>>,
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
        }
    }

    fn emit(&self, event: crate::events::TransportEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(event);
        }
    }

    /// Get an existing live connection, or establish a new one.
    ///
    /// `connect_fn` is called at most once per concurrent batch of callers on
    /// the same `(node_id, ALPN)` pair.  Concurrent callers wait for the
    /// single in-progress connect and receive the same result.
    pub async fn get_or_connect<F, Fut>(
        &self,
        node_id: PublicKey,
        alpn: &'static [u8],
        connect_fn: F,
    ) -> Result<PooledConnection, String>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<Connection, String>> + Send,
    {
        let key = PoolKey { node_id, alpn };

        // Phase 1: check for a live cached connection (no lock held while awaiting).
        if let Some(pooled) = self.cache.get(&key).await {
            if pooled.conn.close_reason().is_none() {
                tracing::debug!(peer = %pooled.remote_id_str, "iroh-http: pool hit");
                self.emit(crate::events::TransportEvent::pool_hit(
                    pooled.remote_id_str.clone(),
                ));
                return Ok((*pooled).clone());
            }
            // Stale — invalidate and fall through to a fresh connect.
            tracing::debug!(peer = %pooled.remote_id_str, "iroh-http: pool stale, reconnecting");
            self.emit(crate::events::TransportEvent::pool_evict(
                pooled.remote_id_str.clone(),
            ));
            self.cache.invalidate(&key).await;
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
                    self.cache.invalidate(&key).await;
                    return Err("pooled connection closed immediately after connect".to_string());
                }
                Ok((*pooled).clone())
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

    /// Remove and close the cached connection for `(node_id, alpn)`, if any.
    ///
    /// Used after request-level transport failures where QUIC may not have
    /// observed a close yet, but reusing the connection would keep future
    /// requests stuck on the same dead path.
    pub(crate) async fn evict_and_close(
        &self,
        node_id: PublicKey,
        alpn: &'static [u8],
        reason: &'static [u8],
    ) {
        let key = PoolKey { node_id, alpn };
        if let Some(pooled) = self.cache.get(&key).await {
            tracing::debug!(peer = %pooled.remote_id_str, "iroh-http: pool evict and close");
            self.emit(crate::events::TransportEvent::pool_evict(
                pooled.remote_id_str.clone(),
            ));
            pooled.conn.close(0u32.into(), reason);
        }
        self.cache.invalidate(&key).await;
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
}
