# iroh-http-adapter

Shared utilities for the iroh-http adapter crates (Deno, Node.js, Tauri).

> **This is an internal implementation detail.** Application developers should use the platform adapters directly:
> - **Node.js** → [`@momics/iroh-http-node`](https://www.npmjs.com/package/@momics/iroh-http-node)
> - **Deno** → [`@momics/iroh-http-deno`](https://jsr.io/@momics/iroh-http-deno)
> - **Tauri** → [`@momics/iroh-http-tauri`](https://www.npmjs.com/package/@momics/iroh-http-tauri)

## What this crate is

This crate is the deliberately narrow seam shared by the native adapters. It
owns fail-closed input validation and coercion, request-delivery helpers, and a
common JSON error envelope at the FFI boundary:

```json
{"code": "REFUSED", "message": "connection refused"}
```

Keeping these rules here prevents Node, Deno, and Tauri from interpreting the
same request differently. HTTP transport and endpoint lifecycle still belong to
`iroh-http-core`; platform glue remains in each adapter.

## What this crate is not

- It does not expose any FFI symbols itself.
- It does not contain HTTP transport logic — that lives in `iroh-http-core`.
- It does not contain platform-specific code — each adapter crate handles that.

## Usage

```toml
[dependencies]
iroh-http-adapter = { path = "../../crates/iroh-http-adapter" }
```

```rust
use iroh_http_adapter::{core_error_to_json, format_error_json};

// From a CoreError:
let json = core_error_to_json(&e);
// → {"code":"REFUSED","message":"..."}

// With an explicit code:
let json = format_error_json("INVALID_HANDLE", "endpoint not found");
// → {"code":"INVALID_HANDLE","message":"endpoint not found"}
```

## Error codes

| `ErrorCode` variant | JSON `code` |
|---|---|
| `InvalidInput` | `INVALID_INPUT` |
| `ConnectionFailed` | `REFUSED` |
| `Timeout` | `TIMEOUT` |
| `BodyTooLarge` | `BODY_TOO_LARGE` |
| `HeaderTooLarge` | `HEADER_TOO_LARGE` |
| `PeerRejected` | `PEER_REJECTED` |
| `Cancelled` | `CANCELLED` |
| `Internal` | `INTERNAL` |
| _(future variants)_ | `UNKNOWN` |

## License

`MIT OR Apache-2.0`
