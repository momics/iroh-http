---
id: "015"
title: "Self-requests via in-process loopback"
status: accepted
date: 2026-06-28
area: api
tags: [client, server, ffi, loopback, fetch, tauri]
---

# [015] Self-requests via in-process loopback

## Context

A node cannot `fetch()` its own node id. iroh's transport hard-blocks
self-dial — a connection attempt to your own public key fails with
`Connecting to ourself is not supported` before any HTTP layer runs. This
surfaced as issue #264.

The asymmetry is sharpest in Tauri, where the `httpi://` URL scheme handler
calls `iroh_http_core::fetch` directly. A WebView can load
`<audio src="httpi://<peer>/track.mp3">` for any *other* peer, but the same
element pointed at the local node's own id fails — even though that node is
serving the exact bytes. Application code that addresses peers uniformly (a
media player, a content-addressed cache, a router that doesn't special-case
"is this me?") hits an opaque transport error with no actionable remedy.

The original triage of #264 leaned toward "won't-fix + userland recipe" on
two technical objections. Both were wrong on re-examination:

- *"A self-request deadlocks the FFI callback."* It does not. `FfiDispatcher::dispatch`
  fires `on_request` as a non-blocking enqueue and awaits the response head on
  a tokio task. A handler doing `await fetch(self)` is structurally identical
  to two concurrent peers, which is already supported. No JS thread is blocked
  and no lock is held across an await.
- *"The peer identity would be wrong/spoofable."* Routing the request with the
  node's own id as `RemoteNodeId` injects `peer-id == own key`, which is
  truthful (the peer really is us) and unforgeable by a remote.

With the blockers gone, the elegant fix is to route self-targeted fetches into
the node's own serve service in-process, rather than attempting a network dial
that the transport refuses.

## Questions

1. Where should the self-detection and routing live, given the
   `mod http` ⇏ `mod ffi` architecture boundary (ADR-014 / `tests/architecture.rs`)?
2. How do we hand a self-request to the *same* service remote peers reach,
   without forking the request-handling path?
3. What are the semantic differences between a loopback self-request and a
   real network round-trip, and how do we keep them honest?
4. Does storing the service on the endpoint introduce a reference-cycle leak?

## What we know

- `IrohHttpService` (the FFI-shaped `tower::Service`) is `#[derive(Clone)]` and
  cheaply cloneable (it wraps `Arc<FfiDispatcher>`).
- The FFI `fetch` entry lives in `mod ffi` (`ffi/fetch.rs`); the pure-Rust
  `fetch_request` lives in `mod http`. `mod http` may not import `mod ffi`
  types, but `mod ffi` may import both. So the self-route must live in
  `ffi/fetch.rs`, not in `http/client.rs`.
- `serve_with_events` already hands the accept loop a clone of the same
  `IrohHttpService`; while serving, that running task already holds a strong
  reference to the endpoint (via the dispatcher's `endpoint` clone). Serving
  therefore already keeps the endpoint alive until `close()` / `stop_serve()`.
- `node_id()` and the fetch path's `remote_str` are both the canonical base32
  of the same public-key bytes, so `remote_str == endpoint.node_id()` is an
  exact, allocation-free self-check.

## Options considered

| Option | Upside | Downside |
| ------ | ------ | -------- |
| Won't-fix + userland recipe (`withSelfRouting` wrapper) | Zero core change | Every adapter reimplements it; Tauri's `httpi://` scheme handler (native Rust) can't be wrapped, so `<audio src="httpi://self">` stays broken |
| Patch iroh to allow self-dial | "Real" network path, full server stack | Upstream-owned, slow, and pointless — a self QUIC handshake to localhost is pure overhead |
| In-process loopback into the node's own service (chosen) | One handling path, fixes Tauri for free, no wire overhead | Bypasses the per-connection server stack (documented below); requires a stored service handle |

## Decisions

`iroh_http_core::fetch` short-circuits self-targeted requests into the node's
own serve service, in-process:

1. When `serve()` starts (`ffi_serve_with_callback`), the constructed
   `IrohHttpService` is registered on the endpoint via `set_local_service`.
2. In `ffi/fetch.rs::fetch`, after the typed `Request<Body>` is built, if
   `remote_str == endpoint.node_id()` the request is handed to `self_fetch`,
   which fetches the registered service, injects `RemoteNodeId(own)` as a
   request extension (the same identity the per-connection `AddExtensionLayer`
   would inject on the wire), and calls `svc.oneshot(req)`. Cancellation and
   `timeout` mirror the QUIC path; the response is packaged identically via
   `package_response`.
3. If no server is registered, `self_fetch` returns a clear, actionable error
   ("this node has no active server to handle a request to its own node id —
   call serve() first") instead of the opaque upstream self-dial refusal.
4. The slot is cleared on `close()`, `close_force()`, and `stop_serve()`.

This is a **loopback, not a network path**. It is the same decision shape as
`127.0.0.1` for IP: addressing yourself resolves to in-process delivery.

## Consequences

- **Fixes all adapters at once.** Node, Deno, and Tauri call `core::fetch`, so
  `<audio src="httpi://<own-id>/...">` works in Tauri with no adapter code.
- **One handling path.** Self-requests reach the exact `IrohHttpService` remote
  peers reach — no parallel "self" handler to drift out of sync.
- **Bypassed layers (documented, intentional).** A self-request skips the wire:
  no QUIC/TLS handshake, no compression negotiation, and none of the
  per-connection server stack applied at the accept loop (connection-level
  timeouts, body limits). A self-request is therefore **not** a
  network-reachability check. The request-level `timeout` and cancellation
  passed to `fetch` are still honored. The `decompress` / compression knob is
  a no-op on this self-loopback path because compression is wire-only.
- **Authenticated identity is truthful.** The handler observes `peer-id ==
  own node id`. It is not produced by a cryptographic handshake (there is no
  remote to authenticate), but it cannot be spoofed by a third party.
- **Reference cycle is bounded.** Storing the service on the endpoint creates a
  strong cycle (endpoint → `FfiBridge` → service → dispatcher → endpoint). This
  has the *same lifetime* as the already-running accept loop's own service
  clone, and is cleared in the same teardown paths (`close` / `close_force` /
  `stop_serve`). No leak beyond what an active `serve()` already implies.

## Next steps

- [x] Store `IrohHttpService` on the endpoint; register on serve, clear on teardown.
- [x] Self-route in `ffi/fetch.rs` with `self_fetch` helper.
- [x] Rust core regression test: serve + self-fetch on a single endpoint, plus
      the no-server and post-stop error paths (`tests/http_self_fetch.rs`).
- [x] FFI boundary test across Node / Deno adapters (`tests/suites/self-fetch.mjs`).
- [ ] Manual verification in Tauri (`<audio src="httpi://<own-id>/...">`).
- [x] Update `docs/specification.md` to document the first-class self-request path.
