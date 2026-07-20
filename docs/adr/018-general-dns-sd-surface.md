---
id: "018"
title: "General DNS-SD surface with an iroh-http specialization"
status: accepted
date: 2026-07-09
area: api
tags: [discovery, dns-sd, mdns, ffi, api]
supersedes: ["016"]
---

# [018] General DNS-SD surface with an iroh-http specialization

> Builds on [ADR-017](017-standard-dns-sd-discovery.md) (standard DNS-SD wire
> format via `mdns-sd`) and reverses decisions 1 & 2 of the superseded
> [ADR-016](016-mdns-discovery-scope.md) ("no general DNS-SD"): the wire format
> is already standard, so exposing a generic, lossless advertise/browse surface
> is now cheap and additive.

## Context

`iroh-http-discovery` speaks standard DNS-SD on the wire (ADR-fixed for #329:
`PTR` + `SRV` + `TXT` + `A`/`AAAA` under `_<service>._udp.local.`), which is
what made desktop nodes visible to iOS `NWBrowser` / Android `NsdManager`.

But the *public API* is specialized for iroh peer discovery and is lossy:

- **Advertise** forces the instance name to the base32 endpoint id, takes the
  port from the endpoint, and emits peer TXT (`pk` plus optional `relay` and
  dialable `address` values). A caller cannot advertise a non-iroh service, set
  a custom instance name, or attach custom TXT through this specialization.
- **Browse** (`resolved_to_event`) reads only `pk`/`relay` and **discards**
  every other TXT property, the port, the instance name, and the host. The
  `PeerDiscoveryEvent` / `DiscoveredPeer` types expose only `nodeId`, `addrs`,
  `isActive`.

We want to expose *generic* DNS-SD advertise/browse to JS/TS users while keeping
the ergonomic, zero-config iroh-http path that makes `fetch(nodeId)` resolve LAN
peers. The overriding constraint from design discussion: **do not build two
parallel bridge systems.** There must be one engine and one FFI plumbing, with
the iroh-http behavior as a thin layer on top.

## Questions

1. One surface (`node.advertise`/`node.browse` only) or two (a generic module +
   node specializations)?
2. Can the iroh-http path be implemented purely on top of the generic bridge, or
   does it require its own FFI seam?
3. What is the generic record/config shape, and how does the iroh path map onto
   it without losing DNS-SD fields?
4. `_udp` only, or first-class `_tcp` now that non-iroh services are supported?

## What we know

- Generic DNS-SD advertise (`ServiceInfo::new(...).enable_addr_auto()`) needs no
  `iroh::Endpoint`. Generic browse needs no endpoint either.
- The iroh-http path needs the endpoint for **two** things that cannot move to
  JS reliably:
  - **advertise:** the authoritative SRV port is `ep.bound_sockets()`. Parsing
    it out of `ep.addr()` strings is fragile (relay-only nodes have no direct
    socket addr).
  - **browse:** LAN `fetch(nodeId)` works because `browse_peers` registers an
    in-process `AddressLookup` on the endpoint and feeds it from the browse
    pump. This is a registered provider fed over time — inherently a Rust-side
    seam, not a per-record JS call.
- `mdns-sd` `ServiceInfo` carries instance, host, port, `addresses`, and full
  `txt_properties`. Nothing forces us to drop fields.
- DNS-SD instance labels must be < 64 bytes; iroh ids are emitted as base32
  (52 chars) to fit, and the `pk` TXT carries the same id.

## Options considered

| Option | Upside | Downside |
| ------ | ------ | -------- |
| Single overloaded `node.advertise`/`node.browse` (generic via config, iroh via defaults) | one surface | one method with two meanings; the iroh path silently does more (address-lookup wiring, `pk` TXT); dishonest signature |
| Standalone `advertiseService`/`browseService` top-level exports | flat | the FFI is loaded through the node addon, so a node-free export needs a second per-runtime init path — the "two bridges" we forbade |
| `dnsSd` sub-object (generic) + `node.advertise`/`node.browse` (iroh) | honest signatures, one obvious escape hatch | a `dnsSd` namespace implies a separability the surface does not have (it still needs a node) |
| **Generic `node.advertise`/`node.browse` + `node.advertisePeer`/`node.browsePeers`** | distinct names, no overloading, and the API mirrors the engine → specialization layering | the common (peer) path gets the longer name |

## Decisions

1. **Generic engine on `node`, iroh path renamed.** Expose the generic engine
   directly as `node.advertise(config)` / `node.browse(config)` (modern
   async-iterator / disposable sessions driven by `AbortSignal`). The iroh-http
   path is the **specialization** `node.advertisePeer()` / `node.browsePeers()`,
   which fills in the reserved service name, the endpoint port, and the `pk`
   TXT, wires the endpoint `AddressLookup`, and yields a `DiscoveredPeer` rather
   than a raw `ServiceRecord`. Distinct names avoid an overloaded `advertise`
   and make the API mirror the layering: the generic engine is the primitive;
   the peer path is the specialization that does strictly more.

2. **One engine, thin iroh seam.** Rust exposes a generic engine yielding full
   `ServiceRecord` values. `advertise_peer` builds a canonical `ServiceConfig`
   from the endpoint; `browse_peers` projects the generic record stream into a
   source-scoped endpoint `AddressLookup`. Desktop and mobile then differ only
   in the transport implementation below that engine seam.

3. **Two FFI entry points, one engine.** Because the endpoint is genuinely
   required for the iroh path (authoritative port; address-lookup wiring), we
   keep endpoint-bound `advertisePeer`/`browsePeers` *and* add endpoint-free
   `advertise`/`browse`/`browseNext`. These are thin entry points
   over the single `dns_sd` engine — not a second bridge. This is the accepted
   "separate FFI calls where required" from the design decision.

