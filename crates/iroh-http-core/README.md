# iroh-http-core

Rust core for [iroh-http](https://github.com/momics/iroh-http). Runs HTTP/1.1 over [Iroh](https://iroh.computer) QUIC bidirectional streams via [hyper](https://hyper.rs). Nodes are addressed by Ed25519 public key.

This is the runtime-independent Rust API and transport engine. JavaScript
application developers should normally use a higher-level adapter:
> - **Node.js** → [`@momics/iroh-http-node`](https://www.npmjs.com/package/@momics/iroh-http-node)
> - **Deno** → [`@momics/iroh-http-deno`](https://jsr.io/@momics/iroh-http-deno)
> - **Tauri** → [`@momics/iroh-http-tauri`](https://www.npmjs.com/package/@momics/iroh-http-tauri)

## Usage

```rust
use std::convert::Infallible;

use iroh_http_core::{Body, IrohEndpoint, NodeOptions, ServeOptions, serve};
use tower::service_fn;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Bind a local endpoint
    let endpoint = IrohEndpoint::bind(NodeOptions::default()).await?;
    println!("Node ID: {}", endpoint.node_id());

    // Serve standard hyper requests through a standard tower Service.
    let handle = serve(
        endpoint.clone(),
        ServeOptions::default(),
        service_fn(|request| async move {
            let body = format!("hello from {}", request.uri().path());
            Ok::<_, Infallible>(hyper::Response::new(Body::full(body)))
        }),
    );

    // An application normally keeps the handle alive, then closes it during
    // shutdown. `drain()` stops and awaits the serve task.
    handle.drain().await;

    Ok(())
}
```

## Features

- **Connection reuse**: QUIC connections to the same peer are pooled and multiplexed
- **Streaming bodies**: request and response bodies stream through `mpsc` channels with configurable backpressure
- **Fetch cancellation**: abort in-flight requests via cancellation tokens
- **Bidirectional streams**: full-duplex streaming via QUIC bidi streams
- **Trailer support**: HTTP/1.1 chunked trailers for streaming metadata
- **Configurable**: idle timeout, concurrency limits, channel capacity, chunk sizes
- **Runtime-opt-in compression**: zstd request/response compression is compiled
  in and enabled per node through `NodeOptions`; there is no Cargo feature gate

## License

`MIT OR Apache-2.0`
