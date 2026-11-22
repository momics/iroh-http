# Connection Pool

iroh-http-core maintains a pool of QUIC connections so that multiple `fetch()` calls to the same peer reuse one connection rather than performing a new QUIC handshake each time.

---

## Design

The pool is a `moka::future::Cache<PoolKey, PooledConn>` with the following properties:

| Property | Value |
|----------|-------|
| Key | `(node_id: String, alpn: &'static [u8])` |
| Value | `PooledConn { conn: iroh::endpoint::Connection }` |
| Concurrency primitive | `try_get_with` (single-flight) |
| Stale detection | `conn.close_reason().is_none()` check before use |
| Eviction | Time-to-idle (configurable via `NodeOptions::pool_idle_timeout_ms`, default 60 s) |
| Max entries | Configurable via `NodeOptions::max_pooled_connections` |

---

## Single-flight via `try_get_with`

When two concurrent `fetch()` calls target the same peer and no connection exists yet, only one QUIC handshake should happen. `moka::future::Cache::try_get_with` provides this guarantee:

```rust
cache.try_get_with(key, async {
    ep.connect(addr, alpn).await.map_err(|e| format!("connect: {e}"))
}).await
```

- If the key is not in cache: the initializer runs once; all concurrent callers for that key wait on the same future.
- If the key is in cache: the cached value is returned immediately.
- If the initializer fails: the error is returned (as `Arc<String>`) to all waiters. The failure is **not** cached — the next call retries.

> Use `try_get_with`, not `get_with`. `get_with` is for infallible initializers; with connection errors, `get_with` would panic or misbehave.

---

## Stale connection handling

A pooled QUIC connection can become stale (remote closed, idle timeout, network change) while sitting in the cache. Before using a pooled connection, iroh-http-core checks:

```rust
if conn.close_reason().is_some() {
    pool.invalidate(&key).await;
    // retry: connect fresh
}
```

`close_reason()` is a non-blocking check on the connection's internal state. If it returns `Some`, the connection is dead and the cache entry is removed. The retry logic then falls through to a fresh `try_get_with`.

---

## Stale connections after network path migration

### What happens

QUIC supports transparent connection migration — when a device changes
networks (e.g., WiFi → cellular), the QUIC connection object stays "open" and
migrates its underlying UDP path. The connection is **never explicitly closed**
during migration; `close_reason()` returns `None` even if the old path has
died and the new path has not yet been confirmed.

This means the pool's stale-detection check (`close_reason().is_some()`) cannot
distinguish a healthy migrating connection from one stuck on a dead path. The
first `fetch()` attempt after the network change may time out before QUIC's own
keepalive detects the dead path and fails the connection.

### Observed behaviour

| Phase | What you see |
|-------|-------------|
| Migration completes before the request | No impact — the new path is active by the time `fetch()` runs |
| Migration in progress during `fetch()` | Higher latency while QUIC retries on the new path; typically resolves in < 1 s |
| Old path dies, new path not yet established | Request times out; pool evicts the connection and retries on the next call |

Callers may bound a replacement dial with per-fetch `connectTimeout`; a live
pooled connection bypasses that deadline. The separate `requestTimeout`
continues to govern stream opening, HTTP handshake/dispatch, and response-head
receipt. In practice, QUIC's keepalive probes often fail a stale connection
before either deadline fires.

### Current mitigation

No proactive path health check is implemented. The pool relies on QUIC's own
keepalive and path validation to detect dead paths. This is acceptable for
most workloads because:

- QUIC path migration is fast on modern networks (< 500 ms typical)
- The pool evicts connections once QUIC reports them as closed
- Retry on the next `fetch()` call re-establishes the connection from scratch

### Recommendations for mobile or high-churn scenarios

If your application runs on mobile devices that frequently switch networks,
consider:

1. **Lower `poolIdleTimeoutMs`** (e.g., 10–30 s instead of 60 s). This
   increases the chance that a post-migration connection is fresh rather than
   cached. Trade-off: more reconnects.

2. **Catch connection timeout errors and retry once.** A single retry after a
   `CONNECT_TIMEOUT` will start a fresh pool attempt. Use `connectTimeout` to
   bound the dial without shortening the response-head budget:

   ```ts
   async function fetchWithRetry(node, peer, url) {
     try {
       return await node.fetch(peer.toURL(url));
     } catch (e) {
       if (e.name === 'NetworkError') return node.fetch(peer.toURL(url));
       throw e;
     }
   }
   ```

3. **Use `node.peerStats()` to monitor RTT.** A sudden spike in `rttMs`
   after a fetch is a signal that migration is in progress. You can poll
   `peerStats` to detect the transition and preemptively close the old session.

### Connection-pool.md reference

The `ALPN` and `(node_id, alpn)` key mean that each distinct protocol type
gets its own pool slot. Network migration affects the QUIC connection object
regardless of ALPN — all pool slots for the same peer may be affected
simultaneously if the path changes.

---

The pool keys on `(node_id, alpn)` — not just `node_id`. This means:

- Regular HTTP requests use ALPN `b"iroh-http/2"` — pooled separately
- Duplex (`raw_connect`) uses ALPN `b"iroh-http/2-duplex"` — its own pool slot
- WebTransport sessions use a session-specific ALPN — their own pool slots

A peer can have multiple live connections in the pool, one per ALPN.

---

## Pool lifecycle

The pool is owned by `IrohEndpoint` (inside `EndpointInner`). Cloning an `IrohEndpoint` is a cheap `Arc` clone — all clones share the same pool.

When `IrohEndpoint::close()` is called, the pool is dropped. moka's cache holds weak references to its values by default — dropping the pool allows connections to be closed by the QUIC layer's normal drop path.

---

## Configuration

| Option | Default | Description |
|--------|---------|-------------|
| `NodeOptions::max_pooled_connections` | 128 | Maximum entries in the cache |
| `NodeOptions::pool_idle_timeout_ms` | 60 000 | Evict connections idle this long (ms) |

These are set when creating the endpoint:

```rust
IrohEndpoint::bind(NodeOptions {
    max_pooled_connections: Some(64),
    pool_idle_timeout_ms: Some(30_000),
    ..Default::default()
}).await?
```
