# Architecture

This document describes the actual architecture of iroh-http as built. It is the single source of truth for how the system is structured, what each component does, and why it is structured this way. Update it when the architecture changes — an outdated architecture doc is worse than none.

For engineering values and invariants, see [principles.md](principles.md). For detailed rationale behind specific technical choices, see [internals/design-decisions.md](internals/design-decisions.md).

---

## What This Is

iroh-http is an HTTP implementation over [Iroh](https://iroh.computer/) QUIC transport, exposed to Deno, Node.js, and Tauri via FFI bridges. Nodes are addressed by Ed25519 public key, not by domain name. Two devices that know each other's public key can exchange HTTP requests peer-to-peer, through NATs, without servers or DNS.

The **user-facing API** is deliberately familiar:
- **`fetch()`** — follows the [WHATWG Fetch specification](https://fetch.spec.whatwg.org/)
- **`serve()`** — follows the [Deno.serve](https://docs.deno.com/api/deno/~/Deno.serve) contract

Any deviation from these contracts is a bug unless explicitly documented.

---

## Layer Diagram

```
┌──────────────────────────────────────────────────────────────┐
│  Platform Adapters                                            │
│  Node.js (napi-rs) · Deno (FFI) · Tauri (invoke)             │
│                                                              │
│  Thin shims. Translate platform types ↔ u64 handles.         │
│  No logic. No state.                                         │
└────────────────────────┬─────────────────────────────────────┘
                         │
┌────────────────────────▼─────────────────────────────────────┐
│  iroh-http-shared (TypeScript)                                │
│                                                              │
│  Bridge interface, makeFetch(), makeServe(), error classes,   │
│  stream helpers, key utilities. Shared by all JS/TS adapters. │
└────────────────────────┬─────────────────────────────────────┘
                         │  fetch / serve / respond / next_chunk / …
┌────────────────────────▼─────────────────────────────────────┐
│  iroh-http-core (Rust)                                        │
│                                                              │
│  client.rs   — fetch()                                       │
│  server.rs   — serve(), IrohHttpService + FfiDispatcher,    │
│                drain                                         │
│  pool.rs     — moka-backed single-flight connection pool     │
│  stream.rs   — slotmap handle registries, body channels      │
│  session.rs  — session/WebTransport-style API                │
│  endpoint.rs — IrohEndpoint, NodeOptions, ServeOptions       │
│  io.rs       — IrohStream: AsyncRead+AsyncWrite adapter      │
│  lib.rs      — CoreError/ErrorCode, FFI types, re-exports   │
└──────┬──────────────────────────────┬────────────────────────┘
       │                              │
┌──────▼──────────┐        ┌──────────▼───────────────────────┐
│  tower-http     │        │  hyper v1                         │
│  Compression    │        │  HTTP/1.1 framing, headers,       │
│  (zstd, gated)  │        │  chunked encoding,                │
│                 │        │  upgrade, body streaming           │
└─────────────────┘        └──────────┬────────────────────────┘
                                      │
┌─────────────────────────────────────▼───────────────────────┐
│  Iroh (noq/Quinn QUIC)                                       │
│  SendStream / RecvStream → AsyncWrite / AsyncRead            │
│  Multiplexing, flow control, encryption, NAT traversal       │
└──────────────────────────────────────────────────────────────┘
```

---

## Core Component Details

### iroh-http-core

The Rust crate that owns all transport logic. Platform adapters depend only on its `pub` API — they have no direct dependency on hyper, tower, or iroh internals.

| File | Responsibility |
|------|----------------|
| `client.rs` | `fetch()`. Obtains a QUIC connection via the pool, wraps it in `IrohStream`, drives hyper's HTTP/1.1 client, pumps the response body into handle-based channels. |
| `server.rs` | `serve()`. Accepts QUIC connections, spawns per-stream hyper HTTP/1.1 handlers, enforces concurrency via drain semaphore, and composes the standard `tower-http` reliability stack (`CompressionLayer`, `RequestDecompressionLayer`, `TimeoutLayer`, `ConcurrencyLimitLayer`, `LoadShedLayer`) per ADR-014. `IrohHttpService` is the thin `tower::Service<Request<B>, Response = Response<Body>, Error = Infallible>` shell at the hyper boundary; it delegates each request to `FfiDispatcher`, which owns the JS-bridge concerns (handle allocation, `on_request` firing, body-channel pumping, response-head rendezvous, duplex upgrade). The only bespoke layer in the stack is `HandleLayerError`, which converts `tower::timeout::Elapsed` / `tower::load_shed::Overloaded` errors into 408 / 503 responses (no `axum::error_handling::HandleErrorLayer` equivalent exists in plain tower / tower-http — see ADR-013). |
| `pool.rs` | `ConnectionPool`. moka async cache keyed by `NodeId`. `try_get_with` provides single-flight connection establishment — concurrent fetches to the same peer share one connection attempt. Failed attempts are not cached. |
| `stream.rs` | All resource handle registries (body readers, writers, fetch tokens, sessions, request heads). Uses `slotmap` for generational u64 keys. Also defines backpressure config (channel capacity, max chunk size). |
| `session.rs` | Session lifecycle: `session_connect` (non-pooled dedicated connections), bidirectional/unidirectional streams, datagrams. Session registry. |
| `endpoint.rs` | `IrohEndpoint` (cheap `Arc` clone). Bind, share, close. `NodeOptions` for QUIC transport config, discovery, relay. `ServeOptions` for server limits. `CompressionOptions` (feature-gated). |
| `io.rs` | `IrohStream`: merges Iroh's split `SendStream`/`RecvStream` into a single `AsyncRead + AsyncWrite` type that hyper can drive directly via `hyper_util::rt::TokioIo`. |
| `lib.rs` | `CoreError`/`ErrorCode` enum, `FfiResponse`, `RequestPayload`, ALPN constants, crypto helpers (sign/verify), base32 encoding, node ticket parsing, `core_error_to_json`/`format_error_json` serializers. |

### Platform Adapters

Each adapter is a thin FFI shim — no logic, no state, just type translation:

| Crate | FFI | Language |
|-------|-----|----------|
| `iroh-http-node` | napi-rs v2 | Node.js / Bun |
| `iroh-http-deno` | Deno FFI (`dlopen`) | Deno |
| `iroh-http-tauri` | Tauri invoke | Tauri (desktop/mobile) |

Adapters translate between platform types (e.g. `BigInt` ↔ `u64`) and call into iroh-http-core. They do not contain business logic. If an adapter needs something currently `pub(crate)` in core, it must be deliberately promoted and documented.

#### Serve Callback Contract

Core's `serve()` accepts an `on_request: Arc<dyn Fn(RequestPayload) + Send + Sync>` callback. Each adapter implements this differently to bridge into its platform runtime:

| Adapter | Mechanism | Model |
|---------|-----------|-------|
| Node | `ThreadsafeFunction` | Push — callback into JS event loop |
| Deno | `mpsc` queue + `nextRequest()` | Pull — JS polls for requests |
| Tauri | Tauri `Channel` | Push — event emitted to frontend |

**Behavioral guarantees of `on_request`:**

1. Called exactly **once per accepted HTTP request** (one call per QUIC bi-stream).
2. Called from a Tokio task — the callback must be `Send + Sync` and must not block the Tokio runtime.
3. The callback receives a `RequestPayload` containing opaque handles; it does not own the underlying resources (those live in the core slab registries).
4. If the callback is slow, the per-request timeout (`request_timeout_ms`) still applies — the hyper response channel will time out regardless of callback latency.
5. The callback must not panic. A panic in a Tokio task aborts only that task, but the request will hang until the timeout fires.

Each adapter is responsible for surfacing errors from its callback mechanism. The core does not observe whether the callback succeeded.

### iroh-http-shared (TypeScript)

Shared TypeScript layer consumed by all JS/TS adapters:
- `Bridge` interface — abstract FFI contract every adapter implements
- `makeFetch()`, `makeServe()`, `makeConnect()` — compose the user-facing API from a Bridge
- `makeReadable()`, `pipeToWriter()` — stream helpers
- Error classification (`classifyError()` → `IrohError`, `IrohConnectError`, `IrohAbortError`, etc.)
- `PublicKey`, `SecretKey`, key utilities

---

## Wire Format

Standard HTTP/1.1 over QUIC bidirectional streams. Each stream carries exactly one request-response exchange. Multiplexing is at the QUIC layer.

```
GET /path HTTP/1.1\r\n
Host: <node-id>\r\n
<headers>\r\n
\r\n
[HTTP/1.1 chunked body]
```

**ALPN strings:** `iroh-http/2` (HTTP request/response) and `iroh-http/2-duplex` (sessions — bi/uni streams, datagrams, server-side `req.upgrade()`). The version bump from 1 to 2 marks the migration from custom framing to hyper. Old and new builds refuse to connect — the ALPN mismatch is intentional.

**URL scheme:** `httpi://<public-key>/path` — clean, parseable, and distinct from `http://`. The `Request` constructor normalizes to `http:` internally; `httpi://` is preserved in `Response.url` and `payload.url`.

---

## Concurrency Model

The drain semaphore in `server.rs` is the central concurrency gate:

- `max_concurrency` (default: 1024) = initial semaphore permits
- One permit acquired **per QUIC bi-stream** (= per HTTP request), not per connection
- Permit drops when hyper finishes serving the request
- `drain()` acquires all permits → blocks until every in-flight request completes

This bounds total in-flight requests across all peers. Per-peer limits are enforced separately via `max_connections_per_peer`.

---

## Connection Pool

moka async cache keyed by `NodeId`. The critical choice is `try_get_with` (not `get_with`):
- On success: caches the connection for reuse
- On failure: does **not** cache the error — next caller retries
- Liveness check before returning a cached connection; on a stale hit the entry is invalidated and one reconnect attempt is made transparently — callers never observe a stale connection

This provides single-flight semantics: many concurrent fetches to the same peer share one connection attempt. No thundering herd.

---

## Handle System

All resource state lives in Rust. Platform adapters hold only opaque `u64` handles (slotmap generational keys).

- Lower 32 bits: slot index
- Upper 32 bits: generation counter
- A stale handle fails with `Err("invalid handle")` instead of silently accessing a new resource

Handles cross FFI as `u64`. In JavaScript: transmitted as `BigInt`, converted at the boundary.

---

## Security Defaults

| Limit | Default | Config |
|-------|---------|--------|
| Max concurrent requests | 64 | `ServeOptions::max_concurrency` |
| Per-request timeout | 60 000 ms | `ServeOptions::request_timeout_ms` |
| Per-peer connection limit | 8 | `ServeOptions::max_connections_per_peer` |
| Max request head size | 64 KB | `NodeOptions::max_header_size` |
| Max request body size (wire) | 16 MiB | `ServeOptions::max_request_body_wire_bytes` |
| Max request body size (decoded) | 16 MiB | `ServeOptions::max_request_body_decoded_bytes` |
| Drain timeout | 30 000 ms | `ServeOptions::drain_timeout_ms` |

All defaults are safe against hostile peers without opt-in. Increasing limits is always explicit.

---

## Error Model

```
Rust (CoreError / ErrorCode)
  ↓ core_error_to_json() / format_error_json()
JSON envelope: {"code":"TIMEOUT","message":"..."}
  ↓ platform adapter
Native error types (DOMException subtypes in JS, etc.)
```

Error codes are a finite enum: `InvalidInput`, `ConnectionFailed`, `Timeout`, `BodyTooLarge`, `HeaderTooLarge`, `PeerRejected`, `Cancelled`, `Internal`. New failure modes get new codes — never rely on catch-all.

---

## Compression

Optional (feature-gated: `compression`). Policy: **zstd-only**.

```rust
tower_http::decompression::DecompressionLayer::new()
    .gzip(false).br(false).deflate(false).zstd(true)
```

Compression belongs in core because the Rust layer reads `Accept-Encoding` before JS sees the body. Skipped when: response already has `Content-Encoding`, `Content-Range`, `Cache-Control: no-transform`, or body is a stream.

---

## Scope Boundaries

The litmus test: *"Has the core already consumed or acted on information before the platform layer sees it?"*

**If yes → core.** Compression negotiation (core reads `Accept-Encoding`), connection limits (core accepts connections before JS can reject them), transport timeouts, upgrade handshakes.

**If no → userland.** Retry logic, caching, rate limiting, auth, tracing export, middleware. The core exposes enough information for userland to implement these; it does not implement them itself.

---

## Open Questions

- [ ] Can `hyper-util` pooling replace the custom moka pool via trait impls on the Iroh transport?
- [ ] Does the Iroh transport support extended CONNECT for WebTransport over HTTP/2, or does it require HTTP/3?
- [ ] What is the error type hierarchy per target language? Is it consistent and documented?
- [ ] How are streaming request/response bodies and WebTransport streams represented across FFI?
- [ ] Path to h3: requires a `h3-noq` crate (analogous to `h3-quinn` but for Iroh's noq fork) — upstream work
