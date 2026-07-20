/**
 * Regression: #338 — Android (Tauri) `node.serve()` delivers an EMPTY body.
 *
 * Root cause: the shared serve pipe used `res.body ?? emptyStream()`. On the
 * Android System WebView, a body-carrying `new Response("hello")` returns a
 * FALSY `res.body` (no usable `ReadableStream`), so `?? emptyStream()` silently
 * dropped the body — remote peers saw correct status + headers but zero bytes
 * (`send_chunk` was never called, only `finish_body`).
 *
 * Fix: when `res.body` is falsy, buffer the bytes via `res.arrayBuffer()` into a
 * one-shot stream. Genuine empty bodies (204/304) stay empty; when `res.body` is
 * present the stream is used directly so streaming is NOT forced to buffer.
 *
 * These tests exercise the response-body path at the FFI/adapter boundary by
 * driving the real `makeServe` from `@momics/iroh-http-shared` with a mock
 * adapter that records `sendChunk` / `finishBody`. The Android WebView is
 * simulated with a real `Response` whose `body` getter is overridden to `null`
 * while `arrayBuffer()` still yields the bytes — exactly the observed behaviour.
 */

import { describe, expect, it } from "vitest";
import { makeServe } from "@momics/iroh-http-shared";

/** Fields of the FFI request payload that the shared serve loop reads. */
interface ServePayload {
  method: string;
  url: string;
  headers: [string, string][];
  remoteNodeId: string;
  reqHandle: bigint;
  reqBodyHandle: bigint;
  resBodyHandle: bigint;
}

type ServeCallback = (
  payload: ServePayload,
) => Promise<{ status: number; headers: [string, string][] }>;

/**
 * Simulate the Android System WebView: a real `Response` that carries bytes but
 * whose `body` getter yields `null` (no usable `ReadableStream`). `arrayBuffer()`
 * still returns the bytes, mirroring the device behaviour observed in #338.
 */
function androidResponse(body: BodyInit | null, init?: ResponseInit): Response {
  const res = new Response(body, init);
  Object.defineProperty(res, "body", { get: () => null, configurable: true });
  return res;
}

interface DrivenServe {
  head: { status: number; headers: [string, string][] };
  chunks: Uint8Array[];
  finishBodyCalled: boolean;
  sendChunkCalls: number;
}

/**
 * Drive one request through `makeServe` with a mock adapter and return what the
 * adapter received on the response-body writer handle.
 */
async function driveServeOnce(
  handler: () => Response | Promise<Response>,
  payloadOverrides: Partial<ServePayload> = {},
): Promise<DrivenServe> {
  const chunks: Uint8Array[] = [];
  let sendChunkCalls = 0;
  let finishBodyCalled = false;
  let resolveFinish!: () => void;
  const finishBodyDone = new Promise<void>((resolve) => {
    resolveFinish = resolve;
  });

  let captured: ServeCallback | undefined;

  const adapter = {
    // Capture the per-request callback; return a promise that never resolves so
    // the serve loop stays "running" for the duration of the test.
    rawServe(_handle: number, _opts: unknown, callback: ServeCallback) {
      captured = callback;
      return new Promise<void>(() => {});
    },
    async sendChunk(_handle: bigint, chunk: Uint8Array) {
      sendChunkCalls += 1;
      chunks.push(new Uint8Array(chunk));
    },
    async finishBody(_handle: bigint) {
      finishBodyCalled = true;
      resolveFinish();
    },
    async nextChunk() {
      return null;
    },
    async cancelRequest() {},
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } as unknown as Parameters<typeof makeServe>[0];

  // onNodeClose never resolves — the node stays alive for the test.
  const serve = makeServe(adapter, 1, new Promise<void>(() => {}));
  serve(handler as never);

  if (!captured) throw new Error("rawServe callback was not captured");

  const payload: ServePayload = {
    method: "GET",
    url: "http://peer/resource",
    headers: [],
    remoteNodeId: "peer",
    reqHandle: 0n,
    reqBodyHandle: 0n,
    resBodyHandle: 7n,
    ...payloadOverrides,
  };

  const head = await captured(payload);
  // The non-error path pipes the body concurrently; wait for finishBody.
  await finishBodyDone;

  return { head, chunks, finishBodyCalled, sendChunkCalls };
}

function concatChunks(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((n, c) => n + c.byteLength, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const c of chunks) {
    out.set(c, offset);
    offset += c.byteLength;
  }
  return out;
}

describe("serve response-body path (regression #338)", () => {
  it("delivers a non-empty body when res.body is falsy (Android WebView)", async () => {
    const { head, chunks, finishBodyCalled } = await driveServeOnce(() =>
      androidResponse("hello")
    );

    expect(head.status).toBe(200);
    expect(finishBodyCalled).toBe(true);
    // Before the fix, `res.body ?? emptyStream()` dropped the body and no chunk
    // was ever sent — this assertion is the core regression guard.
    expect(new TextDecoder().decode(concatChunks(chunks))).toBe("hello");
  });

  it("delivers exact binary bytes for a bundled-media / Range response with falsy res.body", async () => {
    const bytes = new Uint8Array([0x00, 0x01, 0xff, 0x7f, 0x80, 0x00, 0xab]);
    const { head, chunks } = await driveServeOnce(() =>
      androidResponse(bytes, {
        status: 206,
        headers: {
          "Content-Range": "bytes 0-6/128",
          "Content-Type": "video/mp4",
        },
      })
    );

    expect(head.status).toBe(206);
    expect(
      head.headers.some(
        ([k, v]) =>
          k.toLowerCase() === "content-range" && v === "bytes 0-6/128",
      ),
    ).toBe(true);
    expect([...concatChunks(chunks)]).toEqual([...bytes]);
  });

  it("keeps a genuinely empty body empty (204, falsy res.body → no chunks)", async () => {
    const { head, chunks, finishBodyCalled } = await driveServeOnce(() =>
      androidResponse(null, { status: 204 })
    );

    expect(head.status).toBe(204);
    expect(finishBodyCalled).toBe(true);
    expect(chunks.length).toBe(0);
  });

  it("streams a present res.body without forcing a full buffer (no regression)", async () => {
    const encoder = new TextEncoder();
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoder.encode("foo"));
        controller.enqueue(encoder.encode("bar"));
        controller.close();
      },
    });
    const res = new Response(stream);
    // If the fix ever regressed to buffering when res.body is present, this
    // would be invoked and throw — guarding acceptance criterion #2.
    Object.defineProperty(res, "arrayBuffer", {
      value: () => {
        throw new Error(
          "arrayBuffer() must not be called when res.body is present",
        );
      },
      configurable: true,
    });

    const { head, chunks, finishBodyCalled } = await driveServeOnce(() => res);

    expect(head.status).toBe(200);
    expect(finishBodyCalled).toBe(true);
    // Both source chunks are delivered, in order, streamed rather than merged.
    expect(chunks.map((c) => new TextDecoder().decode(c))).toEqual([
      "foo",
      "bar",
    ]);
  });
});