4. **Lossless generic record; deliberately small peer record.**
   `ServiceRecord` carries `serviceType`, `instanceName`, `host`, `port`,
   `addrs`, `txt`, and `isActive`. `DiscoveredPeer` remains the stable
   `{ nodeId, addrs, isActive }` projection. Generic `node.advertise({ txt })`
   preserves caller TXT; the peer specialization alone owns its reserved
   `pk`/`relay`/`address` keys.

5. **`protocol` is first-class.** `ServiceConfig`/`BrowseConfig` take
   `protocol?: "udp" | "tcp"` (default `udp`). The iroh path stays `udp`.

6. **Interop bridge.** Export the convention so the two worlds meet:
   `IROH_HTTP_SERVICE` and the reserved TXT keys, plus `asIrohPeer(record)` to
   recognize an iroh-http peer inside a generic browse.

7. **One discovery permission.** The Tauri plugin previously shipped two
   permission sets — `iroh-http:mdns` (peer discovery) and `iroh-http:dns-sd`
   (generic). Since the peer path is a specialization of the generic one,
   collapse them into one `iroh-http:discovery` set covering both public command
   families. A capability grants discovery once and gets both.

8. **Mobile parity.** Generic DNS-SD is implemented on mobile, not just
   desktop. The native plugins expose one generic advertise/browse adapter over
   `NsdManager` (Android) or `NWBrowser`/`NetService` (iOS); peer APIs reuse it
   through the Rust projection instead of adding native peer engines. Android
   resolves full records via `resolveService`; iOS surfaces instance, service
   type, and TXT but leaves host/port/addresses unresolved because doing more
   requires an `NWConnection` per result.

Accepted trade-off: generic DNS-SD via this library requires an iroh node
(the FFI is loaded through the node addon). This is fine — the library exists to
serve iroh-http, not to be a standalone Bonjour replacement.

## Consequences

- Rust `iroh-http-discovery`: new `dns_sd` module (config, record, engine);
  `advertise_peer`/`browse_peers` refactored onto it. Public API additive.
- FFI (node napi, deno dispatch, tauri desktop): new generic
  `advertise`/`browse`/`browseNext` entry points and a `JsServiceRecord`.
- Mobile tauri: generic DNS-SD bridged to native NsdManager / NWBrowser, at
  parity with desktop (iOS records are metadata-only — see decision 8).
- Tauri permissions: `iroh-http:mdns` and `iroh-http:dns-sd` are replaced by a
  single `iroh-http:discovery` set (breaking; capabilities must be updated).
- Shared TS: generic `node.advertise`/`node.browse` + iroh-http
  `node.advertisePeer`/`node.browsePeers`, `ServiceConfig`/`ServiceRecord` types,
  generic adapter methods, and interop helpers. `DiscoveredPeer` stays minimal.
- Docs: discovery feature doc updated; examples may show a non-iroh service.

## Next steps

- [x] Rust `dns_sd` engine + refactor + unit tests.
- [x] node napi generic FFI entries.
- [x] deno dispatch generic FFI entries.
- [x] tauri desktop generic commands.
- [x] shared TS: generic `node.advertise`/`node.browse` + iroh `advertisePeer`/`browsePeers`, types, adapter methods, interop helpers.
- [x] discovery feature doc + example (deno / node / tauri).
- [x] unify `iroh-http:mdns` + `iroh-http:dns-sd` into `iroh-http:discovery`.
- [x] mobile generic DNS-SD (Android full records; iOS metadata-only) and Rust peer projection — pending on-device verification.

## Naming convention (as shipped)

The final implementation names every layer along the **peer-vs-generic axis**,
not the misleading `mdns` / `dns_sd` transport split it was prototyped under
(both paths are DNS-SD-over-mDNS; the real distinction is the generic primitive
versus the iroh-peer specialization that adds `AddressLookup` wiring). Names
mirror the TypeScript API surface at every layer:

| Layer | Generic primitive | Peer specialization |
| ----- | ----------------- | ------------------- |
| TS API (`node`) | `advertise` / `browse` | `advertisePeer` / `browsePeers` |
| Rust core seam (`iroh-http-discovery`) | `dns_sd::advertise` / `dns_sd::browse` | `advertise_peer` / `browse_peers` |
| Node napi export | `advertise` / `browse` / `browseNext` / `advertiseClose` / `browseClose` | `advertisePeer` / `browsePeers` / `browsePeersNext` / `advertisePeerClose` / `browsePeersClose` |
| Deno dispatch key | `advertise` / `browse` / `browseNext` / … | `advertisePeer` / `browsePeers` / `browsePeersNext` / … |
| Tauri command | `advertise` / `browse` / `browse_next` / … | `advertise_peer` / `browse_peers` / `browse_peers_next` / … |
| Tauri permission leaf | `allow-advertise` / `allow-browse` / `allow-browse-next` / … | `allow-advertise-peer` / `allow-browse-peers` / `allow-browse-peers-next` / … |
| Mobile native method | `advertise_start/update/stop` / `browse_start/poll/stop` | None — peer APIs project the generic engine in Rust |

The single `iroh-http:discovery` permission set and the high-level `node`
methods are the stable surface; the FFI export, Tauri command, permission-leaf,
and mobile-native names below them are renamed together and are breaking for
low-level consumers. The `DnsSdAdvertiseOptions` / `DnsSdBrowseOptions` option
types keep their names — they describe generic DNS-SD configuration and remain
accurate.
