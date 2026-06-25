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
 * Decode a base64 string to a `Uint8Array`.
 *
 * Uses the platform-native `Buffer.from` when available (Node.js, Deno) for
 * ~7× faster decoding on typical response bodies (#129 profiling).
 */
export const decodeBase64: (s: string) => Uint8Array =
  typeof (globalThis as Record<string, unknown>).Buffer === "function"
    ? (s) =>
      (globalThis as unknown as {
        Buffer: { from(s: string, e: string): Uint8Array };
      })
        .Buffer.from(s, "base64")
    : (s) => {
      const bin = atob(s);
      const out = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      return out;
    };
