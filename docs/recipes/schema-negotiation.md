# Schema and Version Negotiation

Real deployments have mixed client versions. Newer nodes speak a richer
protocol; older ones only know the original. Negotiation lets both coexist
without a forced upgrade cycle.

## The insight

In a centralised system, you upgrade the server and everyone gets the new API
at once. In a P2P network, nodes upgrade independently. Your laptop might be
on v2 of your app; your friend's phone is still on v1; the Raspberry Pi in
the corner has been running since before v1 existed and nobody remembered to
update it.

This isn't a bug — it's the correct model for software that doesn't have a
central server to force upgrades. The recipes here show how to negotiate
gracefully so a v2 node can talk to a v1 node at the v1 protocol level, and
two v2 nodes can use the richer v2 features without any explicit coordination.

```
A (v2) connects to B (v1):
  A → OPTIONS /.well-known/api → B
  B ← { versions: ["1.0"] }
  A: "I'll speak v1" → continues with v1 schema
  
A (v2) connects to C (v2):
  A → OPTIONS /.well-known/api → C
  C ← { versions: ["1.0", "2.0"] }
  A: "We both speak v2" → continues with v2 schema
```

## Capability document

Every node publishes a machine-readable API description:

```ts
interface ApiCapabilities {
  versions: string[];                    // semver strings, oldest first
  extensions?: string[];                 // named optional features
  maxRequestBodyWireBytes?: number;
  maxRequestBodyDecodedBytes?: number;
  supportsCompression?: boolean;
  supportsStreaming?: boolean;
  supportsWebTransport?: boolean;
}
```

```ts
node.serve({}, async (req) => {
  if (req.method === 'GET' && new URL(req.url).pathname === '/.well-known/api') {
    return Response.json({
      versions: ['1.0', '2.0'],
      extensions: ['compression', 'streaming'],
      supportsCompression: true,
      supportsStreaming: true,
      supportsWebTransport: false,
    } satisfies ApiCapabilities);
  }
  // ...
});
```

## Negotiating on connect

Before making substantive requests, the client fetches the capability document
and picks the best mutually supported version:

```ts
const MY_VERSIONS = ['2.0', '1.0']; // preferred first

async function negotiate(
  node: IrohNode,
  peerNodeId: string,
): Promise<{ version: string; caps: ApiCapabilities }> {
  const res = await node.fetch(`iroh://${peerNodeId}/.well-known/api`, {
    signal: AbortSignal.timeout(3000),
  });

  if (!res.ok) {
    // Old peer that doesn't serve the capability document — assume v1
    return { version: '1.0', caps: { versions: ['1.0'] } };
  }

  const caps: ApiCapabilities = await res.json();

  const agreed = MY_VERSIONS.find((v) => caps.versions.includes(v));
  if (!agreed) throw new Error(
    `No common protocol version. Peer speaks: ${caps.versions.join(', ')}`
  );

  return { version: agreed, caps };
}
```

## Version-dispatched handlers

On the server, route requests to the appropriate handler based on the
`x-api-version` header the client sends:

```ts
node.serve({}, async (req) => {
  const apiVersion = req.headers.get('x-api-version') ?? '1.0';

  if (new URL(req.url).pathname === '/sync') {
    if (apiVersion === '2.0') return handleSyncV2(req);
    return handleSyncV1(req);
  }
  // ...
});

