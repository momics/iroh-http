# Specification

> **Normative.** This document defines the public interface contract for all
> iroh-http adapters. It is the single source of truth for what a conformant
> adapter must expose. Coding-style guidance lives in [guidelines/](guidelines/README.md);
> behavioural details live in [features/](features/README.md).
>
> **As of iroh-http-core 0.4.0.** Update this stamp when the public API changes.

---

## Overview

iroh-http provides HTTP/1.1-over-QUIC networking addressed by Ed25519 public
keys. Three JavaScript/TypeScript adapters (Node, Deno, Tauri) expose
identical semantics through platform-appropriate FFI mechanisms.

Every adapter must export the **core interfaces** below. **Feature
interfaces** are required only when the adapter claims to support that feature.

---

## Core Interfaces

### `createNode`

The sole entry point. Creates and returns an `IrohNode`.

```ts
function createNode(options?: NodeOptions): Promise<IrohNode>;
```

See [NodeOptions](#nodeoptions) for the full option set.

---

### `IrohNode`

The primary API surface. All interaction with the network flows through a
node instance.

```ts
interface IrohNode {
  /** The node's Ed25519 public key (its stable network address). */
  readonly publicKey: PublicKey;
  /**
   * The node's Ed25519 secret key — present only when a `key` was supplied to
   * `createNode`. When omitted, the identity is generated natively and never
   * surfaced to JS, so this is `undefined` (identically on Node.js, Deno, and
   * Tauri). `createNode({ key })` narrows the return type to
   * `IrohNodeWithSecret`, where `secretKey` is non-optional.
   */
  readonly secretKey: SecretKey | undefined;

  /** Send an HTTP request to a peer. */
  fetch(
    input: string | URL,
    init?: IrohFetchInit,
  ): Promise<Response>;
  // Self-requests: when `input` targets this node's own id, the request is
  // routed in-process to this node's own `serve()` service instead of dialing
  // over QUIC (iroh forbids self-dial). This requires an active `serve()`;
  // otherwise the fetch rejects with a clear "no active server" error. The
  // handler observes `peer-id == own id`. Loopback skips the wire (no
  // QUIC/TLS, compression, or per-connection server limits). See ADR-015.

  /** Start serving HTTP requests from peers. */
  serve(handler: ServeHandler): ServeHandle;
  serve(options: ServeOptions, handler: ServeHandler): ServeHandle;
  serve(options: ServeOptions & { handler: ServeHandler }): ServeHandle;
  // Note: Rust exports a single serve() function. The three JS overloads
  // are an adapter responsibility — each adapter expands the ergonomic
  // signatures into one Rust call with the appropriate options struct.

  /** Open a WebTransport session to a peer. */
  connect(
    peer: PublicKey | string,
    init?: { directAddrs?: string[] },
  ): Promise<IrohSession>;

  /** Discover peers on the local network via mDNS. */
  browse(
    options?: MdnsOptions,
    signal?: AbortSignal,
  ): AsyncIterable<PeerDiscoveryEvent>;

  /** Advertise this node on the local network via mDNS. */
  advertise(options?: MdnsOptions, signal?: AbortSignal): Promise<void>;

  /** Resolves when the node's endpoint has closed. */
  readonly closed: Promise<WebTransportCloseInfo>;

  /** Get this node's address information (node ID + addresses). */
  addr(): Promise<NodeAddrInfo>;
  /** Get a ticket string for this node (serialised address info). */
  ticket(): Promise<string>;
  /** Get the home relay URL, or null if not connected. */
  homeRelay(): Promise<string | null>;
  /** Get address info for a connected peer, or null. */
  peerInfo(peer: PublicKey | string): Promise<NodeAddrInfo | null>;
  /** Get connection statistics for a peer, or null. */
  peerStats(peer: PublicKey | string): Promise<PeerStats | null>;
  /** Stream path changes for a peer. */
  pathChanges(
    peer: PublicKey | string,
    pollIntervalMs?: number,
  ): AsyncIterable<PathInfo>;

  /** Close the node. */
  close(options?: CloseOptions): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
}
```

---

### `NodeOptions`

Configuration for `createNode`. All fields are optional.

```ts
interface NodeOptions {
  // ── Identity ──────────────────────────────────────────────────────
  /** Pre-existing secret key (restores identity across restarts). */
  key?: SecretKey | Uint8Array;

  // ── Connectivity ──────────────────────────────────────────────────
  /** Relay mode: "default" | "staging" | "disabled" | relay URL(s). */
  relayMode?: RelayMode;
  /** Local bind address(es). */
  bindAddr?: string | string[];
  /** QUIC idle timeout in milliseconds. */
  idleTimeout?: number;

  // ── Discovery ─────────────────────────────────────────────────────
  discovery?: {
    dns?: boolean | { serverUrl?: string };
    mdns?: boolean | { serviceName?: string };
  };

  // ── Proxy ─────────────────────────────────────────────────────────
  proxyUrl?: string;
  proxyFromEnv?: boolean;

  // ── Debug ─────────────────────────────────────────────────────────
  keylog?: boolean;

  // ── Connection pool ───────────────────────────────────────────────
  maxPooledConnections?: number;
  poolIdleTimeoutMs?: number;

  // ── Compression ───────────────────────────────────────────────────
  compression?: boolean | { level?: number; minBodyBytes?: number };

  // ── Limits ────────────────────────────────────────────────────────
  /** Max header block size in bytes. Default: 65 536. */
  maxHeaderBytes?: number;

  // ── Reconnect ─────────────────────────────────────────────────────
  reconnect?: { auto?: boolean; maxRetries?: number };

  // ── Advanced ──────────────────────────────────────────────────────
  advanced?: {
    channelCapacity?: number;
    maxChunkSizeBytes?: number;
    handleTtl?: number;
  };

  // ── Testing ───────────────────────────────────────────────────────
  disableNetworking?: boolean;
}
```

---

### `IrohFetchInit`

Extends the standard `RequestInit` with iroh-specific fields.

```ts
interface IrohFetchInit extends RequestInit {
  /** Direct socket addresses to try before relay. */
  directAddrs?: string[];
}
```

---

### `ServeHandler`

The handler function passed to `node.serve()`.

```ts
type ServeHandler = (req: Request) => Response | Promise<Response>;
```

The incoming `Request` is augmented with:

| Property | Type | Description |
|---|---|---|
| `req.headers.get('Peer-Id')` | `string` | Authenticated peer's public key (base32) |

---

### `ServeHandle`

Returned by `node.serve()`. Controls the running server.

```ts
interface ServeHandle {
  close(): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
}
```

---

### `ServeOptions`

Server-side configuration passed at `node.serve()` time.

```ts
interface ServeOptions {
  /** Called when a handler throws. Returns a fallback Response. */
  onError?: (error: unknown) => Response | Promise<Response>;
  /** Abort signal for graceful shutdown. */
  signal?: AbortSignal;
  /** Max concurrent in-flight requests. Default: 1024. */
  maxConcurrency?: number;
  /** Max QUIC connections from one peer. Default: 8. */
  maxConnectionsPerPeer?: number;
  /** Per-request timeout in ms. Default: 60 000. 0 = disabled. */
  requestTimeout?: number;
  /** Reject bodies larger than this many wire (compressed) bytes.
   *  Default: 16 777 216 (16 MiB). */
  maxRequestBodyWireBytes?: number;
  /** Reject bodies larger than this many decoded bytes (after decompression).
   *  Default: 16 777 216 (16 MiB). */
  maxRequestBodyDecodedBytes?: number;
  /** Max total QUIC connections. Unlimited by default. */
  maxTotalConnections?: number;
  /** Max consecutive accept errors before shutdown. Default: 5. */
  maxServeErrors?: number;
  /** Drain timeout in ms after shutdown. Default: 30 000. */
  drainTimeout?: number;
  /** Reject with 503 at capacity instead of queuing. Default: true. */
  loadShed?: boolean;
}
```

---

## Key Classes

### `PublicKey`

Immutable Ed25519 public key. The node's stable network address.

```ts
class PublicKey {
  /** Copy of the raw 32-byte key material. */
  readonly bytes: Uint8Array;

  /** Lowercase base32 string (the "node ID"). */
  toString(): string;

  /** Construct an `httpi://` URL for this peer. Defaults path to `"/"`. */
  toURL(path?: string): string;

  /** Constant-time equality check. */
  equals(other: PublicKey): boolean;

  /** Verify an Ed25519 signature. Returns false (never throws) on invalid sig. */
  async verify(data: Uint8Array, signature: Uint8Array): Promise<boolean>;

  /** Parse from base32 string (case-insensitive). */
  static fromString(s: string): PublicKey;

  /** Parse a `Peer-Id` header value into a PublicKey. */
  static fromPeerId(id: string): PublicKey;

  /** Construct from 32 raw bytes. Copies the input. */
  static fromBytes(bytes: Uint8Array): PublicKey;
}
```

### `SecretKey`

Ed25519 secret key. Persist `toBytes()` to restore identity across restarts.

```ts
class SecretKey {
  /** Copy of the raw 32-byte secret key material. */
  toBytes(): Uint8Array;

