/**
 * Unit tests for `SecretKey` string redaction (regression #274).
 *
 * `SecretKey.toString()` must never leak the raw key material. Exporting the
 * secret has to go through an explicitly-named method so it is always a
 * deliberate act.
 */

import { beforeAll, describe, expect, it } from "vitest";
import { randomFillSync } from "crypto";
import { SecretKey } from "@momics/iroh-http-shared";

// jsdom lacks WebCrypto — polyfill the bits `SecretKey.generate` needs.
beforeAll(() => {
  if (!globalThis.crypto?.getRandomValues) {
    Object.defineProperty(globalThis, "crypto", {
      configurable: true,
      value: {
        getRandomValues: (buffer: Uint8Array) => randomFillSync(buffer),
      },
    });
  }
});

const REDACTED = "SecretKey(********)";

describe("SecretKey redaction (#274)", () => {
  it("toString() returns a redacted marker, not the raw secret", () => {
    const sk = SecretKey.generate();
    const raw = sk.toBase32Secret();

    expect(sk.toString()).toBe(REDACTED);
    expect(sk.toString()).not.toBe(raw);
    expect(sk.toString()).not.toContain(raw);
  });

  it("template interpolation and String() do not leak the secret", () => {
    const sk = SecretKey.generate();
    const raw = sk.toBase32Secret();

    expect(`${sk}`).toBe(REDACTED);
    expect(String(sk)).toBe(REDACTED);
    expect(`${sk}`).not.toContain(raw);
  });

  it("toBase32Secret() exports the raw base32 secret and round-trips", () => {
    const sk = SecretKey.generate();
    const exported = sk.toBase32Secret();

    // Exported string is the real secret, restorable via fromString.
    const restored = SecretKey.fromString(exported);
    expect(restored.toBytes()).toEqual(sk.toBytes());
  });

  it("toStringUnsafe() is an alias for toBase32Secret()", () => {
    const sk = SecretKey.generate();
    expect(sk.toStringUnsafe()).toBe(sk.toBase32Secret());
  });
});
