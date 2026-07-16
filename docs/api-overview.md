# API Overview

This covers the full API surface shared by all runtimes (Node.js, Deno, Tauri). Import paths differ; everything else is identical.

```ts
import { createNode } from "@momics/iroh-http-node";  // or -deno, -tauri
```

---

## Creating a node

```ts
const node = await createNode();
console.log(node.publicKey.toString()); // base32 node ID, this is your address
```

With options:

```ts
const node = await createNode({
  key: savedKey,                              // Uint8Array, restores a stable identity
  relayMode: "https://my-relay.example.com",  // custom relay (or "default", "staging", "disabled")
  advanced: { drainTimeout: 30_000 },         // ms to wait for in-flight requests on close
});
```

---

## HTTP: fetch and serve

### fetch

```ts
const res = await node.fetch("httpi://<peer-public-key>/api/data");
console.log(res.status, await res.json());
```

`httpi://` is the URL scheme. The host is a base32 public key, not a hostname. The response is a standard `Response` object.

### serve

```ts
node.serve({}, (req) => {
  const peerId = req.headers.get("Peer-Id"); // authenticated caller identity
  return new Response("ok");
});
```

The handler receives a standard `Request` and returns a `Response`, same contract as `Deno.serve`. The `Peer-Id` header is injected by the QUIC layer and cannot be spoofed.

Graceful shutdown via `AbortSignal`:

```ts
const ac = new AbortController();
node.serve({ signal: ac.signal }, handler);
ac.abort(); // stop accepting new connections
```

---

## QUIC sessions

Raw QUIC connections with bidirectional streams, unidirectional streams, and datagrams. The API follows [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport).

### Connect to a peer

```ts
const session = await node.dial("<peer-public-key>");
await session.ready;
```

### Bidirectional streams

```ts
const { readable, writable } = await session.createBidirectionalStream();

const writer = writable.getWriter();
await writer.write(new TextEncoder().encode("hello"));
await writer.close();

const reader = readable.getReader();
const { value } = await reader.read();
console.log(new TextDecoder().decode(value));
```

### Accept incoming sessions

```ts
const ac = new AbortController();
for await (const session of node.incoming({ signal: ac.signal })) {
  console.log("peer:", session.remoteId.toString());
  for await (const { readable, writable } of session.incomingBidirectionalStreams) {
    // handle stream
  }
}
```

### Datagrams

Unreliable, low-latency, no ordering guarantees.

```ts
await session.datagrams.writable.getWriter()
  .write(new TextEncoder().encode("ping"));

const { value } = await session.datagrams.readable.getReader().read();
```

### Unidirectional streams

```ts
// Send-only:
const writable = await session.createUnidirectionalStream();
await writable.getWriter().write(data);

// Receive-only:
const reader = session.incomingUnidirectionalStreams.getReader();
const { value: inStream } = await reader.read();
```

---

## mDNS peer discovery

Find peers on the local network. Desktop uses the Rust DNS-SD transport; the
Tauri adapter uses Android `NsdManager` and Apple Network/Bonjour APIs on mobile.
All platforms feed the same Rust peer projection and endpoint lookup.

### Advertise

```ts
await node.advertisePeer({ serviceName: "my-app" });
```

### Browse

```ts
const ac = new AbortController();
for await (const event of node.browsePeers({ serviceName: "my-app", signal: ac.signal })) {
  if (event.isActive) {
    console.log("found:", event.nodeId, event.addrs);
    const res = await node.fetch(`httpi://${event.nodeId}/api`);
  } else {
    console.log("gone:", event.nodeId);
  }
}
```

---

## Cryptographic utilities

Every node has an Ed25519 keypair. Signing and verification work both as standalone functions and as methods on a live node.

### Standalone (synchronous in Node, async in Deno/Tauri)

```ts
import { generateSecretKey, secretKeySign, publicKeyVerify } from "@momics/iroh-http-node";

const sk  = generateSecretKey();                   // 32-byte Ed25519 seed
const sig = secretKeySign(sk, data);               // 64-byte signature
const ok  = publicKeyVerify(publicKey, data, sig); // boolean
```

### On a live node (always async, round-trips through Rust)

Signing via `node.secretKey` requires a node created with a supplied `key`
(otherwise `secretKey` is `undefined` — the identity is native-held):

```ts
const key  = generateSecretKey();
const node = await createNode({ key });
const sig  = await node.secretKey.sign(data);
const ok   = await node.publicKey.verify(data, sig);
```

### Key persistence

Generate a key, pass it to `createNode`, and store its bytes for next launch:

```ts
const key = generateSecretKey();                   // Uint8Array (32 bytes)
const node = await createNode({ key });            // node.secretKey === key
// persist `key` (or node.secretKey.toBytes()) in secure storage
const restored = await createNode({ key });        // same identity
```

### PublicKey utilities

```ts
const pk = PublicKey.fromString(nodeIdString);      // parse base32 node ID
pk.toString();                                      // base32 string
pk.bytes;                                           // Uint8Array(32)
pk.equals(otherKey);                                // identity comparison
```

---

## Security model

Iroh QUIC authenticates peer *identity* cryptographically. You always know *who* connected. But it does not handle *authorization*. That's your job.

The `Peer-Id` header on incoming requests is injected by the transport layer and stripped if a client tries to set it. It's safe to trust.

```ts
const ALLOWED = new Set(["<peer-key-1>", "<peer-key-2>"]);

node.serve({}, (req) => {
  if (!ALLOWED.has(req.headers.get("Peer-Id")))
    return new Response("Forbidden", { status: 403 });
  return new Response("ok");
});

for await (const session of node.incoming()) {
  if (!ALLOWED.has(session.remoteId.toString())) {
    session.close(403, "Forbidden");
    continue;
  }
}
```

---

## Node lifecycle

```ts
const node = await createNode();

// ... use the node ...

await node.close();  // graceful: drains in-flight requests, then shuts down
```

Or with `using` (TypeScript 5.2+):

```ts
await using node = await createNode();
// automatically closed when scope exits
```
