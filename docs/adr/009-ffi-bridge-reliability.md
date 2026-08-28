---
id: "009"
title: "FFI bridge reliability under concurrent load"
status: accepted
date: 2026-04-25
area: ffi | transport
tags: [ffi, deno, node, tauri, concurrency, race-condition, backpressure]
---

# [009] FFI bridge reliability under concurrent load

## Decision

The `#119`/`#122`/`#123` stale-handle race class this exploration opened against
has since been **closed in core**, not at the bridges:

- **Option A (oneshot acknowledgment)** landed as the response-head oneshot
  rendezvous in `FfiDispatcher::dispatch`
  (`crates/iroh-http-core/src/ffi/dispatcher.rs`): the request task hands the
  head slot to JS and awaits `respond()` before proceeding, so the handle cannot
  be freed inside the timing window.
- **Option C (ownership tokens)** landed as `ReqHeadGuard` + the `InsertGuard`
  multi-handle allocation guards in `crates/iroh-http-core/src/ffi/handles.rs`,
  which roll back partially-allocated handles on every dispatch exit path.

With the race class closed in core, the remaining decision is about **code
shape, not reliability**: the request-delivery preamble each bridge ran before
handing a request to its runtime was partly duplicated (the header reshape into
`Vec<Vec<String>>` and the fail-closed undeliverable fallback). That preamble is
now lifted into `iroh-http-adapter` behind a `RequestTransport` trait
(`deliver_request` + `DeliverableRequest` + `Undeliverable`, see
[#315](https://github.com/Momics/iroh-http/issues/315)). Each bridge keeps only
its genuinely runtime-specific byte transport:

| Adapter | `RequestTransport` impl | Native failure mapped to `Undeliverable`    |
| ------- | ----------------------- | ------------------------------------------- |
| Node    | `NodeTransport`         | `ThreadsafeFunction` enqueue status ≠ `Ok`  |
| Deno    | `DenoTransport`         | registry miss / serve shutdown / queue full |
| Tauri   | `TauriTransport`        | `Channel::send` error                       |

This is **additive and non-breaking** per
[005 — FFI versioning](005-ffi-versioning-compatibility.md): the seam lives
entirely inside the adapter layer, which is internal and co-versioned with core.
There is no JS-visible contract change — the concern raised below that "a
callback contract change is a breaking FFI change" does not apply because the
core `on_request` callback shape is unchanged.

Folding the undeliverable fallback into the shared path also fixed a
released-behaviour bug: on the undeliverable-503 path Node and Deno sent the
rejection head but never finished the response body writer, so the client hung
until the drain timeout. `deliver_request` now finishes the writer on every
undeliverable path (regression guarded by
`deno_delivery_undeliverable_finishes_response_body` and the shared
`MockTransport` conformance test).

Scope: unary request delivery only. Bidirectional streaming uses the separate
session-stream API (`Session::create_bidi_stream` / `next_bidi_stream`), which
does not run the delivery preamble and is out of scope here.

## Context

iroh-http routes all transport logic through `iroh-http-core` (Rust) and exposes
it to three JS/TS runtimes via thin FFI bridges. Each adapter uses a different
dispatch mechanism:

| Adapter | Dispatch model                                                                    | Respond path           |
| ------- | --------------------------------------------------------------------------------- | ---------------------- |
| Node    | Callback via `ThreadsafeFunction` (push)                                          | Synchronous NAPI call  |
| Deno    | Bounded mpsc queue + `UnsafeCallback` wake (push notification, synchronous drain) | Synchronous FFI symbol |
| Tauri   | Tauri `Channel` event stream (push)                                               | Typed invoke command   |

The Node adapter has proven reliable under high concurrency (32-stream bursts, 5
MB bodies). The Deno adapter has not. Issues #119 and #122 document race
conditions that produce `unknown handle` and `sendChunk failed` errors under the
same load that Node handles cleanly.

The original root causes were architectural, not incidental: Deno's former
JSON-over-FFI + async `nextRequest()` path introduced latency between handle
allocation in Rust and handle consumption in JS. Issue #122 replaced that path
with synchronous queue draining to avoid `spawn_blocking` starvation. Issue #397
retained that safe drain path but replaced its hot idle polling with a
thread-safe callback that only notifies JS when work is ready.

This exploration asks what architectural changes would bring Deno (and future
adapters) to the same reliability level as Node and Tauri.

## Questions

1. Should the Deno adapter move from a polling (`nextRequest`) model to a
   push-notified model while retaining synchronous queue draining?
2. Should `on_request` in the core avoid spawning detached tokio tasks, and
   instead use a synchronous (non-spawn) sender so the adapter controls the
   async boundary?
3. Is there a backpressure contract that all adapters should implement — e.g.
   the Rust request task waits for JS to acknowledge handle receipt before
   proceeding?
4. Should handle lifetimes be extended with explicit ownership tokens (insert
   guards) so that handles cannot be freed while JS still holds a reference?
5. Can the Tauri `Channel` pattern be generalised as a reference architecture
   for all FFI bridges?

## What we know

### Evidence from issues

- **#119** (closed): Response body pipes were fire-and-forget at every layer.
  Fixed by tracking active pipes in `makeServe`, tracking pending tasks in
  adapter dispatch loops, and adding last-access timestamps to `Timed` handles
  in `stream.rs`. Node now passes clean; Deno does not.
- **#122** (closed): Localised the remaining Deno failures to the adapter
  coupling layer. Key finding: newly allocated handles (version 1, fresh slots)
  are being freed by Rust before JS has called `respond()` or `sendChunk()`. The
  problem is timing, not staleness.
- **#123** (open): `rawRespond`/`pipeToWriter` race on handler failure produces
  spurious `unknown handle` errors. Low severity but confirms the handle
  lifecycle coupling is fragile.

### Architectural observations

**Why Node works:** `ThreadsafeFunction` pushes the payload directly into the
libuv event loop. The NAPI `rawRespond` call is synchronous from the JS thread
into the Rust slab. There is no intermediate queue, no polling latency, and no
detached task.

**Why Deno struggled before #122:** The `on_request` callback used an async
`nextRequest()` dispatch through Deno's fixed `spawn_blocking` pool. Under
concurrent self-fetches, request delivery competed with fetch work and could
deadlock. Deno now uses a bounded queue, a request-ready callback carrying only
an opaque generation token, and synchronous `try_next_request` draining on the
JS thread. This preserves #122's deadlock avoidance without #397's idle poll.

**Why Tauri is safe:** Tauri's `Channel` is owned by the command lifetime.
Frontend and backend are separate processes, so there is no shared-runtime
deadlock risk. The channel is push-based with fail-closed semantics.

### Constraints

- Deno's FFI supports `UnsafeCallback.threadSafe()` for calls originating on
  foreign Tokio threads. The callback pointer must outlive every possible native
  invocation and should be unrefed when it must not keep the process alive.
- Respond must be synchronous in Deno to avoid deadlock when client and server
  share the same single-threaded event loop (documented in #122).
- The core's `on_request` callback must not block the QUIC accept loop.
- Handle TTL sweep must not free actively-used handles (partially fixed by
  last-access timestamps in #119, but the window still exists for Deno).

## Options considered

| Option                                                                                                                                                                           | Upside                                                                      | Downside                                                                                          |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| **A. Oneshot acknowledgment per request** — pair each queued event with a oneshot; Rust request task awaits it before proceeding                                                 | Eliminates the timing window; JS controls when Rust may free the handle     | Adds per-request overhead; changes the core callback contract                                     |
| **B. Synchronous `on_request` sender** — remove `tokio::spawn` in the Deno dispatch; call `try_send` directly from `on_request`                                                  | Simplifies ordering; no detached tasks                                      | `on_request` must remain non-blocking; synchronous `try_send` on a bounded channel satisfies this |
| **C. InsertGuard ownership tokens** — Rust returns an owning guard with the handle; handle cannot be freed until guard is dropped                                                | Explicit lifetime control                                                   | Adds RAII complexity across the FFI boundary; guards must be passed back                          |
| **D. Move Deno to napi-rs** — use `napi-rs` for Deno (via `deno_napi` compat) instead of raw FFI                                                                                 | Same proven dispatch as Node                                                | Adds napi dependency; Deno FFI is the official path and napi compat is not guaranteed             |
| **E. Tolerate stale handles** — make `respond`/`sendChunk`/`finishBody` return Ok for missing handles                                                                            | No architectural change                                                     | Silently drops responses; masks real bugs                                                         |
| **F. Callback wake + synchronous drain** — retain the bounded queue and `try_next_request`, but invoke a thread-safe Deno callback with an opaque generation token after enqueue | Preserves #122 behavior, avoids borrowed payload pointers, sleeps when idle | Requires callback pointer lifetime and restart routing to be explicit                             |

## Implications

- The fix must preserve the core invariant: `on_request` must not block the
  accept loop.
- Any backpressure mechanism applies per-connection (not per-endpoint), so a
  slow handler on one connection does not stall others.
- Changes to the core callback contract affect all three adapters. Option B is
  Deno-local; Options A and C would touch the core.
- This exploration interacts with
  [005 — FFI versioning](005-ffi-versioning-compatibility.md): a callback
  contract change is a breaking FFI change.

## Next steps

- [x] Keep direct bounded `try_send` delivery and synchronous queue draining.
- [x] Add Option F's callback wake without passing request payload pointers.
- [x] Run the regression test from #122 (32-stream burst × 5 iterations) to
      confirm zero stale-handle errors.
- [ ] Audit whether any other adapter has a similar detached-task pattern that
      could surface under future load.
- [x] Document the chosen dispatch contract in
      [architecture.md](../architecture.md) so future adapters (e.g. Python)
      follow the same pattern.