  /** Base32 representation. */
  toString(): string;

  /** The derived public key. Throws if derivePublicKey() has not been called. */
  readonly publicKey: PublicKey;

  /** Generate a fresh random key. */
  static generate(): SecretKey;

  /** Construct from 32 raw bytes. */
  static fromBytes(bytes: Uint8Array): SecretKey;

  /** Parse from base32 string. */
  static fromString(s: string): SecretKey;

  /** Derive the Ed25519 public key via Web Crypto. Caches result. */
  async derivePublicKey(): Promise<PublicKey>;

  /** Sign data. Returns a 64-byte Ed25519 signature. */
  async sign(data: Uint8Array): Promise<Uint8Array>;
}
```

---

## Error Contract

All errors extend `IrohError`. Adapters must use these exact class names and
`name` property values.

| Rust error code | JS class | `name` property | When |
|---|---|---|---|
| `TIMEOUT` | `IrohConnectError` | `"NetworkError"` | Connection timeout |
| `REFUSED` | `IrohConnectError` | `"NetworkError"` | Connection refused |
| `PEER_REJECTED` | `IrohConnectError` | `"NetworkError"` | Peer rejected connection |
| `BODY_TOO_LARGE` | `IrohProtocolError` | `"IrohProtocolError"` | Body exceeds limit |
| `HEADER_TOO_LARGE` | `IrohProtocolError` | `"IrohProtocolError"` | Headers exceed limit |
| `INVALID_HANDLE` | `IrohHandleError` | `"IrohHandleError"` | Stale or invalid handle |
| `ABORTED` / `CANCELLED` | `IrohAbortError` | `"AbortError"` | Request cancelled |
| `INVALID_INPUT` | `IrohArgumentError` | `"TypeError"` | Invalid argument |
| `ENDPOINT_FAILURE` | `IrohBindError` | `"NetworkError"` | Endpoint bind failure |
| *(catch-all)* | `IrohError` | `"IrohError"` | Unclassified error |

```ts
class IrohError extends Error { name = "IrohError"; }
class IrohConnectError extends IrohError { name = "NetworkError"; }
class IrohProtocolError extends IrohError { name = "IrohProtocolError"; }
class IrohHandleError extends IrohError { name = "IrohHandleError"; }
class IrohAbortError extends IrohError { name = "AbortError"; }
class IrohArgumentError extends IrohError { name = "TypeError"; }
class IrohBindError extends IrohError { name = "NetworkError"; }
class IrohStreamError extends IrohError { name = "IrohStreamError"; }
```

---

## Handle Lifecycle

Iroh-http-core represents every in-flight resource — body streams,
fetch-cancel tokens, sessions, pending request heads — as an opaque
`u64` handle at the FFI boundary. This section defines the user-facing
contract: when handles are valid, what happens when they expire, and how to
reason about errors.

> JavaScript receives these as `bigint` values because `number` can only
> represent 53 bits of integer precisely, and handles are 64-bit slotmap keys.

### Handle types

| Handle | Created by | Invalidated by |
|--------|-----------|----------------|
| Body reader | serve loop allocates per request; `allocBodyWriter()` for fetch | `nextChunk()` → EOF (auto-removed), `cancelRequest()`, TTL sweep |
| Body writer | `allocBodyWriter()` | `finishBody()` (drops the sender), TTL sweep |
| Fetch cancel token | `allocFetchToken()` | `cancelFetch()`, or auto-removed when `fetch()` completes |
| Session | `connect()` | `session.close()`, TTL sweep |
| Request head | serve loop allocates per request | `respond()` (fires once, then removed) |

### Lifetime rules

1. **Allocated before the callback fires.** On the serve path all handles for a
   request are inserted into the store before the JS handler is called. The
   handler always receives a valid, fully-initialised handle set.

2. **EOF / completion auto-removes.** `nextChunk()` at EOF removes the reader
   handle automatically.
   `respond()` removes the request-head sender. You do not need to call any
   cleanup function at EOF — but calling the corresponding close/cancel/finish
   on the *other* side is still required.

3. **Explicit close.** `finishBody(writerHandle)` drops the writer, signalling
   EOF to the reader. `cancelReader(bodyReaderHandle)` cancels any in-flight
   `nextChunk` (returning `null` immediately) and removes the handle.
   `cancelFetch(token)` cancels an in-progress fetch.

4. **TTL sweep.** Any handle that is neither consumed nor explicitly freed
   within the TTL window (default **5 minutes**) is removed by a background
   sweep that runs every 60 seconds. This prevents handle leaks when JS code
   abandons a request mid-stream. Configure with `NodeOptions.advanced.handleTtl`
   (milliseconds).

5. **Per-endpoint scoping.** Each node has its own isolated HandleStore. A
   handle issued by node A is meaningless on node B. When a node is closed
   (`node.close()`), all of its handles are swept immediately — any subsequent
   call with those handles returns `INVALID_HANDLE`.

### State diagram

```
allocate ──► in-use ──► EOF / explicit close ──► removed
                  │
                  ├──► cancelReader() ──────────► removed (in-flight read returns null)
                  │
                  └──► TTL expiry ──────────────► swept (in-flight read returns null)
