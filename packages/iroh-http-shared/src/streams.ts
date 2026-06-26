/**
 * Web-standard stream helpers.
 *
 * `makeReadable` and `pipeToWriter` are the two primitives that map
 * integer body handles to `ReadableStream` / `WritableStream` abstractions.
 */

import type { IrohAdapter } from "./IrohAdapter.js";

/**
 * Wrap a `BodyReader` handle in a web-standard `ReadableStream<Uint8Array>`.
 *
 * Pulls from the adapter via `nextChunk` on each `pull` request.
 * The stream closes automatically when `nextChunk` returns `null`.
 *
 * @param adapter  Platform adapter implementation.
 * @param handle   Slab handle for the `BodyReader` to read from.
 * @param onClose  Optional callback invoked when the stream reaches EOF or is cancelled.
 * @returns A `ReadableStream<Uint8Array>` backed by the body channel.
 */
export function makeReadable(
  adapter: IrohAdapter,
  handle: bigint,
  onClose?: () => void,
): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      const chunk = await adapter.nextChunk(handle);
      if (chunk === null) {
        controller.close();
        onClose?.();
      } else {
        controller.enqueue(chunk);
      }
    },
    cancel() {
      adapter.cancelRequest(handle);
      onClose?.();
    },
  });
}

/**
 * Drain a `ReadableStream<Uint8Array>` into a `BodyWriter` handle.
 *
 * Calls `sendChunk` for each chunk, then `finishBody` **only on a clean EOF**.
 * If the source stream or a `sendChunk` fails mid-body, the writer is left
 * unfinished and the error is propagated to the returned `Promise`: calling
 * `finishBody` on failure would signal a complete body to the peer, silently
 * truncating a signed upload / append-only entry into a "clean" close. The
 * caller is responsible for resetting the underlying request stream (e.g. via
 * `cancelFetch`) when this promise rejects.
 *
 * Large chunks are split into 64 KB pieces to keep each sync FFI call short.
 * This prevents blocking the JS thread when a ReadableStream enqueues an
 * entire body as one chunk (e.g. `singleChunkStream` for Uint8Array bodies).
 * Each piece is `await`-ed so the event loop can process other work (such as
 * the rawFetch dispatch that claims the body reader) between sends.
 *
 * @param adapter  Platform adapter implementation.
 * @param stream   The `ReadableStream` to consume.
 * @param handle   Slab handle for the `BodyWriter` to write to.
 * @returns Resolves when the entire stream has been piped and finished.
 */
export async function pipeToWriter(
  adapter: IrohAdapter,
  stream: ReadableStream<Uint8Array>,
  handle: bigint,
): Promise<void> {
  // Match the Rust-side max chunk size so each sendChunk is a single
  // channel push (O(1) when the channel has capacity).
  const MAX_CHUNK = 64 * 1024;
  const reader = stream.getReader();
  let completed = false;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      if (!value || value.byteLength === 0) continue;
      if (value.byteLength <= MAX_CHUNK) {
        await adapter.sendChunk(handle, value);
      } else {
        // Split into MAX_CHUNK-sized pieces and yield between each send so
        // the event loop stays responsive and rawFetch can make progress.
        let offset = 0;
        while (offset < value.byteLength) {
          const end = Math.min(offset + MAX_CHUNK, value.byteLength);
          await adapter.sendChunk(handle, value.subarray(offset, end));
          offset = end;
        }
      }
    }
    completed = true;
  } finally {
    reader.releaseLock();
    // Only signal a clean end-of-body when the source drained without error.
    // On failure we deliberately skip finishBody so the peer never sees a
    // truncated body as a successful one — the request stream is reset by the
    // caller instead. (`finally` still re-throws the original error.)
    if (completed) {
      await adapter.finishBody(handle);
    }
  }
}

/**
 * Coerce a `BodyInit` to a `ReadableStream<Uint8Array>`, or `null` for empty bodies.
 *
 * Supports `ReadableStream`, `Uint8Array`, any `ArrayBufferView` (e.g. `Int16Array`,
 * `DataView`), `ArrayBuffer`, `string`, `Blob`, and `URLSearchParams`.
 * Throws for `FormData` (not supported in iroh-http v1) and for any other type.
 *
 * @param body  The body value to coerce.
 * @returns A `ReadableStream<Uint8Array>`, or `null` if the body is empty.
 * @throws {TypeError} If `body` is a `FormData` instance or an unsupported type.
 */
export function bodyInitToStream(
  body: BodyInit | null | undefined,
): ReadableStream<Uint8Array> | null {
  if (body == null) return null;
  if (body instanceof ReadableStream) return body as ReadableStream<Uint8Array>;
  if (body instanceof Uint8Array) {
    return singleChunkStream(body);
  }
  if (body instanceof ArrayBuffer) {
    return singleChunkStream(new Uint8Array(body));
  }
  if (typeof body === "string") {
    return singleChunkStream(new TextEncoder().encode(body));
  }
  if (body instanceof Blob) {
    return body.stream() as ReadableStream<Uint8Array>;
  }
  if (body instanceof FormData) {
    throw new TypeError(
      "FormData request bodies are not supported by iroh-http (v1). " +
        "Serialise the form data manually and pass a string or Uint8Array body instead.",
    );
  }
  if (body instanceof URLSearchParams) {
    return singleChunkStream(new TextEncoder().encode(body.toString()));
  }
  // Catch-all for other ArrayBufferView subtypes (Int16Array, Float64Array, DataView, etc.)
  // Must come after the Uint8Array check so the common case stays on the fast path.
  if (ArrayBuffer.isView(body)) {
    return singleChunkStream(
      new Uint8Array(
        (body as ArrayBufferView).buffer,
        (body as ArrayBufferView).byteOffset,
        (body as ArrayBufferView).byteLength,
      ),
    );
  }
  throw new TypeError(
    `Unsupported BodyInit type: ${Object.prototype.toString.call(body)}. ` +
      `Supported types: ReadableStream, Uint8Array, ArrayBufferView, ArrayBuffer, string, Blob, URLSearchParams.`,
  );
}

function singleChunkStream(data: Uint8Array): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(data);
      controller.close();
    },
  });
}
