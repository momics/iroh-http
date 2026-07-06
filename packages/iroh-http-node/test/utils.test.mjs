/**
 * Shared base64 helper tests — Node.js perspective (#280).
 *
 * `decodeBase64` picks a Node-native `Buffer.from(str, "base64")` fast path
 * when `Buffer` is available and falls back to a portable `atob` loop on
 * browsers/Deno. These tests cover:
 *   - round-trip correctness of the Node fast path,
 *   - the pool-safety guard (small Node buffers are copied out of the shared
 *     internal memory pool so callers never see unrelated `.buffer` bytes),
 *   - correctness of the portable fallback, exercised by re-importing the
 *     module with `Buffer` removed from the global scope.
 *
 * Run:  node --test test/utils.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { decodeBase64, encodeBase64 } from "@momics/iroh-http-shared";

const require = createRequire(import.meta.url);

function b64(bytes) {
  return Buffer.from(bytes).toString("base64");
}

test("decodeBase64 round-trips arbitrary bytes", () => {
  for (const len of [0, 1, 11, 100, 4096, 65537]) {
    const bytes = new Uint8Array(len);
    for (let i = 0; i < len; i++) bytes[i] = (i * 31 + 7) & 0xff;
    const decoded = decodeBase64(b64(bytes));
    assert.ok(decoded instanceof Uint8Array);
    assert.deepEqual([...decoded], [...bytes], `mismatch for length ${len}`);
  }
});

test("decodeBase64 is the inverse of encodeBase64", () => {
  const bytes = new Uint8Array([0, 1, 2, 250, 251, 252, 253, 254, 255]);
  assert.deepEqual([...decodeBase64(encodeBase64(bytes))], [...bytes]);
});

test("decodeBase64 never exposes shared pool memory (small inputs)", () => {
  // Node's Buffer.from may hand back a view into an 8 KiB shared pool for
  // small inputs (nonzero byteOffset, oversized backing ArrayBuffer). The
  // fast path must copy those into an exactly-sized array.
  const decoded = decodeBase64(b64(new Uint8Array([1, 2, 3, 4, 5])));
  assert.equal(decoded.byteOffset, 0, "byteOffset must be 0");
  assert.equal(
    decoded.buffer.byteLength,
    decoded.byteLength,
    "backing ArrayBuffer must be exactly sized",
  );
  assert.deepEqual([...decoded], [1, 2, 3, 4, 5]);
});

test("decodeBase64 uses the Node Buffer fast path when Buffer is present", () => {
  assert.equal(typeof globalThis.Buffer, "function");
  // A large exact-backed payload should decode via Buffer.from and be
  // returned without an extra copy (byteOffset 0, tight backing buffer).
  const bytes = new Uint8Array(8192).fill(0xab);
  const decoded = decodeBase64(b64(bytes));
  assert.equal(decoded.byteOffset, 0);
  assert.equal(decoded.buffer.byteLength, decoded.byteLength);
  assert.deepEqual([...decoded.subarray(0, 4)], [0xab, 0xab, 0xab, 0xab]);
});

test("decodeBase64 falls back to the portable atob loop without Buffer", async () => {
  const savedBuffer = globalThis.Buffer;
  // Resolve the built module file so we can re-import a fresh instance with a
  // cache-busting query after removing the Buffer global — this forces the
  // module-load feature detection down the portable (browser/Deno) branch.
  const indexUrl = pathToFileURL(require.resolve("@momics/iroh-http-shared"));
  const freshUrl = new URL(`./utils.js?portable=${Date.now()}`, indexUrl).href;
  try {
    // @ts-expect-error — deliberately unset for the fallback path.
    delete globalThis.Buffer;
    const portable = await import(freshUrl);
    const bytes = new Uint8Array([9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
    // Rebuild the base64 string without Buffer to stay in the fallback world.
    const encoded = portable.encodeBase64(bytes);
    const decoded = portable.decodeBase64(encoded);
    assert.ok(decoded instanceof Uint8Array);
    assert.equal(decoded.byteOffset, 0);
    assert.equal(decoded.buffer.byteLength, decoded.byteLength);
    assert.deepEqual([...decoded], [...bytes]);
  } finally {
    globalThis.Buffer = savedBuffer;
  }
});
