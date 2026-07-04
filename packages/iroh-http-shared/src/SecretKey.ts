/**
 * SecretKey — typed wrapper around an Ed25519 secret key.
 *
 * Persist `toBytes()` to restore identity across restarts. In Tauri webviews,
 * endpoint secrets may stay native-held and require the crypto-gated
 * `exportSecretKey(endpointHandle)` helper instead.
 * The associated `publicKey` is derived lazily on first access.
 */

import { base32Decode, base32Encode } from "./base32.js";
import { PublicKey } from "./PublicKey.js";

const ED25519: EcKeyAlgorithm = {
  name: "Ed25519",
} as unknown as EcKeyAlgorithm;

/** DER-encoded PKCS8 prefix for an Ed25519 private key (RFC 8410). */
const ED25519_PKCS8_PREFIX = new Uint8Array([
  0x30,
  0x2e,
  0x02,
  0x01,
  0x00,
  0x30,
  0x05,
  0x06,
  0x03,
  0x2b,
  0x65,
  0x70,
  0x04,
  0x22,
  0x04,
  0x20,
]);

/** Wrap a 32-byte Ed25519 seed in PKCS8 DER encoding for Web Crypto import. */
function ed25519Pkcs8(seed: Uint8Array): ArrayBuffer {
  const buf = new Uint8Array(ED25519_PKCS8_PREFIX.length + 32);
  buf.set(ED25519_PKCS8_PREFIX);
  buf.set(seed, ED25519_PKCS8_PREFIX.length);
  return buf.buffer;
}

/**
 * An Ed25519 secret key.
 *
 * @example Save and restore identity:
 * ```ts
 * // First run — generate and save:
 * const node = await createNode();
 * // In Tauri webviews, node.secretKey may be undefined; use the crypto-gated
 * // exportSecretKey(endpointHandle) helper instead.
 * localStorage.setItem('key', btoa(String.fromCharCode(...node.secretKey.toBytes())));
 *
 * // Subsequent runs — restore:
 * const raw = Uint8Array.from(atob(localStorage.getItem('key')!), c => c.charCodeAt(0));
 * const node2 = await createNode({ key: raw });
 * ```
 */
export class SecretKey {
  readonly #bytes: Uint8Array<ArrayBuffer>;
  #publicKey: PublicKey | null = null;

  private constructor(bytes: Uint8Array) {
    if (bytes.length !== 32) {
      throw new TypeError(`SecretKey must be 32 bytes, got ${bytes.length}`);
    }
    this.#bytes = bytes.slice();
  }

  /** Copy of the raw 32-byte secret key material. */
  toBytes(): Uint8Array {
    return this.#bytes.slice();
  }

  /** Base32 representation of the secret key bytes. */
  toString(): string {
    return base32Encode(this.#bytes);
  }

  /**
   * The associated public key.
   *
   * Available immediately if the `SecretKey` was constructed via
   * `_fromBytesWithPublicKey` (as `buildNode` does), otherwise requires a
   * call to `derivePublicKey()` first — accessing this getter before that
   * throws a `TypeError`.
   */
  get publicKey(): PublicKey {
    if (this.#publicKey === null) {
      throw new TypeError(
        "publicKey not yet available — call await secretKey.derivePublicKey() first",
      );
    }
    return this.#publicKey;
  }

  /** Generate a fresh random key using `crypto.getRandomValues`. */
  static generate(): SecretKey {
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    return new SecretKey(bytes);
  }

  /** Construct from 32 raw bytes. Copies the input. */
  static fromBytes(bytes: Uint8Array): SecretKey {
    return new SecretKey(bytes);
  }

  /** Parse from a base32 string (case-insensitive). */
  static fromString(s: string): SecretKey {
    return new SecretKey(base32Decode(s));
  }

  /**
   * Internal helper used by `buildNode` when the public key is already known
   * from the endpoint info returned by Rust — avoids an extra async round-trip.
   * @internal
   */
  static _fromBytesWithPublicKey(
    bytes: Uint8Array,
    publicKey: PublicKey,
  ): SecretKey {
    const sk = new SecretKey(bytes);
    sk.#publicKey = publicKey;
    return sk;
  }

  /**
   * Derive the corresponding public key using Web Crypto (Ed25519).
   * Caches the result so subsequent calls to `this.publicKey` are synchronous.
   */
  async derivePublicKey(): Promise<PublicKey> {
    if (this.#publicKey !== null) return this.#publicKey;
    // Import via PKCS8 (works on Node, Deno, browsers — JWK without `x` fails on Node).
    const cryptoKey = await crypto.subtle.importKey(
      "pkcs8",
      ed25519Pkcs8(this.#bytes),
      ED25519,
      true,
      ["sign"],
    );
    const pubJwk = await crypto.subtle.exportKey("jwk", cryptoKey);
    const pubBytes = Uint8Array.from(
      atob((pubJwk.x as string).replace(/-/g, "+").replace(/_/g, "/")),
      (c) => c.charCodeAt(0),
    );
    this.#publicKey = PublicKey.fromBytes(pubBytes);
    return this.#publicKey;
  }

  /**
   * Sign `data` with this Ed25519 secret key.
   * Returns a 64-byte signature.
   */
  async sign(data: Uint8Array): Promise<Uint8Array> {
    const cryptoKey = await crypto.subtle.importKey(
      "pkcs8",
      ed25519Pkcs8(this.#bytes),
      ED25519,
      false,
      ["sign"],
    );
    const sig = await crypto.subtle.sign(ED25519, cryptoKey, data.slice());
    return new Uint8Array(sig);
  }
}
