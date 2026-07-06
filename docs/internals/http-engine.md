# HTTP Engine Internals

This document covers how iroh-http-core uses hyper v1 and tower. It is aimed at contributors who need to understand the request/response lifecycle, the service layer, or the body channel bridge.

---

## `IrohStream` — the IO adapter

hyper expects an IO object implementing its `hyper::rt::Read` + `hyper::rt::Write` traits. Iroh provides `SendStream` and `RecvStream` which implement `tokio::io::AsyncWrite` and `tokio::io::AsyncRead` respectively.

`IrohStream` (`src/io.rs`) pairs them and wraps the pair in `hyper_util::rt::TokioIo` to bridge between tokio's async IO traits and hyper's IO traits:

```
iroh::SendStream  ─┐
                   ├─► IrohStream (AsyncRead + AsyncWrite) ─► TokioIo ─► hyper
iroh::RecvStream  ─┘
```

`poll_shutdown` on `IrohStream` calls `SendStream::finish()` which sends a FIN on the QUIC stream. hyper calls `poll_shutdown` when the response is complete — the remote side depends on this FIN to know the exchange is done.

---

## Server: `RequestService`

The server accept loop (`serve()` in `server.rs`) builds a base `RequestService` and clones it once per QUIC connection, injecting the peer's `remote_node_id`:

```rust
let mut peer_svc = base_svc.clone();
peer_svc.remote_node_id = Some(remote_id);
```

For each incoming QUIC bi-stream (= one HTTP request), a permit from the drain semaphore is acquired and hyper serves the connection:

```rust
hyper::server::conn::http1::Builder::new()
    .max_buf_size(max_header_size.max(8192))
    .max_headers(128)
    .serve_connection(TokioIo::new(io), hyper_svc)
    .with_upgrades()
    .await
```

`RequestService` implements `tower::Service<Request<Incoming>>`. Its `call` method drives the entire request lifecycle.

### Request lifecycle inside `RequestService::call`

```
1. Extract method, path, headers, remote_node_id
2. Detect duplex: check for "Upgrade: iroh-duplex" header
3. Capture upgrade future (for duplex only): hyper::upgrade::on(&mut req)
4. Allocate channels:
     req_body_writer, req_body_reader  ← make_body_channel()
     res_body_writer, res_body_reader  ← make_body_channel()
5. Store handles in global slotmap registries:
     req_body_handle  = insert_reader(ep_idx, req_body_reader)
     res_body_handle  = insert_writer(ep_idx, res_body_writer)
6. Allocate response-head rendezvous:
     (head_tx, head_rx) = oneshot::channel::<ResponseHeadEntry>()
     req_handle = allocate_req_handle(ep_idx, head_tx)
7. Pump request body:
     Non-duplex: spawn pump_hyper_body_to_channel_limited(body, req_body_writer, limit)
     Duplex: retain req_body_writer for the upgrade spawn
8. Fire on_request callback with all handles → JS handler runs
9. Await head_rx — parks here until JS calls respond(req_handle, status, headers)
10. Non-duplex path:
     Build hyper Response with status/headers from head
     Body = body_from_reader(res_body_reader)
     Return Ok(response) → hyper sends it
11. Duplex path:
     Return 101 Switching Protocols immediately
     Spawn upgrade task: wire recv_io → req_body_writer AND res_body_reader → send_io
```

### Response-head rendezvous

`req_handle` is a `u64` slotmap key pointing to `head_tx: oneshot::Sender<ResponseHeadEntry>`. When JS calls `respond(req_handle, status, headers)`:

1. `take_req_sender(req_handle)` removes and returns the sender
2. Sender fires with `ResponseHeadEntry { status, headers }`
3. `head_rx.await` in `call` resolves
4. hyper gets its `Response<BoxBody>` and sends it on the wire

hyper is oblivious to this — it just sees an async service that takes some time to produce a response.

---

## Body channel bridge

iroh-http-core uses `BodyWriter`/`BodyReader` channels (mpsc-backed, defined in `stream.rs`) as the FFI boundary for streaming bodies. hyper works with `http_body::Body` types. Two pump functions bridge between them.

### Hyper → channel: `pump_hyper_body_to_channel_limited`

Drains a hyper `Incoming` body frame-by-frame:

- `Frame::data(bytes)` → `BodyWriter::send_chunk(bytes)`
- Body size counted per-chunk; returns `Err(BodyTooLarge)` if limit exceeded
- Dropping `BodyWriter` signals EOF on the `BodyReader` side (JS's `nextChunk` returns `null`)

### Channel → hyper: `body_from_reader`

Produces an `http_body::Body` (via `StreamBody`) from a `BodyReader`:

- Yields `Frame::data` for each chunk from the reader
- Used both for response bodies (server side) and request bodies (client side)

---

## Server-side duplex upgrade

Duplex mode uses HTTP Upgrade semantics. The ALPN for duplex connections is `iroh-http/2-duplex`.

**Server side** (the only side this engine implements; the client side is the user's responsibility via the session API):
1. Detects `Upgrade: iroh-duplex` header
2. Captures upgrade future before consuming request: `hyper::upgrade::on(&mut req)`
3. Returns `101 Switching Protocols` to hyper immediately
4. In a spawned task, awaits upgrade resolution, then runs two concurrent pumps:
   - `recv_io → req_body_writer`: bytes from the client feed into `req_body_handle` (JS reads via `nextChunk`)
   - `res_body_reader → send_io`: bytes JS writes via `sendChunk` are forwarded to the client

*Client-side initiation of an upgraded HTTP/1.1 stream lived in `client::raw_connect` until v0.3.5; it was removed because the [session API](../../crates/iroh-http-core/src/session.rs) covers the same use case with a cleaner WebTransport-shaped surface and the JS adapters never wired it up.*

---

## Resource limits

| Limit | Configured via | Enforcement |
|-------|---------------|-------------|
| Max header size | `NodeOptions::max_header_size` (default 64 KB) | `max_buf_size(limit.max(8192))` on hyper builder + post-parse byte-count check |
| Max request body | `ServeOptions::max_request_body_bytes` | Byte counter in `pump_hyper_body_to_channel_limited` |
| Max concurrent requests | `ServeOptions::max_concurrency` (default 1024) | Drain semaphore: one permit per in-flight bi-stream |
| Per-request timeout | `ServeOptions::request_timeout_ms` (default 60 s) | `TimeoutService` wrapping `RequestService` |
| Max connections per peer | `ServeOptions::max_connections_per_peer` (default 8) | `PeerConnectionGuard` + `DashMap` counter |

> `max_buf_size` on hyper's builder **panics** for values below 8192. Always clamp: `limit.max(8192)`. The actual limit is then enforced by counting bytes across the parsed headers after hyper returns them.

---

## Compression (optional feature)

When the `compression` feature is enabled, `tower-http`'s `CompressionLayer` is inserted into the server's `ServiceBuilder` chain:

```rust
TowerToHyperService::new(
    ServiceBuilder::new()
        .layer(CompressionLayer::new().zstd(true).compress_when(SizeAbove::new(512)))
        .service(TimeoutService::new(svc, timeout_dur)),
)
```

Policy: **zstd only, responses ≥ 512 bytes only**. The client sends `Accept-Encoding: zstd` (automatically added by iroh-http-core) and decompresses responses inline.

`TowerToHyperService` (from `hyper-util`, requires the `service` feature) bridges the tower `Service` type to hyper's connection handler.
