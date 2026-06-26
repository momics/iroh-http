# @momics/iroh-http-node

[![npm](https://img.shields.io/npm/v/@momics/iroh-http-node)](https://www.npmjs.com/package/@momics/iroh-http-node)

> Pre-v1.0. APIs may change between minor releases.

Node.js adapter for [iroh-http](https://github.com/momics/iroh-http). Native
addon via napi-rs. No child processes, no WASM.

## Install

```sh
npm install @momics/iroh-http-node
```

Pre-built binaries are published for macOS (x64, ARM), Linux glibc (x64, ARM),
and Windows (x64). Other platforms need a source build:

```sh
npx napi build --platform --release
```

## Quick start

```ts
import { createNode } from "@momics/iroh-http-node";

const node = await createNode();
console.log("Node ID:", node.publicKey.toString());

node.serve({}, (req) => {
  if (req.headers.get("Peer-Id") !== ALLOWED_PEER) {
    return new Response("Forbidden", { status: 403 });
  }
  return new Response("hello");
});

const res = await node.fetch("httpi://<peer-public-key>/");
console.log(await res.text());
await node.close();
```

## Full API

The API is identical across Node.js, Deno, and Tauri: HTTP fetch/serve, QUIC
sessions, mDNS discovery, and Ed25519 crypto. See the
[API overview](../../docs/api-overview.md) for the complete reference.

## Node.js specifics

- Standalone crypto functions (`generateSecretKey`, `secretKeySign`,
  `publicKeyVerify`) are **synchronous**. They run in Rust on the calling thread
  without a Tokio round-trip.
- The class API on a live node (`node.secretKey.sign()`,
  `node.publicKey.verify()`) is async.
- Serve callbacks run on the Node.js event loop via napi-rs
  `ThreadsafeFunction`.

## Other runtimes

| Runtime  | Package                                                                            |
| -------- | ---------------------------------------------------------------------------------- |
| Deno     | [`@momics/iroh-http-deno`](https://jsr.io/@momics/iroh-http-deno)                  |
| Tauri v2 | [`@momics/iroh-http-tauri`](https://www.npmjs.com/package/@momics/iroh-http-tauri) |

## License

MIT OR Apache-2.0
