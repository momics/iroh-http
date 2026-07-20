/**
 * Regression: #338 (device-pass follow-up) — Android body encoding over the
 * Tauri IPC channel.
 *
 * The first #338 fix (falsy `res.body` guard in shared `serve.ts`) was NOT
 * sufficient on real Android hardware. On the Nokia 7.2 Android System WebView
 * `res.body` is actually truthy (`branch=stream`), and the body then throws when
 * piped over the Tauri channel:
 *
 *   serve.ts  — response body: TypeError expected raw binary body or base64 string
 *   fetch.ts  — request  body: {"code":"INVALID_INPUT","message":"expected raw
 *                binary body or base64 string"}
 *
 * Root cause: the Android System WebView does not transmit a raw `Uint8Array`
 * `invoke` payload as `InvokeBody::Raw` — it arrives as JSON, so the Rust
 * `send_chunk` command rejects it. Desktop / iOS (WKWebView) deliver raw binary
 * fine. Both request AND response bodies funnel through the single
 * `TauriAdapter.sendChunk`, so the fix belongs there.
 *
 * Fix: `sendChunk` starts on the fast raw-binary path and, the first time a
 * platform rejects the raw payload as non-binary, permanently switches this
 * adapter to base64 (the command's documented JSON compat path). Desktop / iOS
 * never trigger the fallback.
 *
 * These tests drive the REAL adapter's `sendChunk` through the shared
 * `pipeToWriter` (the exact code path used by both `serve` and `fetch` body
 * pipes), mocking IPC to simulate each platform's `send_chunk` behaviour.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { decodeBase64, pipeToWriter } from "@momics/iroh-http-shared";
import { _createAdapterForTesting } from "../index.ts";

afterEach(() => {
  clearMocks();
});

const ANDROID_RAW_REJECTION =
  '{"code":"INVALID_INPUT","message":"expected raw binary body or base64 string"}';

/** Build a `ReadableStream` that emits the given chunks then closes. */
function streamOf(...chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(controller) {
      for (const c of chunks) controller.enqueue(c);
      controller.close();
    },
  });
}

function concat(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((n, c) => n + c.byteLength, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const c of chunks) {
    out.set(c, offset);
    offset += c.byteLength;
  }
  return out;
}

/** True for a raw binary invoke payload (Uint8Array / ArrayBuffer / view). */
function isRawBinary(payload: unknown): boolean {
  return payload instanceof Uint8Array ||
    payload instanceof ArrayBuffer ||
    ArrayBuffer.isView(payload as ArrayBufferView);
}

interface SendChunkRecorder {
  received: Uint8Array[];
  finishBodyCalls: number;
  rawAttempts: number;
  base64Attempts: number;
}

describe("Tauri body encoding over IPC (regression #338 device pass)", () => {
  beforeEach(() => {
    mockWindows("main");
  });

  it("delivers a non-empty body on the Android WebView by falling back to base64", async () => {
    // Simulate the Android System WebView: `send_chunk` rejects a raw binary
    // payload (arrives as JSON in Rust) but accepts a base64 string.
    const rec: SendChunkRecorder = {
      received: [],
      finishBodyCalls: 0,
      rawAttempts: 0,
      base64Attempts: 0,
    };

    mockIPC((cmd, payload) => {
      if (cmd === "plugin:iroh-http|send_chunk") {
        if (isRawBinary(payload)) {
          rec.rawAttempts += 1;
          throw new Error(ANDROID_RAW_REJECTION);
        }
        if (typeof payload === "string") {
          rec.base64Attempts += 1;
          rec.received.push(decodeBase64(payload));
          return;
        }
        throw new Error(ANDROID_RAW_REJECTION);
      }
      if (cmd === "plugin:iroh-http|finish_body") {
        rec.finishBodyCalls += 1;
        return;
      }
    });

    const adapter = _createAdapterForTesting(1);
    const enc = new TextEncoder();
    // Two chunks so we also prove the sticky switch: only the FIRST raw attempt
    // fails, and every subsequent chunk goes straight to base64.
    await pipeToWriter(
      adapter,
      streamOf(enc.encode("hel"), enc.encode("lo")),
      7n,
    );

    expect(new TextDecoder().decode(concat(rec.received))).toBe("hello");
    expect(rec.finishBodyCalls).toBe(1);
    // Exactly one raw probe, then sticky base64 for both chunks.
    expect(rec.rawAttempts).toBe(1);
    expect(rec.base64Attempts).toBe(2);
  });

  it("preserves exact binary bytes (bundled-media / Range) over the Android fallback", async () => {
    const rec: SendChunkRecorder = {
      received: [],
      finishBodyCalls: 0,
      rawAttempts: 0,
      base64Attempts: 0,
    };

    mockIPC((cmd, payload) => {
      if (cmd === "plugin:iroh-http|send_chunk") {
        if (typeof payload === "string") {
          rec.base64Attempts += 1;
          rec.received.push(decodeBase64(payload));
          return;
        }
        rec.rawAttempts += 1;
        throw new Error(ANDROID_RAW_REJECTION);
      }
      if (cmd === "plugin:iroh-http|finish_body") return;
    });

    const bytes = new Uint8Array([0x00, 0x01, 0xff, 0x7f, 0x80, 0x00, 0xab]);
    const adapter = _createAdapterForTesting(1);
    await pipeToWriter(adapter, streamOf(bytes), 7n);

    expect([...concat(rec.received)]).toEqual([...bytes]);
  });

  it("keeps the raw-binary fast path on desktop / iOS (no base64, no regression)", async () => {
    // Simulate WKWebView / desktop: raw binary is accepted; base64 must never
    // be used, so the adapter's wire format is unchanged on those platforms.
    const rawChunks: Uint8Array[] = [];
    let base64Used = false;

    mockIPC((cmd, payload) => {
      if (cmd === "plugin:iroh-http|send_chunk") {
        if (isRawBinary(payload)) {
          rawChunks.push(new Uint8Array(payload as ArrayBufferLike));
          return;
        }
        base64Used = true;
        return;
      }
      if (cmd === "plugin:iroh-http|finish_body") return;
    });

    const adapter = _createAdapterForTesting(1);
    const enc = new TextEncoder();
    await pipeToWriter(
      adapter,
      streamOf(enc.encode("ab"), enc.encode("cd")),
      7n,
    );

    expect(base64Used).toBe(false);
    expect(new TextDecoder().decode(concat(rawChunks))).toBe("abcd");
  });

  it("rethrows non-encoding send_chunk errors instead of masking them as base64", async () => {
    // A genuine failure (e.g. unknown handle) must NOT be swallowed by the
    // base64 fallback — only the raw-binary-unsupported error triggers it.
    mockIPC((cmd) => {
      if (cmd === "plugin:iroh-http|send_chunk") {
        throw new Error(
          '{"code":"INVALID_HANDLE","message":"unknown body writer handle"}',
        );
      }
      if (cmd === "plugin:iroh-http|finish_body") return;
    });

    const adapter = _createAdapterForTesting(1);
    await expect(
      pipeToWriter(adapter, streamOf(new TextEncoder().encode("x")), 7n),
    ).rejects.toThrow(/INVALID_HANDLE/);
  });
});
