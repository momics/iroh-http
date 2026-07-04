# Server Limits

All resource limits are configured at **`serve(options, handler)`** and enforced
in Rust before any JavaScript handler runs. They protect the serve loop
against misbehaving or hostile peers at the transport level.

## Options

```ts
node.serve({
  /** Maximum simultaneous in-flight requests across all peers. Default: 1024. */
  maxConcurrency: 1024,

  /** Maximum simultaneous connections from a single peer. Default: 8. */
  maxConnectionsPerPeer: 8,

  /** Per-request timeout in milliseconds. Default: 60 000. */
  requestTimeout: 60_000,

  /** Maximum request body size in **wire** (compressed) bytes. Requests with
   *  larger bodies are rejected with 413 before the body is read. Default: 16 MiB. */
  maxRequestBodyWireBytes: 10 * 1024 * 1024,  // 10 MB example

  /** Maximum request body size in **decoded** bytes (after decompression).
   *  Primary compression-bomb guard. Default: 16 MiB. */
  maxRequestBodyDecodedBytes: 10 * 1024 * 1024,  // 10 MB example

  /** Maximum consecutive accept-loop errors before shutdown. Default: 5. */
  maxServeErrors: 5,

  /** Drain timeout in ms after shutdown signal. Default: 5 000. */
  drainTimeout: 5_000,
}, handler);

// Header size is configured at node level:
const node = await createNode({
  limits: {
    /** Maximum request header block size in bytes. Default: 64 KB. */
    maxHeaderBytes: 64 * 1024,
  },
});
```

All limits are optional. Omitting a limit uses the default shown above.

## Why at the Rust layer

These limits intercept bytes or connections before they reach the FFI
boundary. A JS handler never runs for a rejected request, so no user code
needs to handle the overflow cases.

For example, without `maxRequestBodyWireBytes` a peer could stream an unbounded
body, accumulating data in the channel until memory is exhausted. The limit
is checked as bytes arrive in the Rust body reader — no full-body buffering
occurs.

## Wire bytes vs decoded bytes

Request body size is capped at two distinct points, and both default to 16 MiB:

- **`maxRequestBodyWireBytes`** limits the **compressed** bytes as they arrive
  off the network. It guards against high-bandwidth floods at the transport
  level, before any decompression work is done.
- **`maxRequestBodyDecodedBytes`** limits the **decoded** bytes *after*
  decompression. This is the primary compression-bomb guard: a payload that is
  tiny on the wire (e.g. a zstd stream) but expands to gigabytes is rejected
  here.

Both are enforced incrementally as bytes flow, so an oversized body is rejected
with `413 Content Too Large` without ever being fully buffered. When
`decompress` is `false`, bodies are not decoded and only the wire limit applies.

There is **no** single `maxRequestBodyBytes` option — use the two caps above.

## What each limit protects against

| Option | Attack vector | Behavior |
|---|---|---|
| `maxConcurrency` | Request flood from many peers | Excess requests queue until a slot is free; if none frees before `requestTimeout`, they receive a `408 Request Timeout` |
| `maxConnectionsPerPeer` | Connection flood from one peer | Excess connections are closed at the QUIC level (transport close, not an HTTP response) |
| `requestTimeout` | Slow request / stalled handler | 408 Request Timeout |
| `maxRequestBodyWireBytes` | Oversized compressed body exhausting bandwidth/memory | 413 Content Too Large |
| `maxRequestBodyDecodedBytes` | Compression bomb expanding after decode | 413 Content Too Large |
| `maxHeaderBytes` | Header flood exhausting memory | 431 Request Header Fields Too Large |