```

### Cancellation contract

`cancelReader(handle)` (Rust: `HandleStore::cancel_reader`) removes the reader
from the handle store and fires a cancellation signal. Any in-flight
`nextChunk()` await returns `null` immediately. Subsequent `nextChunk()` calls
with the same handle return `INVALID_HANDLE`.

TTL sweep of a reader also fires the cancellation signal, so in-flight reads
terminate promptly rather than hanging on a dropped receiver.

Drop-based EOF (when the writer is dropped without `finishBody`) closes the
mpsc channel. The reader's next `nextChunk()` returns `null` (EOF) and
auto-removes the handle. This is the normal completion path.

After a handle transitions to "removed" or "swept", any call that references
it returns an `IrohHandleError` (name `"IrohHandleError"`).

### `INVALID_HANDLE` causes

An `IrohHandleError` means one of:

1. **Already freed** — the handle was consumed (EOF, `finishBody`, `respond`)
   or cancelled. Common adapter bug: calling `nextChunk()` after
   receiving `null` (EOF), or calling `finishBody()` twice.

2. **TTL-expired** — the handle lived past the TTL window without being
   consumed. Increase `handleTtl` if your use case requires long-lived handles.

3. **Wrong endpoint** — the handle was issued by a different node instance.

4. **Never valid** — the handle value was never issued (programming error in
   the adapter).

### Calling `nextChunk()` after EOF

`nextChunk()` returns `null` at EOF. After that, the reader handle is
automatically removed. Calling `nextChunk()` again with the same handle will
return an `IrohHandleError`. Design loops as:

```ts
while (true) {
  const chunk = await req.body.getReader().read();
  if (chunk.done) break;
  // process chunk.value
}
```

The `Request` body exposed by `node.serve()` wraps the native handle in a
`ReadableStream`, so you never need to call `nextChunk()` directly.

---

## Supporting Types

```ts
type RelayMode = "default" | "staging" | "disabled" | string | string[];

