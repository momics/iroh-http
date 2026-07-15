# JavaScript / TypeScript Guidelines

Applies to: `iroh-http-node`, `iroh-http-deno`, `iroh-http-tauri` (guest JS), and `iroh-http-shared`.

For engineering values and invariants, see [principles.md](../principles.md).

---

## Naming

| Scope | Convention | Example |
|-------|------------|---------|
| Functions | `camelCase` | `createNode`, `makeFetch` |
| Types | `PascalCase` | `IrohNode`, `PublicKey` |
| Constants | `UPPER_SNAKE` | `METHODS_WITH_BODY` |
| Properties | `camelCase` | `nodeId`, `bodyHandle` |
| Events | `lowercase-dash` | `"abort"`, `"close"` |

Prefix internal/FFI-only types with `Ffi` (`FfiRequest`, `FfiResponseHead`). Never export `Ffi`-prefixed types from user-facing modules.

---

## Use Platform Types

| Concept | Use | Never |
|---------|-----|-------|
| Request | `Request` (WHATWG) | Custom request class |
| Response | `Response` (WHATWG) | Custom response class |
| Headers | `Headers` (WHATWG) | `Record<string, string>`, bare tuples |
| Readable body | `ReadableStream<Uint8Array>` | `AsyncIterator`, Node `stream` |
| Byte data | `Uint8Array` | `ArrayBuffer`, Node `Buffer` in APIs |
| Cancellation | `AbortSignal` | Boolean flags, custom cancel tokens |
| URL | `URL` or `string` | Custom URL class |
| Errors | `DOMException` subtypes | Generic `Error`, string throws |
| Async results | `Promise<T>` | Callbacks, EventEmitter |
| Cleanup | `Symbol.asyncDispose` | Manual `.destroy()` / `.free()` |

Internal code may use `[string, string][]` for header pairs across FFI, but convert to `Headers` before reaching user code.

---

## Error Handling

All errors thrown to user code must be subclasses of `DOMException` or typed error classes from `@momics/iroh-http-shared/errors`. Never throw plain strings. Never expose Rust error messages without classification.

The Rust core emits a structured JSON envelope (`{"code":"TIMEOUT","message":"..."}`). The JS layer maps these to typed error classes — see the [error contract in the specification](../specification.md#error-contract) for the full mapping table.

Use `instanceof` for class checks; use `.code` for the specific Rust error code and `.name` for web-platform-style matching.

---

## Async Patterns

- All I/O is `async`/`await`. No synchronous FFI calls that block the event loop.
- Every long-running operation (`fetch`, `createBidirectionalStream`) accepts `signal?: AbortSignal`. If already aborted, reject immediately with `AbortError` before touching the transport.
- Fire-and-forget calls (e.g. `cancelFetch`) are synchronous and intentionally return no `Promise`.
- `IrohNode` implements `Symbol.asyncDispose` for `await using` compatibility. `close()` is also available. `node.closed` is a `Promise<void>` that resolves on shutdown.

---

## Streaming

Body streams are `ReadableStream<Uint8Array>`.

- One `ReadableStream` per body handle. Never create two streams from the same handle.
- Request bodies that support streaming set `duplex: "half"` in `RequestInit`.
- `cancel()` on a stream calls the appropriate bridge cancellation path.

---

## Serve Handler Contract

See [`ServeHandler` in the specification](../specification.md#servehandler) for the canonical type.

- The handler receives a standard `Request`. Authenticated peer identity is available as the `Peer-Id` header.
- The `httpi:` scheme is normalised to `http:` before constructing `Request` so the WHATWG URL parser accepts it.

---

## Fetch Signature

See [`IrohFetchInit` in the specification](../specification.md#irohfetchinit) for the canonical type.

`IrohFetchInit` extends standard `RequestInit` with `signal?: AbortSignal`,
`directAddrs?: string[]`, and `relayUrl?: string`. An explicit relay URL lets a
fresh node dial a discovered peer without waiting for DNS lookup; iroh may still
upgrade the connection to a direct path.

---

## Bridge Interface

`Bridge` is the only platform-varying abstraction. Each adapter (Node, Deno, Tauri) implements it; `iroh-http-shared` composes the user-facing API from it. Bridge methods use opaque integer handles — never expose handles in user-facing APIs.

---

## Platform Adapters

| Adapter | FFI mechanism | Notable constraint |
|---------|---------------|--------------------|
| Node.js | napi-rs | Chunks cross as `Buffer`; serve uses `ThreadsafeFunction` |
| Deno | C-ABI `dlopen` | JSON-based dispatch; chunks as raw byte pointers |
| Tauri | `invoke` + `Channel` | Chunks as base64 (invoke limitation); serve via Channel |

All adapters produce the same `IrohNode` interface via `buildNode` from `iroh-http-shared`.

---

## Testing

- Test through the public `IrohNode` surface, not through `Bridge` methods directly.
- Serve handler tests use real QUIC connections (two nodes, same process).
- Cancellation and timeout tests must exercise `AbortSignal` integration.
- Streaming tests cover early cancellation, backpressure, and write completion.
