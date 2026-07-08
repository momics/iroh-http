# @momics/iroh-http-tauri

[![npm](https://img.shields.io/npm/v/@momics/iroh-http-tauri)](https://www.npmjs.com/package/@momics/iroh-http-tauri)

> Pre-v1.0. APIs may change between minor releases.

Tauri v2 plugin for [iroh-http](https://github.com/momics/iroh-http). Runs as a
Rust plugin with capability-based permissions. Your frontend JS only gets the
network access you grant.

## Install

**Frontend:**

```sh
npm install @momics/iroh-http-tauri
```

**Rust plugin** in `src-tauri/Cargo.toml`:

```toml
[dependencies]
tauri-plugin-iroh-http = "0.3"
```

**Register** in `src-tauri/src/lib.rs`:

```rust
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_iroh_http::init())
        .run(tauri::generate_context!())
        .unwrap();
}
```

To enable native `httpi://` URL resolution in the webview (see
[below](#httpi-scheme-handler)):

```rust
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_iroh_http::builder().with_scheme().build())
        .run(tauri::generate_context!())
        .unwrap();
}
```

### iOS

The plugin's network-interface enumeration links Apple's `SystemConfiguration`
framework, which iOS does not link automatically. Add it to your app's
`bundle.iOS.frameworks` and recreate the iOS project:

```jsonc
// src-tauri/tauri.conf.json
{
  "bundle": {
    "iOS": {
      "frameworks": ["SystemConfiguration"]
    }
  }
}
```

```sh
npm run tauri ios init   # regenerate the Xcode project so the framework applies
```

Without this, the Xcode link step fails with missing `_kSCNetwork*` /
`_kSCProp*` symbols.

If you use mDNS discovery (`node.browse()` / `node.advertise()`), iOS also gates
local-network access behind a user permission. Declare it and every Bonjour
service type you use in `src-tauri/Info.ios.plist`, which Tauri merges into the
generated iOS `Info.plist`:

```xml
<!-- src-tauri/Info.ios.plist -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Discover and connect to nearby peers on your local network.</string>
  <key>NSBonjourServices</key>
  <array>
    <!-- "_<serviceName>._udp"; the default serviceName is "iroh-http" -->
    <string>_iroh-http._udp</string>
  </array>
</dict>
</plist>
```

Without these, `NWBrowser` is denied with `NWError -65555 (NoAuth)` and browsing
silently restarts. Each custom `serviceName` needs its own `_<name>._udp` entry.

## Quick start

```ts
import { createNode } from "@momics/iroh-http-tauri";

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
```

## Full API

The API is identical across Node.js, Deno, and Tauri: HTTP fetch/serve, QUIC
sessions, mDNS discovery, and Ed25519 crypto. See the
[API overview](../../docs/api-overview.md) for the complete reference.

## Permissions

Tauri's capability system controls what the frontend can access. Declare
permissions in `capabilities/default.json`:

| Permission          | Covers                                        |
| ------------------- | --------------------------------------------- |
| `iroh-http:default` | `createNode()`, `close()`, node introspection |
| `iroh-http:fetch`   | `node.fetch()` + body streaming               |
| `iroh-http:serve`   | `node.serve()` + body streaming               |
| `iroh-http:connect` | Raw QUIC sessions (bidi streams, datagrams)   |
| `iroh-http:mdns`    | mDNS peer discovery                           |
| `iroh-http:crypto`  | Key generation, signing, verification         |

A typical app using fetch and serve:

```json
{
  "permissions": [
    "iroh-http:default",
    "iroh-http:fetch",
    "iroh-http:serve"
  ]
}
```

## `httpi://` scheme handler

Call `.with_scheme()` on the plugin builder to register `httpi://` as a native
URI scheme in the webview. Once an endpoint is created, standard browser APIs
resolve `httpi://` URLs directly through iroh-http-core — no JavaScript bridging
required.

```ts
// After createNode(), these all just work:
const res = await fetch("httpi://<peer-id>/path");
document.querySelector("img").src = "httpi://<peer-id>/photo.jpg";
document.querySelector("audio").src = "httpi://<peer-id>/track.flac"; // seeking supported
```

The handler auto-binds to the first endpoint created. There is nothing else to
configure.

> **GET only:** The scheme handler resolves GET requests. Non-GET callers
> receive `405 Method Not Allowed`. Use `node.fetch()` for POST, PUT, DELETE.

> **Platform note:** On macOS, Linux, and iOS the origin is
> `httpi://<nodeid>/path`. On Windows and Android, Tauri rewrites the origin to
> `http://httpi.localhost/path` — the handler accounts for this automatically.

### Response size limit

Tauri's custom-protocol responder takes a **complete body, not a stream**, so
the scheme handler must buffer each `httpi://` response fully in memory before
handing it to the webview. To stop a large or hostile peer from exhausting app
memory, the handler caps a single buffered response at **64 MiB by default**
(`DEFAULT_SCHEME_MAX_RESPONSE_BYTES`).

This cap is intentionally **per response, not per file**: media plays back
through many small `Range` requests, each well under the cap, so videos and
audio of any size stream fine as long as the serving peer supports `Range` (it
does by default). The limit only rejects a single _non-ranged_ response larger
than the cap.

Override it on the builder when you knowingly serve large non-ranged assets:

```rust
tauri::Builder::default()
    .plugin(
        tauri_plugin_iroh_http::builder()
            .with_scheme()
            .max_response_bytes(256 * 1024 * 1024) // 256 MiB
            .build(),
    );
```

The cap applies **only** to the `httpi://` scheme handler. The `node.fetch()`
IPC path streams the body and is bounded by your own consuming code, not by this
value.

## Tauri specifics

- Serve callbacks are delivered to the frontend via Tauri `Channel` events (push
  model).
- All crypto functions are async (round-trip through the Rust plugin via Tauri
  invoke).
- QUIC sessions require the `iroh-http:connect` permission.
- mDNS requires the `iroh-http:mdns` permission.
- The `httpi://` scheme handler (opt-in via `.with_scheme()`) enables native URL
  resolution without IPC overhead.

## Supported platforms

| Platform |      Architecture       | Status |
| -------- | :---------------------: | :----: |
| macOS    |         x86_64          |   ✅   |
| macOS    | aarch64 (Apple Silicon) |   ✅   |
| Linux    |         x86_64          |   ✅   |
| Linux    |         aarch64         |   ✅   |
| Windows  |         x86_64          |   ✅   |

## Other runtimes

| Runtime | Package                                                                          |
| ------- | -------------------------------------------------------------------------------- |
| Node.js | [`@momics/iroh-http-node`](https://www.npmjs.com/package/@momics/iroh-http-node) |
| Deno    | [`@momics/iroh-http-deno`](https://jsr.io/@momics/iroh-http-deno)                |

## License

MIT OR Apache-2.0