interface NodeAddrInfo {
  id: string;       // Base32-encoded public key
  addrs: string[];   // Relay URLs and/or "host:port" strings
}

interface PeerStats {
  relay: boolean;
  relayUrl: string | null;
  paths: PathInfo[];
  /** Round-trip time in milliseconds. `null` if no active QUIC connection. */
  rttMs: number | null;
  /** Total UDP bytes sent to this peer. `null` if no active connection. */
  bytesSent: number | null;
  /** Total UDP bytes received from this peer. `null` if no active connection. */
  bytesReceived: number | null;
  /** Total packets lost on the QUIC path. `null` if no active connection. */
  lostPackets: number | null;
  /** Total packets sent on the QUIC path. `null` if no active connection. */
  sentPackets: number | null;
  /** Current congestion window in bytes. `null` if no active connection. */
  congestionWindow: number | null;
}

interface PathInfo {
  relay: boolean;
  addr: string;
  active: boolean;
}

interface CloseOptions {
  closeCode?: number;
  reason?: string;
}
```

### `EndpointStats`

Endpoint-level observability snapshot returned by `node.endpointStats()`.

```ts
interface EndpointStats {
  /** Currently open body reader handles. */
  activeReaders: number;
  /** Currently open body writer handles. */
  activeWriters: number;
  /** Live QUIC sessions (WebTransport connections). */
  activeSessions: number;
  /** Total allocated handles (reader + writer + session + other). */
  totalHandles: number;
  /** QUIC connections currently cached in the connection pool. */
  poolSize: number;
  /** Live QUIC connections accepted by the serve loop. */
  activeConnections: number;
  /** HTTP requests currently being processed. */
  activeRequests: number;
}
```

### `ConnectionEvent`

Fired when a QUIC peer connection opens or closes.

```ts
interface ConnectionEvent {
  /** Base32-encoded public key of the peer. */
  peerId: string;
  /** `true` = first connection opened (0→1); `false` = last connection closed (1→0). */
  connected: boolean;
}
```

### `TransportEvent`

Transport-level events emitted by the endpoint, forwarded as
`CustomEvent('transport', { detail })` on the JS `IrohNode` instance.

```ts
type TransportEvent =
  | { type: "pool:hit"; peerId: string; timestamp: number }
  | { type: "pool:miss"; peerId: string; timestamp: number }
  | { type: "pool:evict"; peerId: string; timestamp: number }
  | { type: "path:change"; peerId: string; addr: string; relay: boolean; timestamp: number }
  | { type: "handle:sweep"; evicted: number; timestamp: number };
