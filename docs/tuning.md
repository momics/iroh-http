# Performance Tuning

`NodeOptions` and `ServeOptions` expose ~21 tunables. Most workloads work fine
with the defaults — tune only when you have a specific bottleneck. Profile or
measure before adjusting.

---

## Deployment scenarios

### Chat / real-time messaging (low latency, few peers)

Goal: minimise end-to-end latency on small payloads; keep connections alive.

```ts
const node = await createNode({
  connections: {
    idleTimeoutMs: 120_000,        // 2 min — QUIC idle timeout
    poolIdleTimeoutMs: 60_000,     // 1 min — keep pooled connections warm
    maxPooled: 16,                 // small fleet; generous pool
  },
  internals: {
    channelCapacity: 64,           // larger queue for burst messages
  },
});

node.serve({
  requestTimeout: 10_000,         // 10 s — fail fast on stale connections
  drainTimeout: 5_000,            // 5 s — fail fast when reader stalls
}, handler);
```

**Key trade-offs:**
- `idleTimeoutMs` high → long QUIC idle connections stay alive; saves reconnect latency
- `requestTimeout` low → fast failure detection; surface network problems quickly
- `drainTimeout` low → readers that stall more than 5 s get dropped; prevents head-of-line blocking

---

### File transfer (high throughput, large bodies)

Goal: maximise throughput on large streaming bodies; tolerate slow readers.

```ts
const node = await createNode({
  connections: {
    idleTimeoutMs: 300_000,        // 5 min — transfers take time
  },
  compression: { level: 1, minBodyBytes: 4096 },   // fast compression only
  internals: {
    channelCapacity: 128,          // deep queue prevents producer stalls
    maxChunkSizeBytes: 256_000,    // 256 KB chunks — fewer round-trips
    handleTtl: 600_000,            // 10 min — handles must outlive the transfer
  },
});

node.serve({
  requestTimeout: 600_000,        // 10 min — large files take time
  drainTimeout: 60_000,           // 60 s — tolerate slow readers
}, handler);
```

**Key trade-offs:**
- `maxChunkSizeBytes` higher → fewer FFI round-trips per MB; increases per-chunk latency (trade throughput for latency)
- `channelCapacity` higher → more memory per stream (capacity × maxChunkSizeBytes bytes buffered in worst case)
- `handleTtl` ≥ `requestTimeout` — always set these together for long transfers

---

### IoT hub (many peers, low traffic per peer)

Goal: support many idle connections; conserve memory; evict stale connections quickly.

```ts
const node = await createNode({
  connections: {
    maxPooled: 512,                // many devices
    poolIdleTimeoutMs: 30_000,     // 30 s — evict idle connections quickly
    idleTimeoutMs: 60_000,         // 1 min — QUIC layer idle timeout
  },
  internals: {
    channelCapacity: 8,            // small queue saves memory × (n devices)
    maxChunkSizeBytes: 16_000,     // 16 KB — small payloads from sensors
    handleTtl: 60_000,             // 1 min — handles expire with idle connections
  },
});

node.serve({
  maxConnectionsPerPeer: 2,       // IoT devices send few concurrent requests
}, handler);
```

**Key trade-offs:**
- `poolIdleTimeoutMs` low → more reconnects; each reconnect is one QUIC handshake (~100–200 ms on LAN)
- `channelCapacity` × `maxChunkSizeBytes` ≈ peak memory per stream; for 512 devices ≈ 512 × 8 × 16 KB = 64 MB worst case

---

### Mobile application (intermittent connectivity, path changes)

Goal: recover quickly from network changes; minimise stale-connection delays.

```ts
const node = await createNode({
  connections: {
    idleTimeoutMs: 30_000,         // 30 s — QUIC idles out quickly
    poolIdleTimeoutMs: 15_000,     // 15 s — evict pooled connections often
  },
});

node.serve({
  requestTimeout: 20_000,         // 20 s — fail fast; mobile app should retry
  drainTimeout: 10_000,           // 10 s — don't wait long for stalled reads
}, handler);
```

