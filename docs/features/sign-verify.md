# Sign / Verify / Encrypt

`SecretKey` and `PublicKey` are the cryptographic primitives of iroh-http.
Every node has an Ed25519 identity keypair. The same keys that authenticate
the transport are available for signing, verifying, and sealing messages at the
application layer.

## Importing the key classes

`PublicKey` and `SecretKey` are exported from each adapter package:

```ts
// Node.js
import { createNode, PublicKey, SecretKey } from "@momics/iroh-http-node";

// Deno
import { createNode, PublicKey, SecretKey } from "@momics/iroh-http-deno";

// Tauri
import { createNode, PublicKey, SecretKey } from "@momics/iroh-http-tauri";
```

A node exposes its own secret key **only when you supplied one at creation**.
Pass a `key` to `createNode` and the return type narrows to
`IrohNodeWithSecret`, where `secretKey` is guaranteed present:

```ts
const key = SecretKey.generate();
const node = await createNode({ key });
const sk: SecretKey  = node.secretKey;   // Ed25519 secret key (the one you passed)
const pk: PublicKey  = node.publicKey;   // Ed25519 public key (= node ID)
```

Omit `key` and the identity is generated natively and never surfaced to JS, so
`node.secretKey` is `undefined` on every adapter. To sign with a node's own
identity, generate the key yourself and pass it in (as above).

## Sign

Sign arbitrary bytes with a `SecretKey`. Returns a 64-byte Ed25519 signature.

```ts
const data = new TextEncoder().encode("hello iroh");
const sig: Uint8Array = await node.secretKey.sign(data);
```

`SecretKey.generate()` creates a standalone key that is not tied to a node:

```ts
const key = SecretKey.generate();
const sig = await key.sign(data);
```

## Verify

Verify a signature against any `PublicKey`. Returns `false` rather than
throwing on an invalid signature.

```ts
// Verify using the sender's known node ID:
const senderKey = PublicKey.fromString(senderNodeId);
const ok: boolean = await senderKey.verify(data, sig);

// Or using the public key already on a node object:
const ok = await node.publicKey.verify(data, sig);
```

## Encrypt / Decrypt

Sealed-box encryption (Ed25519→X25519 key conversion, ephemeral ECDH,
HKDF-SHA256, AES-GCM-256) is available as a recipe pattern rather than a
core API — the derived X25519 keys are cryptographically distinct from the
Ed25519 identity keys. See [sealed-messages recipe](../recipes/sealed-messages.md)
for the full implementation.

## Types summary

| Value | Type | Description |
|---|---|---|
| `sig` | `Uint8Array` (64 bytes) | Ed25519 signature |
| `publicKey.verify` result | `boolean` | `false` on invalid sig, never throws |

See also: [key classes in the specification](../specification.md#key-classes).

All cryptographic operations are **async** — always `await` them.

## Platform support

| Feature | Node / Deno / Tauri |
|---------|:---:|
| **Sign** (`secretKey.sign`) | ✅ class method |
| **Verify** (`publicKey.verify`) | ✅ class method |
| **Generate key** (`SecretKey.generate`) | ✅ class method |
| **Sealed-box encrypt/decrypt** | via [recipe](../recipes/sealed-messages.md) |

## What to avoid

Do not use the lower-level `secretKeySign` / `publicKeyVerify` functions that
some older adapter versions exported. Those take raw `Uint8Array` keys instead
of typed class instances, are inconsistently available across adapters, and are
removed in the current API. Use the class methods above instead.

## See also

- [sealed-messages](../recipes/sealed-messages.md) — encrypt messages for offline delivery
- [capability-tokens](../recipes/capability-tokens.md) — signed access tokens
- [sign-verify (feature)](sign-verify.md) — this page
- [witness-receipts](../recipes/witness-receipts.md) — tamper-evident audit logs