```

### `CompressionOptions`

Configuration for response body compression. Only available when the
`compression` feature is enabled.

```ts
interface CompressionOptions {
  /** Zstd compression level (1–22). Default: 3 (zstd default). */
  level?: number;
  /** Minimum body size in bytes before compression is applied. Default: 512. */
  minBodyBytes?: number;
}
```

---

## Rust FFI Types

> These types define the FFI boundary between `iroh-http-core` and all three
> adapters. They are not directly visible to JS but determine the wire format
> of every cross-boundary call. Adapter authors must map these correctly.

### `FfiResponse`

Returned by `fetch()` at the Rust layer.

```ts
// Rust struct — all adapters must map these fields to the JS Response.
interface FfiResponse {
  status: number;                   // HTTP status code
  headers: [string, string][];      // key-value header pairs
  bodyHandle: bigint;               // Handle to a BodyReader (0 = no body)
  url: string;                      // Full httpi:// URL
}
```

`bodyHandle` is `0` (the slotmap null sentinel) for null-body status codes
(RFC 9110 §6.3: 204, 205, 304). Adapters should treat `0` as "no body".

### `RequestPayload`

Passed to the serve callback per incoming request.

```ts
interface RequestPayload {
  reqHandle: bigint;        // Handle to the pending response head
  reqBodyHandle: bigint;    // Handle to a BodyReader for the request body
  resBodyHandle: bigint;    // Handle to a BodyWriter for the response body
  method: string;
  url: string;
  headers: [string, string][];
  remoteNodeId: string;     // Base32-encoded peer public key
  isBidi: boolean;          // Whether this is a bidirectional stream upgrade
}
```

### `FfiDuplexStream`

Handles for the two sides of a full-duplex QUIC stream.

```ts
interface FfiDuplexStream {
  readHandle: bigint;
  writeHandle: bigint;
}
```

---

## Rust-Level Functions

> These are the `pub` functions exported from `iroh-http-core`. Each adapter
> wraps them through its FFI mechanism (napi-rs / Deno FFI / Tauri invoke).

### Endpoint Registry

```
insert_endpoint(ep) → u64          // Register endpoint, return handle
get_endpoint(handle: u64) → Option // Look up by handle (cheap Arc clone)
remove_endpoint(handle: u64) → Option  // Remove and return
close_all_endpoints()              // Drain all + force-close (Tauri hot-reload)
```

Handles are slotmap keys (generational), not plain indices.

### Fetch and Connect

```
fetch(endpoint, peer, path, method, headers, body_reader?) → FfiResponse
```

When `peer` equals the endpoint's own node id, `fetch` routes the request
in-process to the node's own serve service (ADR-015) rather than dialing over
QUIC, which iroh refuses for self-connections. This requires an active
`serve()`; otherwise it returns a `CONNECTION_FAILED` error explaining the
missing server.

A dedicated `connect`/`raw_connect` FFI symbol no longer exists; bidirectional connections go through the session API (`session_connect`, `session_create_bidi_stream`, etc., see below).

### Serve

```
serve(endpoint, options, callback) → ServeHandle
serve_with_events(endpoint, options, request_cb, event_cb) → ServeHandle
respond(endpoint, req_handle, status, headers) → ()
```

The JS adapters expand `serve()` into three overload signatures for
ergonomics. The Rust layer has a single function; overload expansion is
an adapter responsibility.

### Session Functions

```
session_connect(endpoint, peer, addrs?) → u64          // Open/reuse session
session_accept(endpoint) → Option<u64>                  // Accept incoming session
session_remote_id(endpoint, handle) → [u8; 32]          // Remote peer's public key
session_close(endpoint, handle, code?, reason?)         // Close session
session_ready(endpoint, handle) → ()                    // Await session ready
session_closed(endpoint, handle) → CloseInfo            // Await session close
session_create_bidi_stream(endpoint, handle) → FfiDuplexStream
session_next_bidi_stream(endpoint, handle) → Option<FfiDuplexStream>
session_create_uni_stream(endpoint, handle) → u64       // Writer handle
session_next_uni_stream(endpoint, handle) → Option<u64> // Reader handle
session_send_datagram(endpoint, handle, data)
session_recv_datagram(endpoint, handle) → Bytes
session_max_datagram_size(endpoint, handle) → Option<usize>
```

### Crypto Utilities

```
generate_secret_key() → [u8; 32]
secret_key_sign(key: [u8; 32], data) → [u8; 64]
public_key_verify(key: [u8; 32], data, sig: [u8; 64]) → bool
```

JS adapters expose these via `SecretKey.sign()`, `PublicKey.verify()`, and
`SecretKey.generate()`.

### Handle Store

```
StoreConfig {
  channel_capacity: usize,     // Body-channel depth. Default: 32.
  max_chunk_size: usize,       // Max bytes per chunk. Default: 64 KB.
  drain_timeout: Duration,     // Slow-reader timeout. Default: 30 s.
  max_handles: usize,          // Max slots per registry. Default: 65 536.
  ttl: Duration,               // Handle TTL before sweep. Default: 5 min.
}
```

Configurable via `NodeOptions.advanced` on the JS side:
`channelCapacity`, `maxChunkSizeBytes`, `handleTtl`.

### `ServeHandle`

Returned by `serve()` / `serve_with_events()` at the Rust layer.

| Method | Description |
|---|---|
| `shutdown()` | Notify the serve loop to stop accepting new connections |
| `drain(self)` | Shutdown + await completion (respects `drainTimeout`) |
| `abort()` | Forcefully cancel the serve task |
| `done()` | Resolves when the serve task has fully exited |

The default drain timeout is **30 seconds** (`DEFAULT_DRAIN_TIMEOUT_MS`).
`close()` on the JS side maps to `drain()`. `close_force()` maps to `abort()`.

### Parsed Node Address

```
ParsedNodeAddr {
  node_id: PublicKey,
  direct_addrs: Vec<SocketAddr>,
}

