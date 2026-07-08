# iroh-http

[![CI](https://github.com/momics/iroh-http/actions/workflows/ci.yml/badge.svg)](https://github.com/momics/iroh-http/actions/workflows/ci.yml)
[![Coverage](https://github.com/momics/iroh-http/actions/workflows/coverage.yml/badge.svg)](https://github.com/momics/iroh-http/actions/workflows/coverage.yml)
[![npm](https://img.shields.io/npm/v/@momics/iroh-http-node)](https://www.npmjs.com/package/@momics/iroh-http-node)
[![JSR](https://jsr.io/badges/@momics/iroh-http-deno)](https://jsr.io/@momics/iroh-http-deno)
[![crates.io](https://img.shields.io/crates/v/iroh-http-core)](https://crates.io/crates/iroh-http-core)

> Pre-v1.0. APIs may change between minor releases.

HTTP over [Iroh](https://iroh.computer) QUIC. Two devices that know each other's Ed25519 public key can exchange HTTP requests peer-to-peer, through NATs, without servers or DNS. Iroh handles the hard QUIC parts (NAT traversal, relay fallback). This library runs HTTP/1.1 on top of it via [hyper](https://hyper.rs).

The API is `fetch()` and `serve()`, same signatures as WHATWG Fetch and Deno.serve. Handlers, routers, and middleware you've already written for Deno, Hono, or anything `fetch`-shaped work without changes.

```ts
import { createNode } from "@momics/iroh-http-node"; // or -deno, -tauri

const node = await createNode();

node.serve({}, (req) => {
  if (req.headers.get("Peer-Id") !== ALLOWED_PEER)
    return new Response("Forbidden", { status: 403 });
  return new Response("hello");
});

const res = await node.fetch("httpi://<peer-public-key>/");
```

### What this is not

- **Not a browser library.** Requires raw UDP (Node.js, Deno, or Tauri desktop).
- **Not a proxy for public HTTP.** Peers are addressed by key, not hostname.
- **Not libp2p or WebRTC.** Single transport (QUIC), single framing (HTTP/1.1), no negotiation.

## Install

| Runtime  | Install                               | Package README |
| -------- | ------------------------------------- | -------------- |
| Node.js  | `npm install @momics/iroh-http-node`  | [iroh-http-node](packages/iroh-http-node/) |
| Deno     | `deno add jsr:@momics/iroh-http-deno` | [iroh-http-deno](packages/iroh-http-deno/) |
| Tauri v2 | `npm install @momics/iroh-http-tauri` | [iroh-http-tauri](packages/iroh-http-tauri/) |

The API is identical across runtimes. Only the import path changes.

## What's built in

The server stack is composed from [tower-http](https://docs.rs/tower-http) layers. You don't configure them individually; they're wired by default with safe limits:

| Concern | How | Default |
|---------|-----|---------|
| **Compression** | zstd via tower-http (request decompression + response compression) | Level 3, bodies > 1 KiB |
| **Timeouts** | Per-request timeout layer | 60 s |
| **Concurrency** | Semaphore-gated request limit | 1024 concurrent |
| **Load shedding** | 503 when overloaded | Automatic |
| **Connection pool** | moka cache with single-flight connect | Per-peer reuse |
| **Per-peer limits** | Max connections per remote node | 8 |
| **Peer identity** | `Peer-Id` header injected from QUIC handshake (tamper-proof) | Always on |

Override any of these via `ServeOptions` and `NodeOptions`. See [docs/tuning.md](docs/tuning.md).

### Where the library draws the line

iroh-http provides transport and reliability. It does not include routing, authentication schemes, session management, or application-level middleware. Use whatever you'd use with `fetch`-based servers: Hono, itty-router, or plain `if` statements.

The library deliberately avoids imposing policy you should own. The one place it ships a default *limit* is the Tauri `httpi://` scheme handler: because Tauri's custom-protocol responder takes a complete body (not a stream), the handler must buffer each response in memory before handing it to the webview, so it caps a single buffered response at **64 MiB** by default to prevent a hostile peer from exhausting app memory. This is per-response, not per-file — media streams via small `Range` requests regardless of total size — and is overridable with `.max_response_bytes(...)` on the plugin builder. See [iroh-http-tauri](packages/iroh-http-tauri/README.md#response-size-limit).

## Beyond HTTP

### QUIC sessions

Direct QUIC connections with bidirectional streams, unidirectional streams, and datagrams. API mirrors [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport).

```ts
const session = await node.dial("<peer-public-key>");
const { readable, writable } = await session.createBidirectionalStream();
```

### mDNS discovery

Find peers on the local network without exchanging keys out-of-band.

```ts
await node.advertise("my-app.iroh-http");
for await (const event of node.browse("my-app.iroh-http")) {
  if (event.type === "discovered")
    await node.fetch(`httpi://${event.nodeId}/api`);
}
```

### Crypto

Ed25519 key generation, signing, and verification, both standalone and on a live node.

```ts
import { generateSecretKey, secretKeySign, publicKeyVerify } from "@momics/iroh-http-node";
const sk  = generateSecretKey();
const sig = secretKeySign(sk, data);
const ok  = publicKeyVerify(publicKey, data, sig);
```

Full API details for sessions, crypto, and mDNS: [docs/api-overview.md](docs/api-overview.md).

## Architecture

| Package | What it does |
|---------|--------------|
| [`iroh-http-core`](crates/iroh-http-core/) | Rust. QUIC transport, HTTP/1.1 framing, connection pool, server stack. |
| [`iroh-http-discovery`](crates/iroh-http-discovery/) | Rust. Standard DNS-SD (mDNS) peer discovery for the local network. |
| [`iroh-http-adapter`](crates/iroh-http-adapter/) | Rust. Shared FFI error envelope for all adapters. |
| [`iroh-http-shared`](packages/iroh-http-shared/) | TypeScript. `IrohNode` class, stream helpers, error hierarchy. No native deps. |
| [`iroh-http-node`](packages/iroh-http-node/) | Node.js native addon (napi-rs). |
| [`iroh-http-deno`](packages/iroh-http-deno/) | Deno FFI via `dlopen`. |
| [`iroh-http-tauri`](packages/iroh-http-tauri/) | Tauri v2 plugin with capability-based permissions. |

See [docs/architecture.md](docs/architecture.md) for the full layer diagram.

## Development

```sh
npm install
npm run check    # cargo check + tsc
npm run lint     # cargo fmt --check + clippy
npm run build    # build everything
npm run test     # test everything
```

Per-target:
```sh
npm run build:core    npm run build:node    npm run build:deno    npm run build:tauri
npm run test:rust     npm run test:node     npm run test:deno     npm run test:interop
```

### Coverage

The [Coverage workflow](.github/workflows/coverage.yml) runs [`cargo llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) over the Rust workspace on every push and PR. It is **non-blocking** — a baseline report only, never a required check — and publishes an lcov + text summary to the job summary and a `coverage-report` artifact. Reproduce locally with:
```sh
cargo install cargo-llvm-cov
cargo llvm-cov --workspace --summary-only
```

## Acknowledgements

Built on [Iroh](https://iroh.computer) by [n0](https://n0.computer).

## License

Apache-2.0 or MIT. See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
