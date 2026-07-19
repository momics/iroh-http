# Reverse Ingress

A device behind CGNAT — a Raspberry Pi, a home server, an ESP32 companion
board — serves content to the internet without port forwarding, a static IP,
or a tunneling service subscription.

## The insight

Conventional ingress requires the server to have a reachable IP address.
iroh-http inverts this: the Pi connects *outward* to the iroh relay, gets a
stable node ID, and any client that knows the node ID can reach it through
NAT traversal or the relay. No router configuration, no DynDNS, no Cloudflare
Tunnel account.

```
Internet client
      │
      │  iroh QUIC (NAT traversal or relay)
      ▼
  Pi node (behind CGNAT)
      │
      │  localhost HTTP
      ▼
  App running on Pi (port 3000)
```

The Pi's **node ID** is its stable address. It doesn't change when the ISP
rotates IPs or when the Pi moves to a different network.

## Pi side

The Pi runs an iroh-http node that proxies inbound iroh requests to a local
service:

```ts
import { IrohNode } from 'iroh-http';

const TARGET = 'http://localhost:3000'; // whatever runs on the Pi

const node = await IrohNode.spawn({ port: 18080 });

node.serve({
  maxConcurrency: 32,
  requestTimeout: 30_000,
}, async (req) => {
  const url = new URL(req.url);
  const upstream = new URL(url.pathname + url.search, TARGET);

  // Forward the request to the local service
  return fetch(upstream.toString(), {
    method: req.method,
    headers: req.headers,
    body: req.method === 'GET' || req.method === 'HEAD' ? undefined : req.body,
    // @ts-ignore — Node.js / Deno fetch: don't follow redirects outward
    redirect: 'manual',
  });
});

// Print the ticket so you can share it
console.log('Node ID:', node.nodeId());
console.log('Ticket:', await node.ticket());
```

The one-line core: `return fetch(upstream, req)`. Everything else is
plumbing.

## Client side

```ts
import { IrohNode } from 'iroh-http';

const node = await IrohNode.spawn();
const PI_NODE_ID = '<paste node ID here>';

// Works exactly like a normal fetch — the fact that it routes through
// QUIC + possible relay is invisible to the rest of the application.
const res = await node.fetch(`iroh://${PI_NODE_ID}:18080/api/sensor-data`);
const data = await res.json();
```

Or use the ticket string from `node.ticket()`, which the Pi can print to a
QR code on its OLED display:

```ts
const res = await node.fetch(`iroh://${piTicket}/api/sensor-data`);
```

See [device-handoff.md](device-handoff.md) for the QR-code distribution
pattern.

## Access control

Without restriction, anyone who learns the node ID can reach your Pi. Add a
[capability token](capability-tokens.md) check as middleware:

```ts
node.serve({}, compose(
  requireToken(trustedPublicKey),
  async (req) => fetch(new URL(new URL(req.url).pathname, TARGET).toString(), {
    method: req.method,
    headers: req.headers,
    body: req.body,
  }),
));
```

Or use [proximity trust](proximity-trust.md) to allow LAN clients (same
home network) unrestricted access while requiring tokens from relayed
connections:

```ts
node.serve({}, (req) => {
  const tier = trustTier(req);
  if (tier === 'relayed' && !hasValidToken(req)) {
    return new Response('Forbidden', { status: 403 });
  }
  return fetch(new URL(new URL(req.url).pathname, TARGET).toString(), req);
});
```

## Relay fallback and direct paths

iroh tries direct connection (UDP hole punching) before falling back to the
relay. For devices on the same home network, the connection is direct and
sub-millisecond. For a client on a different network, the relay adds latency
comparable to a CDN edge node — typically 20–80 ms depending on geography.

You don't write any code to control this. The library handles it.

## Streaming and large responses

The Pi proxy passes `req.body` and the upstream response body as streams. For
a camera feed or a large file download, no buffering occurs:

```ts
node.serve({}, async (req) => {
  const upstream = await fetch(new URL(new URL(req.url).pathname, TARGET).toString(), {
    method: req.method,
    headers: req.headers,
    body: req.body,
  });
  // Response body is streamed directly — never fully buffered in the proxy
  return new Response(upstream.body, {
    status: upstream.status,
    headers: upstream.headers,
  });
});
```

Enable compression to reduce relay bandwidth:

```ts
const node = await createNode({ compression: true });
```

See [body compression](../features/compression.md).

## Multi-service Pi

Route different paths to different local services:

```ts
const services: Record<string, string> = {
  '/cam':     'http://localhost:8554',  // camera MJPEG stream
  '/sensors': 'http://localhost:3001',  // sensor API
  '/files':   'http://localhost:3002',  // file server
};

node.serve({}, async (req) => {
  const path = new URL(req.url).pathname;
  const [prefix, target] = Object.entries(services)
    .find(([p]) => path.startsWith(p)) ?? ['', null];

  if (!target) return new Response('Not Found', { status: 404 });

  const upstreamPath = path.slice(prefix.length) || '/';
  return fetch(target + upstreamPath, {
    method: req.method,
    headers: req.headers,
    body: req.body,
  });
});
```

See [http-gateway.md](http-gateway.md) for the full gateway pattern including
header rewriting.

## Advertising the Pi

Let other nodes on the same LAN find the Pi automatically, without knowing
its node ID in advance:

```ts
node.advertise({ signal: ac.signal });
```

Any other iroh-http node on the Wi-Fi will see the Pi in `node.browse()`
events. See [local-first-sync.md](local-first-sync.md) for how to act on
discovery events.

## What this is not

- Not a full VPN — only HTTP(S) traffic is proxied, not arbitrary TCP/UDP.
- Not load-balanced — for multiple Pi units, maintain a list of node IDs and
  use the [peer fallback](peer-fallback.md) pattern.
- Not persistent storage — the Pi must be on for requests to succeed. For
  blob storage that survives the Pi going offline, see
  [cooperative-backup.md](cooperative-backup.md).