parse_node_addr(s: &str) → ParsedNodeAddr
node_ticket(ep) → String
```

`parse_node_addr` accepts bare node IDs (base32), JSON-encoded
`NodeAddrInfo`, or ticket strings. Relay URLs in `addrs` are silently
skipped (handled by the relay subsystem). Malformed socket addresses
cause a deterministic error.

---

## Feature Interfaces

These are required only when the adapter claims to support the corresponding
feature.

### Streaming

Request and response bodies support the standard `ReadableStream` /
`WritableStream` APIs. No additional interfaces are defined — use the
Web Streams API directly. See [streaming.md](features/streaming.md) for
patterns.

### Sign / Verify

Provided by [`PublicKey.verify()`](#publickey) and [`SecretKey.sign()`](#secretkey).

| Value | Type | Description |
|---|---|---|
| signature | `Uint8Array` (64 bytes) | Ed25519 signature |
| `verify` result | `boolean` | `false` on invalid signature, never throws |

### Discovery (mDNS)

```ts
interface PeerDiscoveryEvent {
  isActive: boolean;  // true = appeared, false = departed
  nodeId: string;     // Base32-encoded public key
  addrs?: string[];   // Known socket addresses
}

interface MdnsOptions {
  serviceName?: string;  // Default: "iroh-http"
}
```

### Compression

Enabled via `NodeOptions.compression`. No additional runtime interfaces —
compression is transparent. See [compression.md](features/compression.md)
for configuration.

### Server Limits

Configured via `NodeOptions` or `ServeOptions`. See [server-limits.md](features/server-limits.md) for behaviour.

| Option | Attack vector | HTTP status on limit |
|---|---|---|
| `maxConcurrency` | Request flood | 408 Request Timeout |
| `maxConnectionsPerPeer` | Connection flood | Closed at QUIC level |
| `requestTimeout` | Slow request | 408 Request Timeout |
| `maxRequestBodyWireBytes` | Oversized/compressed body | 413 Content Too Large |
| `maxRequestBodyDecodedBytes` | Compression bomb | 413 Content Too Large |
| `maxHeaderBytes` | Header flood | 431 Request Header Fields Too Large |

### WebTransport

```ts
interface IrohSession {
  readonly remoteId: PublicKey;
  readonly ready: Promise<undefined>;

