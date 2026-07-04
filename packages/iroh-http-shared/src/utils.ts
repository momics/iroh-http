/**
 * Shared utility functions for iroh-http adapters.
 *
 * Centralises helpers that were previously duplicated across Node, Deno,
 * and Tauri adapter code.
 */

import type { RelayMode } from "./options/NodeOptions.js";

// ── Relay mode normalisation ──────────────────────────────────────────────────

export interface NormalisedRelay {
  relayMode: string | undefined;
  relays: string[] | null;
  disableNetworking: boolean;
}

/**
 * Normalise a `relay` option group into the shape expected by the Rust FFI layer.
 */
export function normaliseRelayMode(
  relay?: { mode?: RelayMode; urls?: string[] },
): NormalisedRelay {
  if (relay?.urls && relay.urls.length > 0) {
    return {
      relayMode: "custom",
      relays: relay.urls,
      disableNetworking: false,
    };
  }
  const mode = relay?.mode;
  if (mode === "disabled") {
    return { relayMode: "disabled", relays: [], disableNetworking: true };
  }
  if (mode === "staging") {
    return { relayMode: "staging", relays: null, disableNetworking: false };
  }
  // "default" or undefined: use Iroh's public relay servers
  return { relayMode: undefined, relays: null, disableNetworking: false };
}

// ── Base64 encoding ───────────────────────────────────────────────────────────

/**
 * Encode a `Uint8Array` to a base64 string.
 *
 * Uses chunked `String.fromCharCode` to avoid call-stack limits on large
 * buffers.
 */
export function encodeBase64(u8: Uint8Array): string {
  const CHUNK = 0x8000; // 32 KB — safe for String.fromCharCode spread
  const parts: string[] = [];
  for (let i = 0; i < u8.length; i += CHUNK) {
    parts.push(String.fromCharCode(...u8.subarray(i, i + CHUNK)));
  }
  return btoa(parts.join(""));
}

/**
 * Portable base64 decode using `atob`. Works in every runtime with the Web
 * platform globals (browsers, Deno) and is the fallback when Node's `Buffer`
 * is unavailable.
 */
function decodeBase64Portable(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/**
 * Node's `Buffer` constructor, discovered once at module load. Absent on
 * browsers and on Deno (unless Node compatibility is enabled), where we keep
 * the portable `atob` path — see {@link decodeBase64Portable}.
 */
const nodeBuffer = (globalThis as {
  Buffer?: { from(s: string, encoding: string): Uint8Array };
}).Buffer;

/**
 * Decode a base64 string to a `Uint8Array`.
 *
 * On Node.js, `Buffer.from(str, "base64")` decodes markedly faster than the
 * portable `atob` loop — measured ~8.7× (8.10 µs → 0.93 µs/iter) for 4 KiB
 * payloads via `bench/node/bench-shared-layer.mjs` (#280, #129 profiling).
 * Browsers and Deno keep the portable {@link decodeBase64Portable} fallback.
 *
 * Node may return a `Buffer` that is a view into a shared internal memory pool
 * for small inputs (nonzero `byteOffset`, oversized backing `ArrayBuffer`). We
 * copy those into an exactly-sized `Uint8Array` so callers never observe
 * unrelated pool bytes through `.buffer`/`.byteOffset`; larger payloads (the
 * hot path) are already exact-backed and returned without copying.
 */
export const decodeBase64: (s: string) => Uint8Array =
  typeof nodeBuffer?.from === "function"
    ? (s) => {
      const buf = nodeBuffer!.from(s, "base64");
      return buf.byteOffset === 0 && buf.buffer.byteLength === buf.byteLength
        ? buf
        : new Uint8Array(buf);
    }
    : decodeBase64Portable;
