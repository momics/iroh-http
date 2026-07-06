/**
 * Microbenchmarks for iroh-http shared TypeScript layer (#129).
 *
 * Isolates per-call overhead of:
 *   - normaliseHeaders (empty, array, Headers, object)
 *   - bodyInitToStream (string, Uint8Array, null)
 *   - decodeBase64 (small, medium)
 *   - Response construction: inline Uint8Array vs ReadableStream wrapper
 *
 * Run: node bench/node/bench-shared-layer.mjs
 */

import { bench, group, run } from "mitata";

// ── Inline implementations (mirrors packages/iroh-http-shared/src) ────────

function normaliseHeaders(h) {
  if (!h) return [];
  if (h instanceof Headers) {
    const pairs = [];
    h.forEach((v, k) => pairs.push([k, v]));
    return pairs;
  }
  if (Array.isArray(h)) return h;
  return Object.entries(h);
}

function normaliseHeadersOpt(h) {
  if (!h) return EMPTY_HEADERS;
  if (Array.isArray(h)) return h;
  if (h instanceof Headers) return [...h];
  return Object.entries(h);
}

const EMPTY_HEADERS = Object.freeze([]);

function decodeBase64Current(s) {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function decodeBase64Optimised(s) {
  const buf = Buffer.from(s, "base64");
  // Mirror the shipped implementation: copy pool-backed views (small inputs)
  // into an exactly-sized array; return larger, exact-backed buffers as-is.
  return buf.byteOffset === 0 && buf.buffer.byteLength === buf.byteLength
    ? buf
    : new Uint8Array(buf);
}

function singleChunkStream(data) {
  return new ReadableStream({
    start(controller) {
      controller.enqueue(data);
      controller.close();
    },
  });
}

// ── Payloads ──────────────────────────────────────────────────────────────────

const SMALL_BODY = new Uint8Array(11); // '{"ok":true}'
const MEDIUM_BODY = new Uint8Array(4096).fill(0x61);

const SMALL_B64 = btoa(String.fromCharCode(...SMALL_BODY));
const MEDIUM_B64 = (() => {
  const CHUNK = 0x8000;
  const parts = [];
  for (let i = 0; i < MEDIUM_BODY.length; i += CHUNK) {
    parts.push(String.fromCharCode(...MEDIUM_BODY.subarray(i, i + CHUNK)));
  }
  return btoa(parts.join(""));
})();

const HEADERS_ARRAY = [["content-type", "application/json"], ["x-custom", "value"]];
const HEADERS_OBJ = { "content-type": "application/json", "x-custom": "value" };
const HEADERS_INST = new Headers(HEADERS_OBJ);

// ── Benchmarks ────────────────────────────────────────────────────────────────

group("normaliseHeaders", () => {
  bench("normaliseHeaders/empty (current)", () => normaliseHeaders(undefined));
  bench("normaliseHeaders/empty (optimised)", () => normaliseHeadersOpt(undefined));
  bench("normaliseHeaders/array (current)", () => normaliseHeaders(HEADERS_ARRAY));
  bench("normaliseHeaders/array (optimised)", () => normaliseHeadersOpt(HEADERS_ARRAY));
  bench("normaliseHeaders/Headers (current)", () => normaliseHeaders(HEADERS_INST));
  bench("normaliseHeaders/Headers (optimised)", () => normaliseHeadersOpt(HEADERS_INST));
  bench("normaliseHeaders/object (current)", () => normaliseHeaders(HEADERS_OBJ));
  bench("normaliseHeaders/object (optimised)", () => normaliseHeadersOpt(HEADERS_OBJ));
});

group("decodeBase64", () => {
  bench("decodeBase64/small/current", () => decodeBase64Current(SMALL_B64));
  bench("decodeBase64/small/buffer", () => decodeBase64Optimised(SMALL_B64));
  bench("decodeBase64/4KB/current", () => decodeBase64Current(MEDIUM_B64));
  bench("decodeBase64/4KB/buffer", () => decodeBase64Optimised(MEDIUM_B64));
});

group("response-construction", () => {
  bench("response/ReadableStream (current)", () => {
    const decoded = SMALL_BODY;
    const body = new ReadableStream({
      start(controller) {
        if (decoded.byteLength > 0) controller.enqueue(decoded);
        controller.close();
      },
    });
    return new Response(body, { status: 200 });
  });

  bench("response/Uint8Array direct (optimised)", () => {
    const decoded = SMALL_BODY;
    return new Response(decoded, { status: 200 });
  });

  bench("response/ReadableStream 4KB (current)", () => {
    const decoded = MEDIUM_BODY;
    const body = new ReadableStream({
      start(controller) {
        if (decoded.byteLength > 0) controller.enqueue(decoded);
        controller.close();
      },
    });
    return new Response(body, { status: 200 });
  });

  bench("response/Uint8Array direct 4KB (optimised)", () => {
    const decoded = MEDIUM_BODY;
    return new Response(decoded, { status: 200 });
  });
});

group("bodyInitToStream", () => {
  bench("bodyInitToStream/null", () => {
    const body = null;
    return body == null ? null : singleChunkStream(body);
  });

  bench("bodyInitToStream/Uint8Array", () => {
    return singleChunkStream(SMALL_BODY);
  });

  bench("bodyInitToStream/string", () => {
    return singleChunkStream(new TextEncoder().encode("hello world"));
  });
});

await run({ units: true });
