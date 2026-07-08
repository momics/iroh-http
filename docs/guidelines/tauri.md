# Tauri Guidelines

Applies to: `iroh-http-tauri` (Rust plugin + guest JS).

For engineering values and invariants, see [principles.md](../principles.md).
For JS/TS conventions on the guest side, see [javascript.md](javascript.md).
For local-network discovery on devices, see
[Mobile mDNS / DNS-SD setup](mobile-mdns-setup.md).

---

## Architecture

```
Guest JS (webview)
  └── @momics/iroh-http-shared  (buildNode, makeFetch, makeServe)
        └── Tauri invoke / Channel
              └── iroh-http-tauri plugin (Rust)
                    └── iroh-http-core
```

The guest JS implements the `Bridge` interface using Tauri invoke calls. The result is the same `IrohNode` interface Node and Deno users get — no Tauri-specific surface in the user-facing API.

---

## Rust Plugin Conventions

**Command naming:** `snake_case` in Rust, registered via `tauri::generate_handler![]`, invoked as `plugin:iroh-http|command_name` on the JS side.

**Serde:** command args use `#[derive(Deserialize)]` with `#[serde(rename_all = "camelCase")]`. Return types use `#[derive(Serialize)]` with the same rename. The Rust struct uses `endpoint_handle`; JS sends/receives `endpointHandle`.

**Errors:** all commands return `Result<T, String>`. Errors are stringified before crossing the invoke boundary. The guest JS wraps them in the shared error classification system (`classifyError`).

**Binary data:** body chunks cross the invoke boundary as raw binary using `tauri::ipc::Response` (Rust → JS) and `tauri::ipc::Request` (JS → Rust). The `next_chunk` / `try_next_chunk` commands return `ArrayBuffer` directly; `send_chunk` accepts raw `Uint8Array` via IPC headers for metadata. Code above the bridge never sees encoding details.

---

## Guest JS Conventions

The guest JS implements `Bridge` by calling `invoke`. All base64 conversion happens at the bridge boundary — nothing above sees it.

**Serve callback:** uses a `Channel<RequestPayload>` rather than `ThreadsafeFunction` (Node) or JSON dispatch (Deno). The Rust `serve` command accepts the channel; each incoming request is pushed through it; the guest receives it, runs the handler, and calls back with the response.

**Naming:** follows the [JavaScript guidelines](javascript.md) — `camelCase` functions, `PascalCase` types, WHATWG standard types for `Request`/`Response`.

---

## Permissions

Follow Tauri's permission naming: `allow-<command-name>` in kebab-case. The default permission set grants all plugin commands. Do not leave commands unprotected by relying on ambient app permissions.

---

## What Not To Do

- Don't add Tauri-specific types to the user-facing `IrohNode` API. The `buildNode` factory produces an identical interface across all platforms.
- Don't expose invoke command names or Tauri internals in error messages shown to users.
- Don't duplicate logic that belongs in `@momics/iroh-http-shared`. If it works for all JS platforms, it goes in shared.

---

## Testing

- **Rust commands:** test through `iroh-http-core` integration tests. Tauri commands are thin wrappers.
- **Guest JS:** test through the `IrohNode` surface. The `Bridge` is substitutable — mock tests can replace invoke calls.
- **End-to-end:** `tauri-driver` or manual webview testing. Reserve for release validation.

---

## Endpoint lifecycle and hot-reload

The plugin registers a `WindowEvent::Destroyed` handler that calls
`iroh_http_core::registry::close_all_endpoints()`. This force-closes every
open endpoint when the webview is destroyed, preventing QUIC socket leaks
during Vite HMR hot-reload.

For well-behaved apps, still call `closeEndpoint(handle)` explicitly before
your frontend framework teardown (e.g. `onBeforeUnmount` / `beforeunload`).
This allows a graceful drain instead of a force-close:

```ts
// Vue / React teardown
onBeforeUnmount(async () => {
  await node.close();
});
```

The `WindowEvent::Destroyed` handler is the safety net for the case where the
page reloads before teardown code runs (common in Vite dev mode).