  createBidirectionalStream(): Promise<WebTransportBidirectionalStream>;
  createUnidirectionalStream(): Promise<WritableStream<Uint8Array>>;

  readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
  readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;

  readonly datagrams: WebTransportDatagramDuplexStream;
  readonly closed: Promise<WebTransportCloseInfo>;

  close(info?: WebTransportCloseInfo): void;
  [Symbol.asyncDispose](): Promise<void>;
}

interface WebTransportBidirectionalStream {
  readonly readable: ReadableStream<Uint8Array>;
  readonly writable: WritableStream<Uint8Array>;
}

interface WebTransportDatagramDuplexStream {
  readonly readable: ReadableStream<Uint8Array>;
  readonly writable: WritableStream<Uint8Array>;
  readonly maxDatagramSize: number | null;
  incomingHighWaterMark: number;
  outgoingHighWaterMark: number;
}

interface WebTransportCloseInfo {
  closeCode: number;
  reason: string;
}
```

### Tickets

No additional interfaces. `node.ticket()` returns a `string`;
`ticketNodeId(ticket)` extracts the node ID. See
[tickets.md](features/tickets.md).

---

## Conformance

An adapter is **conformant** when:

1. It exports `createNode` (or `create_node`) returning an object satisfying
   the `IrohNode` interface.
2. It re-exports `PublicKey` and `SecretKey` as named exports.
3. All error types match the [error contract](#error-contract).
4. For each feature the adapter claims, all interfaces in the corresponding
   feature section are implemented.
5. Non-implemented features throw `IrohError` with a descriptive message
   rather than silently failing.

---

## Building on Top

The core interfaces above can be composed into higher-level patterns.
See [recipes/](recipes/README.md) for practical examples including:

- [Sealed messages](recipes/sealed-messages.md) — encrypt to a peer's public key using ECIES (Ed25519→X25519 + AES-GCM)
- [Device handoff](recipes/device-handoff.md) — transfer sessions between devices
- [Local-first sync](recipes/local-first-sync.md) — CRDT-based data synchronisation
- [Capability tokens](recipes/capability-tokens.md) — delegated authorisation via signed tokens
