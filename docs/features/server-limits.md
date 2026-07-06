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

  /** Reject request bodies larger than this many wire (compressed) bytes,
   *  before the body is read. Guards against bandwidth floods. Default: 16 MiB. */
  maxRequestBodyWireBytes: 10 * 1024 * 1024,  // 10 MB example

  /** Reject request bodies larger than this many decoded bytes (after
   *  decompression). Primary compression-bomb guard. Default: 16 MiB. */
  maxRequestBodyDecodedBytes: 10 * 1024 * 1024,  // 10 MB example

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

For example, without a request-body limit a peer could stream an unbounded
body, accumulating data in the channel until memory is exhausted. The limits
are checked as bytes arrive in the Rust body reader — no full-body buffering
occurs.

Request bodies are capped by two independent limits. `maxRequestBodyWireBytes`
bounds the **wire** (compressed) size: a declared `Content-Length` larger than
the limit is rejected up front (before the body is read), and the wire stream
is capped as bytes arrive so a chunked upload that omits or understates
`Content-Length` still stops at the limit. `maxRequestBodyDecodedBytes` bounds
the **decoded** size (after decompression), so a small compressed payload that
inflates past the limit once decompressed is stopped as it expands. Both
surface the same `413`; the decoded cap is what protects against
compression bombs.

## What each limit protects against

| Option | Attack vector | Behavior |
|---|---|---|
| `maxConcurrency` | Request flood from many peers | Excess requests queue until a slot is free; if none frees before `requestTimeout`, they receive a `408 Request Timeout` |
| `maxConnectionsPerPeer` | Connection flood from one peer | Excess connections are closed at the QUIC level (transport close, not an HTTP response) |
| `requestTimeout` | Slow request / stalled handler | 408 Request Timeout |
| `maxRequestBodyWireBytes` | Oversized/compressed body exhausting bandwidth | 413 Content Too Large |
| `maxRequestBodyDecodedBytes` | Compression bomb exhausting memory | 413 Content Too Large |
| `maxHeaderBytes` | Header flood exhausting memory | 431 Request Header Fields Too Large |

