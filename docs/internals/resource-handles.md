# Resource Handles

All runtime resources in iroh-http-core — body streams, fetch cancellation tokens, sessions, and pending request heads — are referenced by opaque `u64` handles at the FFI boundary. This document explains the handle system.

---

## Why u64 handles

Platform FFI boundaries (napi-rs, Deno, PyO3, Tauri) cannot safely hold Rust references or `Arc` values. Instead, the platform side holds an integer key and calls back into Rust with that key. Rust looks up the corresponding resource and performs the operation.

The handle is a `u64` because that is the natural FFI type for a 64-bit integer on all supported platforms, and because the underlying slotmap key is a `u64` (`KeyData::as_ffi()`).

> In TypeScript/JavaScript, these are `bigint` values (not `number`) since `number` has only 53 bits of integer precision.

---

## Generational slotmap keys

Handles are produced by `slotmap`. Each slotmap key encodes:

- **32-bit index** — slot position in the backing array
- **32-bit generation** — incremented each time a slot is reused

When a resource is freed (e.g. `finish_body(handle)` drops the `BodyReader`), the slot's generation is incremented. Any subsequent call with the old handle finds a mismatched generation and returns `Err("invalid handle: …")` — no panic, no use-after-free, no silent wrong-resource access.

This makes handle invalidation automatic and cheap. There is no additional bookkeeping needed.

---

## Handle types

`stream.rs` defines seven distinct slotmap key types and one registry per type:

| Key type | Resource | Produced by | Consumed/freed by |
|----------|----------|-------------|-------------------|
| `ReaderKey` | `BodyReader` (mpsc receiver) | `insert_reader` | `next_chunk` (EOF auto-removes), `cancel_reader` |
| `WriterKey` | `BodyWriter` (mpsc sender) | `insert_writer` | `finish_body` (drops writer) |
| `FetchCancelKey` | `CancellationToken` | `alloc_fetch_token` | `cancel_in_flight`, `remove_fetch_token` |
| `SessionKey` | `SessionEntry` (QUIC session state) | `insert_session` | `remove_session` |
| `RequestHeadKey` | `oneshot::Sender<ResponseHeadEntry>` | `allocate_req_handle` | `take_req_sender` (called by `respond()`) |

Each registry is a `Mutex<SlotMap<K, Timed<T>>>` owned by the per-endpoint `HandleStore`.

---

## Endpoint association

Handles are per-endpoint, not process-global. Each `IrohEndpoint` owns its own
`HandleStore`. When the `IrohEndpoint` is dropped / closed, the `HandleStore` is
dropped with it and all handles become invalid immediately.

---

## Handle lifecycle — typical request

```
serve loop                       JS handler
   │                                │
   │  insert_reader → req_body_handle ──────────────────► nextChunk(req_body_handle)
   │  insert_writer → res_body_handle ──────────────────► sendChunk(res_body_handle, …)
   │  allocate_req_handle → req_handle ─────────────────► respond(req_handle, 200, […])
   │                                │
   │  head_rx.await ◄─────── take_req_sender fires ◄────── respond() called
   │  (serves response)
   │
   │  body_from_reader(res_body_reader) ◄── JS called sendChunk / finish_body
```

All three handles are created before the JS callback fires, so JS always receives a fully valid set of handles.

---

## Fetch cancellation tokens

`alloc_fetch_token(ep_idx)` allocates a `FetchCancelKey` backed by a `CancellationToken`. The token is passed to `fetch()` as an optional argument. Calling `cancel_in_flight(token)` fires the token, which races against the in-progress HTTP exchange and returns `Err("aborted")` if it wins.

Tokens are automatically swept when `unregister_endpoint` runs, preventing leaks if a token is allocated but never explicitly freed.

---

## Thread safety

All slotmap operations lock the per-registry `Mutex`. This is a short critical section (insert/remove/lookup), so contention is negligible under normal workloads. The `OnceLock` wrapper ensures each registry is initialised exactly once at first use.

---

## TTL sweep

Every `IrohEndpoint` spawns a background task at bind time that sweeps expired
handles every **60 seconds**. The default TTL is **5 minutes**
(`DEFAULT_SLAB_TTL_MS = 300_000`). Configure with
`NodeOptions.internals.handleTtl` (milliseconds).

The sweep retains only entries whose `created_at.elapsed() < ttl`:

```rust
reg.retain(|_, entry| !entry.is_expired(ttl));
```

Swept handles are silently discarded. No error is reported during the sweep
itself — the error surfaces on the next call from JS that references the
expired handle, which returns `INVALID_HANDLE`.

**When to raise TTL**: if your application streams very large bodies or keeps
sessions open beyond 5 minutes, increase `handleTtl`. The default is generous
for request-response workloads but may be tight for long-lived streaming.

---

## Endpoint close sweep

When an `IrohEndpoint` is dropped / closed, the `HandleStore` is dropped with
it. All slot-maps are freed. Any subsequent call from JS with a handle from
that store returns `INVALID_HANDLE`. This is the expected signal that the
endpoint has gone away — callers should treat it the same as a network error
and stop referencing the closed node.

---

## Debugging stale handles

If a function returns `Err("invalid handle: N")` it means one of:

1. **Already freed** — the handle was consumed (EOF, `finish_body`, `respond`) or cancelled. Check for double calls.
2. **TTL-expired** — the handle lived past the TTL. Increase `handleTtl` if needed.
3. **Wrong endpoint** — the handle was issued by a different `IrohEndpoint` instance.
4. **Endpoint closed** — the node was closed while a request was in flight.
5. **Never valid** — the handle value was never issued (programming error in the adapter).

The generation in the handle key makes (1) reliable: the old handle can never
silently alias a new resource.
