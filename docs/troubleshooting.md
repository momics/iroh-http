# Troubleshooting

Common problems and how to resolve them. Error code meanings are in
[specification.md § Error Contract](specification.md#error-contract).

---

## Connection / fetch errors

### `NetworkError` on `fetch()` — "connection refused" or "connection failed"

**Likely causes:**

| Symptom | Cause | Fix |
|---------|-------|-----|
| Peer never receives the connection attempt | Wrong node ID | Double-check the peer's public key (`node.addr()`) |
| Peer is not serving | `node.serve()` was not called | Call `serve()` before accepting connections |
| Relay unreachable | No internet / relay down | Check relay reachability; try `relayMode: "disabled"` for LAN-only |
| Both peers behind symmetric NAT | Hole-punching fails | Use relay (default); symmetric NAT cannot be hole-punched |

**Diagnostic steps:**

```ts
// 1. Check your own address
const addr = await node.addr();
console.log('my addresses:', addr.addrs);

// 2. Check home relay
const relay = await node.homeRelay();
console.log('home relay:', relay);   // null = no relay connection

// 3. Check peer stats after a successful connection
const stats = await node.peerStats(peerId);
console.log('peer stats:', stats);
```

---

### `NetworkError` on `fetch()` — "timeout"

**Likely causes:**

- Peer is reachable but not responding (overloaded handler, blocked event loop)
- NAT keepalive expired; QUIC path timed out
- Network path changed (WiFi → cellular); see [stale connections](internals/connection-pool.md#stale-connections-after-network-path-migration)

**Fixes:**

- Increase `NodeOptions.requestTimeout` (default 60 000 ms) for slow operations
- Add backoff + retry in the caller
- On mobile, lower `poolIdleTimeoutMs` to avoid stale connections

---

### `NetworkError` — "peer rejected"

The remote peer received the connection but rejected it at the application
layer. Possible causes:

- Peer's `maxConnectionsPerPeer` limit reached (default 8); wait or increase it
- Peer closed or restarted since you obtained its node ID

---

## Handle errors

### `IrohHandleError` — "invalid handle: N"

See [specification.md § INVALID_HANDLE causes](specification.md#invalid_handle-causes)
for the full list. Most common causes:

1. **Calling `nextChunk()` after EOF.** `nextChunk()` returns `null` at EOF
   and removes the reader handle. The next call with the same handle returns
   an error. Fix: break out of your read loop on `null`.

2. **Calling `finishBody()` twice.** First call drops the writer and returns
   `Ok`; second call returns `IrohHandleError`. Track whether you have already
   finished the body.

3. **Handle expired by TTL sweep.** The default TTL is 5 minutes. If your
   handler takes longer, increase `NodeOptions.advanced.handleTtl`. Very long
   streaming operations (e.g., large file uploads over a slow channel) can
   exceed this limit.

4. **Node closed while request was in flight.** When `node.close()` is called,
   all handles are swept immediately. Any attempt to read from them afterwards
   returns this error.

---

## Serve errors

### Handler returns 500 unexpectedly

If your `serve()` handler throws, iroh-http catches the exception and returns
a 500 response. Check your handler's error handling:

```ts
node.serve(async (req) => {
  try {
    return new Response(await doWork(req));
  } catch (e) {
    // log here so you can see the cause
    console.error('handler error:', e);
    return new Response('internal error', { status: 500 });
  }
});
```

---

### `503` responses (Node.js) or requests queuing up

The Node.js adapter uses a `ThreadsafeFunction` queue between the Rust serve
loop and the JS handler. When the queue is full, new requests are dropped with
503.

**Queue capacity** is controlled by `NodeOptions.internals.channelCapacity`
(default 32). Increase this if your handler is slower than the incoming rate.

**Concurrency limit** is `ServeOptions.maxConcurrency` (default 1024). Requests
above this limit time out with 408.

---

## Discovery errors

### mDNS `browse()` returns no events

mDNS uses UDP multicast on port 5353. Common blockers:

| Symptom | Cause | Fix |
|---------|-------|-----|
| No events at all | Firewall blocks UDP multicast | Allow incoming UDP port 5353 |
| Events from some peers, not others | Peers on different subnets | mDNS is link-local; all peers must be on the same L2 segment |
| Service name mismatch | `serviceName` option differs between browse and advertise | Use the same `serviceName` string on both sides |
| Works on Linux/macOS, not Windows | Different mDNS backend | Ensure mDNS is enabled in Windows networking settings |

---

## Error code reference

| `ErrorCode` | JS class | Common cause | Fix |
|-------------|----------|-------------|-----|
| `InvalidInput` | `IrohArgumentError` | Bad node ID, invalid URL scheme, invalid header | Check input format; URLs must use `httpi://` scheme |
| `ConnectionFailed` | `IrohConnectError` | No network path, peer offline | See [connection errors](#networkerror-on-fetch--connection-refused-or-connection-failed) |
| `Timeout` | `IrohConnectError` | Peer too slow, network timeout | Increase `requestTimeout`; retry with backoff |
| `BodyTooLarge` | `IrohProtocolError` | Request body exceeds `maxRequestBodyWireBytes` or `maxRequestBodyDecodedBytes` | Send smaller payload or increase the relevant limit |
| `HeaderTooLarge` | `IrohProtocolError` | Header block exceeds `maxHeaderBytes` | Reduce header count/size or increase limit |
| `PeerRejected` | `IrohConnectError` | Peer rejected connection at app layer | See [peer rejected](#networkerror--peer-rejected) |
| `Cancelled` | `IrohAbortError` | `AbortSignal` fired or `fetch()` cancelled | Handle `AbortError` in caller; do not retry cancelled requests |
| `Internal` | `IrohError` | Bug in iroh-http or unexpected Rust panic | File an issue with the full error message |
| `InvalidInput` (handle) | `IrohHandleError` | Stale, expired, or wrong-endpoint handle | See [handle errors](#irohhandleerror--invalid-handle-n) |

---

## Debugging tips

### Enable key-log for TLS inspection

```ts
const node = await createNode({ keylog: true });
// set SSLKEYLOGFILE=/tmp/keys.log in environment
// then open in Wireshark: Edit → Preferences → TLS → Pre-Master-Secret log
```

### Inspect endpoint statistics

```ts
const stats = await node.stats();
console.log(stats);
// { activeReaders, activeWriters, activeSessions, totalHandles,
//   poolSize, activeConnections, activeRequests }
```

`totalHandles` growing unboundedly is a sign of leaked handles (see
`IrohHandleError` above). `activeConnections` above `maxPooledConnections`
indicates the pool is saturated.

### Node ID format

Node IDs are lowercase RFC 4648 base32 strings without padding, e.g.:
`abc23defghijk...` (52 characters for a 32-byte public key). If you see a
`TypeError` on a node ID, check that it is exactly 52 lowercase characters
and contains only `a–z2–7`.
