# Threat Model

This document describes what iroh-http protects against, what it deliberately
does **not** protect against, and the mitigations operators should apply for
their specific deployment.

## What the transport provides

Every iroh-http node has a stable Ed25519 keypair that is its QUIC identity.
All connections are mutually authenticated: before any HTTP traffic flows, both
endpoints have cryptographically verified each other's public key using the QUIC
TLS 1.3 handshake.

This gives you:

| Property | Guarantee |
|---|---|
| **Peer identity** | The remote node's public key is unforgeable; you know exactly who you are talking to |
| **Confidentiality** | All traffic is encrypted with TLS 1.3 — passive eavesdropping yields nothing |
| **Integrity** | TLS 1.3 MAC prevents undetected tampering of data in transit |
| **Replay protection** | TLS 1.3 session tickets and QUIC packet numbers prevent replay |

## What the transport does NOT provide

### Authorization

Authenticating a peer's identity is not the same as deciding whether they are
**allowed** to make a request. iroh-http verifies *who* is connecting but makes
no access-control decisions by itself.

**Mitigation (required for any production deployment):** inspect the
`remoteNodeId` field of every incoming `RequestPayload` and reject requests from
peers not on your allowlist before doing any work:

```ts
node.serve({}, async (req) => {
  const allowed = ['<known-node-id-1>', '<known-node-id-2>'];
  if (!allowed.includes(req.remoteNodeId)) {
    return new Response('Forbidden', { status: 403 });
  }
  // … handle the request
});
```

All examples in `examples/` demonstrate this pattern.

### Sybil resistance

There is no cost to generating a new Ed25519 keypair. An adversary can create
unlimited identities. iroh-http does not prevent Sybil attacks; that is an
application-layer concern (e.g. capability tokens, out-of-band identity
registration).

### Key revocation

If a node's private key is compromised, there is no revocation mechanism. The
compromised key retains the same network identity until all peers update their
allowlists to remove it.

**Mitigation:** maintain out-of-band allowlists that can be updated in real
time. The in-progress capability-token system (see
[`adr/002-capability-url-system.md`](adr/002-capability-url-system.md))
will provide finer-grained, time-bounded, revocable authorisation tokens as a
future mitigation.

### DNS / relay infrastructure

iroh-http nodes use Iroh's relay infrastructure and (optionally) DNS-based
discovery. A compromised DNS server or relay can affect peer *discovery* and
*reachability*, but cannot break the cryptographic identity guarantees:
connection authentication is end-to-end, not relay-mediated.

## Resource exhaustion

Even authenticated peers can mount denial-of-service attacks through resource
exhaustion. iroh-http mitigates this with the following built-in limits (all
configurable via `NodeOptions`):

| Limit | Default | Protects against |
|---|---|---|
| `maxRequestBodyWireBytes` | 16 MiB | Oversized/compressed body exhausting bandwidth |
| `maxRequestBodyDecodedBytes` | 16 MiB | Compression bomb exhausting memory |
| `maxConcurrency` | 1 024 | Request flood from many peers |
| `maxConnectionsPerPeer` | 8 | Connection flood from one peer |
| `requestTimeout` | 60 s | Slow request / stalled handler |
| `maxHeaderBytes` | 64 KiB | Header flood exhausting memory |
| `maxTotalConnections` | unlimited | Total QUIC connection cap |
| QUIC `max_concurrent_bidi_streams` | 128 | Stream-level slowloris |
| Response body limit | 256 MiB | Compression bomb from malicious server |

## Out-of-scope threats

The following are explicitly outside the scope of this library:

- **Application-level authorization** — the library surfaces peer identity;
  what to do with it is the application's responsibility.
- **Key management** — generating, persisting, encrypting, and rotating keys is
  the operator's responsibility. See [SECURITY.md](../SECURITY.md) for guidance.
- **Relay infrastructure attacks** — relay nodes affect connectivity but cannot
  forge peer identities.
- **Side-channel attacks** — timing or cache side-channels against the TLS/QUIC
  implementation are out of scope (these live in `rustls` / `quinn`).
- **Physical security** — an attacker with access to the host process can read
  keys from memory regardless of what this library does.