// Client always sends the negotiated version
async function fetchWithVersion(
  node: IrohNode,
  peerNodeId: string,
  negotiated: string,
  path: string,
  init?: RequestInit,
): Promise<Response> {
  return node.fetch(`iroh://${peerNodeId}${path}`, {
    ...init,
    headers: {
      ...Object.fromEntries(new Headers(init?.headers)),
      'x-api-version': negotiated,
    },
  });
}
```

## Schema evolution patterns

### Additive changes (backwards compatible)

New fields in responses — old clients ignore them; new clients read them.
No version bump required.

```ts
// v1 response: { id: string, name: string }
// v2 response: { id: string, name: string, avatar?: string }  ← ok to add
```

### Non-additive changes (require version bump)

Renamed fields, changed types, removed fields. Serve both under the same
endpoint, dispatched by version:

```ts
function serializeDoc(doc: Doc, apiVersion: string): unknown {
  if (apiVersion === '2.0') {
    return { id: doc.id, displayName: doc.name, avatarUrl: doc.avatar };
    //                    ^ renamed from v1's "name"
  }
  return { id: doc.id, name: doc.name }; // v1 shape
}
```

### Capability negotiation for optional features

Check a specific capability before using it:

```ts
const { version, caps } = await negotiate(node, peerNodeId);

// Only request compressed responses if the peer advertises support
const headers: Record<string, string> = { 'x-api-version': version };
if (caps.supportsCompression) {
  headers['Accept-Encoding'] = 'zstd';
}

const res = await node.fetch(`iroh://${peerNodeId}/data`, { headers });
```

## Caching negotiated capabilities

Negotiating on every connection is cheap (~1 RTT) but avoidable if the peer
set is stable:

```ts
const negotiatedCaps = new Map<string, { version: string; caps: ApiCapabilities; expiresAt: number }>();

async function getOrNegotiate(
  node: IrohNode,
  peerNodeId: string,
): Promise<{ version: string; caps: ApiCapabilities }> {
  const cached = negotiatedCaps.get(peerNodeId);
  if (cached && cached.expiresAt > Date.now()) {
    return { version: cached.version, caps: cached.caps };
  }

  const result = await negotiate(node, peerNodeId);
  negotiatedCaps.set(peerNodeId, { ...result, expiresAt: Date.now() + 300_000 }); // 5 min
  return result;
}
```

## Rolling upgrades

When upgrading a group of peers, upgrade one at a time. Each upgraded peer can
still talk v1 to unupgraded peers and v2 to upgraded ones. Full migration
happens naturally as all peers upgrade, with no coordination required.

```
Before:  A(v1) — B(v1) — C(v1)
Step 1:  A(v1) — B(v1) — C(v2)   C speaks v1 to B
Step 2:  A(v1) — B(v2) — C(v2)   B speaks v2 to C, v1 to A
Step 3:  A(v2) — B(v2) — C(v2)   all speak v2; v1 support can be deprecated
```

## Deprecating old versions

When you want to drop v1 support:

1. Publish the deprecation in the capability document with a `deprecatedAt` date.
2. Serve v1 but return a `Deprecation` header so clients can log it.
3. After the cutoff, drop the v1 handler.

```ts
// Signal deprecation before dropping
if (apiVersion === '1.0') {
  // Serve the v1 response but warn
  const response = await handleSyncV1(req);
  response.headers.set('Deprecation', 'true');
  response.headers.set('Sunset', 'Sat, 01 Jan 2027 00:00:00 GMT');
  return response;
}
```

## Bootstrapping

**Starting empty:** publish `versions: ['1.0']` from day one, even if there's
only one version. This makes adding v2 later a no-op for existing peers — they
already know how to negotiate.

**When a peer has no capability document:** assume v1 and proceed. Never fail
hard on missing `/.well-known/api` — many peers predate this recipe.

## When not to use this pattern

For small projects with a single developer and a few devices that all update
together, this is over-engineering. Skip negotiation and just deploy the new
version. Schema negotiation matters when: the peer set is large (> ~10 nodes),
peers upgrade independently, or the software has external users who control
their own upgrade schedule.

## See also

- [Capability advertisement](capability-advertisement.md) — includes
  `supportsStreaming`, `supportsWebTransport` in the roles descriptor;
  capability advertisement and schema negotiation are complementary
- [Release channels](release-channels.md) — how new versions are distributed
  to peers; schema negotiation handles the transition period after a release
- [Ecosystem overview](ecosystem.md) — schema negotiation is what makes the
  ecosystem survive real-world deployment where not all nodes update in
  lockstep
