# @momics/iroh-http-deno

[![JSR](https://jsr.io/badges/@momics/iroh-http-deno)](https://jsr.io/@momics/iroh-http-deno)

> Pre-v1.0. APIs may change between minor releases.

Deno adapter for [iroh-http](https://github.com/momics/iroh-http). Uses Deno FFI
(`dlopen`). No child processes, no WASM.

## Install

```sh
deno add jsr:@momics/iroh-http-deno
```

Or import directly:

```ts
import { createNode } from "jsr:@momics/iroh-http-deno";
```

Pre-built native libraries for macOS (x64, ARM), Linux (x64, ARM), and Windows
(x64). Build from source for other platforms:

```sh
cd packages/iroh-http-deno && deno task build
```

## Quick start

```ts
import { createNode } from "jsr:@momics/iroh-http-deno";

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

## Deno specifics

- Requires `--unstable-ffi` and `--allow-ffi` flags (Deno FFI is unstable).
- Standalone crypto functions (`generateSecretKey`, `secretKeySign`,
  `publicKeyVerify`) are **async**. They round-trip through Rust via FFI.
- Serve uses a pull model internally: JS polls for incoming requests via
  `nextRequest()` rather than receiving push callbacks.

## Permissions

Deno's permission model applies on top of iroh-http. You'll need at minimum:

```sh
deno run --unstable-ffi --allow-ffi --allow-net --allow-env your-app.ts
```

## Other runtimes

| Runtime  | Package                                                                            |
| -------- | ---------------------------------------------------------------------------------- |
| Node.js  | [`@momics/iroh-http-node`](https://www.npmjs.com/package/@momics/iroh-http-node)   |
| Tauri v2 | [`@momics/iroh-http-tauri`](https://www.npmjs.com/package/@momics/iroh-http-tauri) |

## License

MIT OR Apache-2.0

MIT OR Apache-2.0
