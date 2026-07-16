# Peer Discovery

Two mechanisms for discovering other iroh-http nodes:

- **DNS discovery** — global, always-on. Any node that publishes its address
  via Pkarr can be resolved by public key using standard DNS.
- **mDNS** — local network. Nodes announce their presence on the LAN and can
  find each other without internet connectivity, via `node.advertisePeer()` and
  `node.browsePeers()`.

## DNS discovery

DNS discovery is enabled by default and configured at node creation:

```ts
await createNode({
  // Default: true — uses n0's hosted DNS infrastructure.
  dns: true,

  // Custom resolver:
  // dns: { resolverUrl: 'https://dns.example.com' },

  // Disable entirely (air-gapped / embedded):
  // dns: false,
});
```

When enabled, node startup automatically publishes a signed Pkarr record
containing the node's relay URL and direct socket addresses. On `node.fetch`,
if the peer's address isn't already known, Iroh resolves it via DNS before
the QUIC handshake — transparently, with no extra code.

## `node.advertisePeer()`

Announce this node as a discoverable iroh-http peer on the local network via
mDNS until the signal fires:

```ts
const controller = new AbortController();
node.advertisePeer({ serviceName: 'my-app', signal: controller.signal });

// Stop advertising:
controller.abort();
```

Returns a `Promise<void>` that resolves when advertising stops. Calling it
without a signal advertises until the node is closed.

## `node.browsePeers()`

Discover peers on the local network as an async iterable:

```ts
for await (const event of node.browsePeers({ serviceName: 'my-app' })) {
  if (event.isActive) {
    console.log('found peer:', event.nodeId, event.addrs);
  } else {
    console.log('peer left:', event.nodeId);
  }
}
```

See [`PeerDiscoveryEvent` in the specification](../specification.md#discovery-mdns) for the event shape.

Cancel by passing an `AbortSignal` or by breaking from the loop — both clean
up the underlying mDNS listener:

```ts
const controller = new AbortController();
for await (const event of node.browsePeers({ signal: controller.signal })) { ... }
controller.abort();

// Or just break:
for await (const event of node.browsePeers({ serviceName: 'my-app' })) {
  if (done) break;
}
```

## mDNS options

See [`MdnsOptions` in the specification](../specification.md#discovery-mdns) for the option shape.

`browsePeers` and `advertisePeer` accept `MdnsOptions`. Both can run
simultaneously on the same node — they are independent.

## Generic DNS-SD (`node.advertise()` / `node.browse()`)

`node.advertisePeer()` / `node.browsePeers()` are the ergonomic, zero-config
path for finding **iroh-http peers**: the instance name is the node id, the port
comes from the endpoint, and the TXT set is fixed (`pk` + optional `relay`).

To advertise or browse **any** DNS-SD service — a printer, a game lobby, a
non-iroh daemon — use the generic `node.advertise()` / `node.browse()`
primitives. They are the same wire protocol and the same underlying engine, but
lossless and fully caller-controlled:

```ts
const ac = new AbortController();

// Advertise an arbitrary service.
await node.advertise({
  serviceName: "printers",     // → _printers._tcp.local.
  instanceName: "Front Desk",
  port: 9100,
  protocol: "tcp",             // "udp" (default) | "tcp"
  txt: { model: "LaserJet 9000", color: "true" },
  signal: ac.signal,
});

// Browse — records are lossless: instance, host, port, addrs, and every TXT key.
for await (const rec of node.browse({ serviceName: "printers", protocol: "tcp" })) {
  console.log(rec.isActive ? "up" : "down", rec.instanceName, rec.port, rec.txt);
}
```

`node.advertisePeer()` / `node.browsePeers()` are defined as the iroh-http
*specialization* of these generic primitives — there is one bridge, not two. The
generic surface lives on the node because the native discovery FFI is loaded
through the node addon (per [ADR-018](../adr/018-general-dns-sd-surface.md)); it
does not otherwise use the node's identity or endpoint.

### Recognizing iroh-http peers in a generic browse

iroh-http advertises under a known service name with the node id in a `pk` TXT
property. `asIrohPeer(record)` reinterprets a generic `ServiceRecord` as a
`DiscoveredPeer` when it carries one, and returns `null` otherwise:

```ts
import { asIrohPeer, IROH_HTTP_SERVICE } from "@momics/iroh-http-node";

for await (const rec of node.browse({ serviceName: IROH_HTTP_SERVICE })) {
  const peer = asIrohPeer(rec);
  if (peer) await node.fetch(`httpi://${peer.nodeId}/api`);
}
```

## Interop with iroh's built-in mDNS

iroh-http discovery is **not** interoperable with plain-iroh's built-in mDNS
(`swarm-discovery` / `iroh-mdns-address-lookup`). The two use different wire
formats: iroh-http speaks standard DNS-SD with a `PTR` record so it is browsable
by Bonjour, iOS `NWBrowser`, and Android `NsdManager`, whereas iroh's own
backend omits `PTR` and is invisible to those browsers. This trade — dropping
plain-iroh mDNS interop to gain standards-compliant, mobile-visible discovery —
is the whole point of the change; see
[ADR-017](../adr/017-standard-dns-sd-discovery.md) for the wire-format decision.

Nodes on the same LAN still connect fine regardless: once a peer's node id and
addresses are known (via DNS discovery, a ticket, or a direct address), the QUIC
handshake is identical. Only the *mDNS enumeration* differs.

## Without discovery

When DNS is disabled and neither `browse` nor `advertise` is called, the node
operates in explicit-address mode. Connections must use direct hints
(`directAddrs`), a home-relay hint (`relayUrl`), or ticket strings (see
[tickets](tickets.md)).

Appropriate for embedded targets, air-gapped networks, and integration tests.

## Platform support

| Feature | Node / Deno / Tauri |
|---------|:---:|
| **DNS discovery** (auto-resolve by public key) | ✅ |
| **`advertise()`** | ✅ (AbortSignal) |
| **`browse()`** | ✅ (async iterable + AbortSignal) |

> **Feature flag:** mDNS browse and advertise require the `mdns` compile-time
> feature in all Rust adapters.

## Mobile setup (iOS & Android)

Discovery speaks standard [DNS-SD](https://datatracker.ietf.org/doc/html/rfc6763)
over mDNS (`PTR` + `SRV` + `TXT` + `A`/`AAAA`), so a desktop node is browsable
by Apple's `NWBrowser` and Android's `NsdManager` — see
[ADR-017](../adr/017-standard-dns-sd-discovery.md) for why this wire format was
chosen.

Tauri apps must declare local-network permissions and Bonjour service types
before mDNS works on a device. See
[Mobile mDNS / DNS-SD setup](../guidelines/mobile-mdns-setup.md) for the exact
iOS `Info.plist` and Android `AndroidManifest.xml` entries.

Mobile has one native generic browse engine and one native generic advertisement
engine. The peer APIs use those same adapters, then apply the iroh-specific TXT,
identity, and endpoint-lookup projection in Rust. Android resolves full generic
records (host, port, TXT, addresses); iOS surfaces the instance name, service
type and TXT but leaves host/port/addresses unresolved (`NWBrowser` does not
resolve endpoints without an `NWConnection`). A single `iroh-http:discovery`
capability permission grants both paths.
