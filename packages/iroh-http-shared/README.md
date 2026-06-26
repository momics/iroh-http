# @momics/iroh-http-shared

> Pre-v1.0. APIs may change between minor releases.

Shared TypeScript layer for [iroh-http](https://github.com/momics/iroh-http).
Pure TypeScript, no native dependencies. This is a transitive dependency of the
platform adapters (Node.js, Deno, Tauri) and is not intended for direct import.

## What's inside

- **`IrohNode`**: the node class exposed by all adapters (`fetch()`, `serve()`,
  `connect()`, `sessions()`, `browse()`, `advertise()`)
- **`IrohAdapter` / `Bridge`**: the interface each platform adapter implements
  (`nextChunk`, `sendChunk`, `finishBody`, raw FFI fetch/serve)
- **`IrohSession`**: a WebTransport-compatible QUIC session with bidirectional
  streams, unidirectional streams, and datagrams
- **`makeReadable()`**: wraps a Rust body handle in a `ReadableStream`
- **`pipeToWriter()`**: drains a `ReadableStream` into a Rust body handle
- **`makeFetch()`**: wraps raw FFI fetch in web-standard `fetch()` signature
- **`makeServe()`**: wraps raw FFI serve in Deno-style `serve()` signature
- **`PublicKey` / `SecretKey`**: key classes with base32 encoding, async
  sign/verify
- **`IrohError`**: structured error hierarchy with error codes

## License

MIT OR Apache-2.0
