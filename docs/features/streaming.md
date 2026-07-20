# Streaming Bodies

## What

Both `node.fetch` and `node.serve` support fully streaming request and response bodies using web-standard `ReadableStream` and `WritableStream`. Bodies are never fully buffered in JS — chunks flow from the QUIC layer through a typed Rust channel directly into the stream controller.

## Sending a streaming request body

```ts
const stream = new ReadableStream<Uint8Array>({
  async start(controller) {
    for (const chunk of generateChunks()) {
      controller.enqueue(chunk);
    }
    controller.close();
  },
});

const res = await node.fetch(peer.toURL('/upload'), {
  method: 'POST',
  body: stream,
  // Required by the Fetch spec when body is a stream and the response
  // may arrive before the request body is fully sent:
  duplex: 'half',
});
```

Any `BodyInit` accepted by the standard `fetch` API works: `string`, `Uint8Array`, `Blob`, `URLSearchParams`, or `ReadableStream<Uint8Array>`.

> **Note**: `FormData` is not supported in v1. Serialise the form data manually
> and pass a `string` or `Uint8Array` body instead.

## Receiving a streaming response body

```ts
const res = await node.fetch(peer.toURL('/download'));

// Standard streaming consumption:
for await (const chunk of res.body!) {
  process(chunk);
}

// Or pipe through a transform:
await res.body!
  .pipeThrough(new TextDecoderStream())
  .pipeTo(writableDestination);
```

`res.body` is a standard `ReadableStream<Uint8Array>`. All platform stream combinators (`pipeTo`, `pipeThrough`, `tee`, `getReader`) work without restriction.

## Streaming in a serve handler

```ts
node.serve({}, (req) => {
  // Read request body as a stream:
  const reader = req.body?.getReader();

  // Return a streaming response:
  const { readable, writable } = new TransformStream<Uint8Array, Uint8Array>();
  produceIntoWritable(writable);  // write concurrently

  return new Response(readable, {
    headers: { 'Content-Type': 'application/octet-stream' },
  });
});
```

The request body and response body are pumped concurrently. The serve handler can begin writing the response while still reading the request body — there is no forced buffering at the iroh-http layer.

## Backpressure

Backpressure is propagated end-to-end:

- On the **receive** path, `ReadableStream`'s internal queue signals when to pull; `makeReadable` only calls `bridge.nextChunk` when the consumer pulls. The Rust `BodyReader` channel blocks until the JS side is ready.
- On the **send** path, `pipeToWriter` drains the source `ReadableStream` into `bridge.sendChunk` calls. The Rust `BodyWriter` channel applies backpressure when the QUIC send buffer is full.

Large uploads and downloads do not accumulate in memory.

## Body size and chunking

There is no enforced body size limit at the iroh-http layer. Chunked transfer encoding is applied automatically when no `Content-Length` is known (i.e. for streaming bodies). When a `Content-Length` header is present, raw framing is used instead.

The maximum chunk size across the FFI boundary is configurable via
`NodeOptions.internals.maxChunkSizeBytes` (default 64 KB). Larger chunks are
split transparently; JS always receives them reassembled.

## Cancellation

Pass `AbortSignal` in the fetch init to cancel an in-flight request:

```ts
const controller = new AbortController();
setTimeout(() => controller.abort(), 5000);

const res = await node.fetch(peer.toURL('/slow'), { signal: controller.signal });
```

Cancellation propagates to the Rust layer: the in-flight fetch token is released, and the underlying QUIC stream is reset. Both read and write sides are cancelled.