Then, add a single-retry wrapper (see [troubleshooting.md](troubleshooting.md#networkerror-on-fetch--timeout)):

```ts
async function fetchWithRetry(node, peer, url, init) {
  try {
    return await node.fetch(peer.toURL(url), init);
  } catch (e) {
    if (e.name === 'NetworkError') return node.fetch(peer.toURL(url), init);
    throw e;
  }
}
```

**Note on connection migration:** QUIC migrates transparently but the pool
cannot distinguish a migrating connection from a dead one. Lower
`poolIdleTimeoutMs` bounds the maximum stale-connection delay.

---

## Option reference

| Option | Default | Tune when |
|--------|---------|-----------|
| `connections.idleTimeoutMs` | 30 000 ms | Peers are far away / high-latency relay; increase to keep connections warm |
| `connections.poolIdleTimeoutMs` | 60 000 ms | Mobile / frequent network changes (decrease); LAN (increase) |
| `connections.maxPooled` | 512 | Many peers (increase) or memory-constrained (decrease); see [Connection pool sizing](#connection-pool-sizing) |
| `limits.maxHeaderBytes` | 65 536 | Prevent header-flood attacks or accept large cookies (adjust) |
| `compression.level` | 3 | Slow CPUs (reduce to 1); compression ratio priority (increase to 5) |
| `compression.minBodyBytes` | (server default) | Skip compression for small payloads (increase threshold) |
| `internals.channelCapacity` | 32 | High-throughput streaming (increase); memory-constrained (decrease) |
| `internals.maxChunkSizeBytes` | 65 536 (64 KB) | Fewer FFI calls (increase); lower per-call latency (decrease) |
| `internals.handleTtl` | 300 000 ms (5 min) | Transfers > 5 min (increase proportionally); short-lived nodes (decrease) |

**`ServeOptions` (passed at `serve()` time):**

| Option | Default | Tune when |
|--------|---------|-----------|
| `requestTimeout` | 60 000 ms | Transfers large files (increase) or want fast failure detection (decrease) |
| `maxConcurrency` | 1 024 | Serving many concurrent requests (increase); or throttling (decrease) |
| `maxConnectionsPerPeer` | 8 | IoT with few connections/peer (decrease); busy API clients (increase) |
| `maxRequestBodyBytes` | 16 MiB | Prevent large upload attacks (decrease to tighten; raise for large uploads) |
| `drainTimeout` | 5 000 ms | Slow readers expected (increase); tight latency SLA (decrease) |
| `maxServeErrors` | 5 | Noisy channels (increase); fail-fast desired (decrease) |

---

## Memory sizing

Peak memory per active stream (worst case):

```
channelCapacity × maxChunkSizeBytes = max buffered bytes per stream
```

Default: `32 × 65 536 = 2 MB` per stream. For 100 concurrent streams:
~200 MB. For IoT hub tuning above (8 × 16 KB = 128 KB per stream, 512 devices):
~64 MB.

Reduce `channelCapacity` or `maxChunkSizeBytes` to trade throughput for memory.

---

## Compression guidance

Compression is enabled per-endpoint via `NodeOptions.compression`:

```ts
// Transparent (use defaults):
compression: true

// Tuned:
compression: { level: 1, minBodyBytes: 1024 }
```

- `level: 1` — fastest; ~60% CPU of level 3; ~10% less compression
- `level: 3` — default balance (zstd default)
- `level: 5+` — better ratio; noticeable CPU for large bodies

Set `minBodyBytes` to skip compression for small payloads (networking overhead
exceeds compression benefit below ~512 bytes).

Compression is **zstd only**. Both endpoints must be iroh-http — there is no
cross-vendor compression negotiation.

---

## Connection pool sizing

`maxPooledConnections` caps the number of idle QUIC connections kept in the
pool. The default is **512**.

```ts
const node = await createNode({
  maxPooledConnections: 512,  // default — fits most workloads
});
```

Each pooled entry is one idle QUIC connection. Memory cost is low (~10–30 KB
per entry in kernel buffers); the main cost is OS file descriptors and UDP
socket slots.

### How to size the pool

| Topology | Recommended value | Reasoning |
|---|---|---|
| Desktop / mobile app (< 20 peers) | 32–64 | Saves FDs; reconnect overhead is negligible |
| Developer tooling / small mesh | 128–256 | Default overkill; use 128 to be safe |
| CDN edge / job dispatcher (1 000+ peers) | 1 024–4 096 | Avoids per-request QUIC handshakes (~100–200 ms on WAN) |
| IoT hub (many low-traffic devices) | 512–2 048 | Devices connect infrequently; pool keeps paths warm |

**Rule of thumb:** set `maxPooledConnections` ≥ your 95th-percentile active
peer count. Peers beyond the cap will reconnect on next fetch (one QUIC
handshake ≈ 1–2 RTTs).

### Relationship with `poolIdleTimeoutMs`

The pool evicts entries that have been idle longer than `poolIdleTimeoutMs`
(default: 60 s). For high-fanout nodes, pair a large `maxPooledConnections`
with a shorter `poolIdleTimeoutMs` to recycle infrequently used peers quickly:

```ts
const node = await createNode({
  maxPooledConnections: 2_048,  // large fleet
  poolIdleTimeoutMs: 20_000,    // evict after 20 s of no activity
});
```

### Finding the full option path

`createNode({ maxPooledConnections })` → `NodeOptions.pool.max_connections`
→ `ConnectionPool::new(max_idle, …)` in `iroh-http-core`. The JS option is a
direct alias for the Rust `PoolOptions.max_connections` field.
