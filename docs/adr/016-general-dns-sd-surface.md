---
id: "016"
title: "General DNS-SD surface with an iroh-http specialization"
status: accepted
date: 2026-07-09
area: api
tags: [discovery, dns-sd, mdns, ffi, api]
---

# [016] General DNS-SD surface with an iroh-http specialization

## Context

`iroh-http-discovery` speaks standard DNS-SD on the wire (ADR-fixed for #329:
`PTR` + `SRV` + `TXT` + `A`/`AAAA` under `_<service>._udp.local.`), which is
what made desktop nodes visible to iOS `NWBrowser` / Android `NsdManager`.

But the *public API* is specialized for iroh peer discovery and is lossy:

- **Advertise** forces the instance name to the base32 endpoint id, takes the
  port from `ep.bound_sockets()`, and emits a fixed TXT set (`pk` + optional
  `relay`). A caller cannot advertise a non-iroh service, set a custom instance
  name, or attach custom TXT.
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
  - **browse:** LAN `fetch(nodeId)` works because `start_browse` registers an
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
| Node methods only; generic via config, iroh via helpers | one surface | `node.advertise(config)` implies an endpoint coupling generic DNS-SD does not have; dishonest signature |
| Standalone `advertiseService`/`browseService` + node methods | honest, flat | two flat exports to discover; naming collides conceptually with node methods |
| **`dnsSd` object (generic) + `node.*` specialization** | honest signatures, one obvious escape hatch, node stays the nudge, adjustable later | two places to look (mitigated by docs) |

## Decisions

1. **Expose a `dnsSd` object** on the TS side: `dnsSd.advertise(config)` and
   `dnsSd.browse(config)`. Both return modern async-iterator / disposable
   session interfaces driven by `AbortSignal`, matching `node.browse`.
   `node.advertise` / `node.browse` remain and are defined as the iroh-http
   **specialization** of the generic engine.

2. **One engine, thin iroh seam.** Rust exposes a generic engine
   `dns_sd::advertise(ServiceConfig)` and `dns_sd::browse(BrowseConfig)` yielding
   a full `ServiceRecord`. `start_advertise`/`start_browse` become ~15-line
   adapters: advertise builds a `ServiceConfig` from the endpoint; browse calls
   the generic engine and additionally registers + feeds the `AddressLookup`.
   Both funnel through the same daemon slab, pump, and record marshalling.

3. **Two FFI entry points, one engine.** Because the endpoint is genuinely
   required for the iroh path (authoritative port; address-lookup wiring), we
   keep endpoint-bound `mdnsAdvertise`/`mdnsBrowse` *and* add endpoint-free
   `dnsSdAdvertise`/`dnsSdBrowse`/`dnsSdNextRecord`. These are thin entry points
   over the single `dns_sd` engine — not a second bridge. This is the accepted
   "separate FFI calls where required" from the design decision.

4. **Lossless record + merged TXT.** `ServiceRecord` carries `serviceName`,
   `instanceName`, `host`, `port`, `addrs`, `txt`, and `isActive`.
   `DiscoveredPeer` gains optional `txt`/`port`/`instanceName`/`host` so the
   iroh path is no longer lossy. `node.advertise({ txt })` merges caller TXT on
   top of the reserved `pk`/`relay` keys (reserved keys win).

5. **`protocol` is first-class.** `ServiceConfig`/`BrowseConfig` take
   `protocol?: "udp" | "tcp"` (default `udp`). The iroh path stays `udp`.

6. **Interop bridge.** Export the convention so the two worlds meet:
   `IROH_HTTP_SERVICE` and the reserved TXT keys, plus `asIrohPeer(record)` to
   recognize an iroh-http peer inside a generic browse.

Accepted trade-off: generic DNS-SD via this library requires an iroh node
(the FFI is loaded through the node addon). This is fine — the library exists to
serve iroh-http, not to be a standalone Bonjour replacement.

## Consequences

- Rust `iroh-http-discovery`: new `dns_sd` module (config, record, engine);
  `start_advertise`/`start_browse` refactored onto it. Public API additive.
- FFI (node napi, deno dispatch, tauri desktop): new `dnsSd*` entry points and a
  `JsServiceRecord`. Mobile tauri unaffected for now.
- Shared TS: `dnsSd` object, `ServiceConfig`/`ServiceRecord`/`DiscoveredService`
  types, `IrohAdapter` gains generic methods, `DiscoveredPeer` gains optional
  fields, interop helpers exported.
- Docs: discovery feature doc updated; examples may show a non-iroh service.

## Next steps

- [x] Rust `dns_sd` engine + refactor + unit tests.
- [x] node napi generic FFI entries.
- [x] deno dispatch generic FFI entries.
- [x] tauri desktop generic commands.
- [x] shared TS `dnsSd` object, types, adapter methods, interop helpers.
- [ ] discovery feature doc + example.
