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

  /** Maximum request body size in bytes. Requests with larger bodies are
   *  rejected with 413 before the body is read. Default: 16 MiB. */
  maxRequestBodyBytes: 10 * 1024 * 1024,  // 10 MB example

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

For example, without `maxRequestBodyBytes` a peer could stream an unbounded
body, accumulating data in the channel until memory is exhausted. The limit
is checked as bytes arrive in the Rust body reader — no full-body buffering
occurs.

`maxRequestBodyBytes` is enforced in two places against the same byte budget:
a declared `Content-Length` larger than the limit is rejected up front (before
the body is read), and the *decoded* body stream is capped as bytes arrive so a
chunked or compression-bomb upload that omits or understates `Content-Length`
still stops at the limit. Both surface the same `413`; the decoded-stream cap
is what protects against a small compressed payload that inflates past the
limit once decompressed.

## What each limit protects against

| Option | Attack vector | Behavior |
|---|---|---|
| `maxConcurrency` | Request flood from many peers | Excess requests queue until a slot is free; if none frees before `requestTimeout`, they receive a `408 Request Timeout` |
| `maxConnectionsPerPeer` | Connection flood from one peer | Excess connections are closed at the QUIC level (transport close, not an HTTP response) |
| `requestTimeout` | Slow request / stalled handler | 408 Request Timeout |
| `maxRequestBodyBytes` | Oversized body exhausting memory | 413 Content Too Large |
| `maxHeaderBytes` | Header flood exhausting memory | 431 Request Header Fields Too Large |

