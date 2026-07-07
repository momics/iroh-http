---
id: "016"
title: "mDNS discovery scope: iroh-only, not general DNS-SD"
status: accepted
date: 2026-07-07
area: ecosystem
tags: [discovery, mdns, dns-sd, mobile, address-lookup, iroh, packaging]
---

# [016] mDNS discovery scope: iroh-only, not general DNS-SD

## Context

`iroh-http` gives applications native-HTTP-like behaviour over iroh's QUIC
transport, addressing peers by Ed25519 node id instead of DNS. iroh already
performs local peer discovery over mDNS, and `iroh-http-discovery`
(`crates/iroh-http-discovery`, a standalone optional crate) exposes that to
JS/TS via two verbs: **`advertise`** (announce this node on the LAN) and
**`browse`** (enumerate iroh peers on the LAN). All three adapters
(node, deno, tauri) gate this behind an optional `discovery`/`mdns` feature.

Issue #310 asked a broader question: should we generalise this into a
first-class **DNS-SD** capability — i.e. let `iroh-http` advertise and browse
*arbitrary* services (printers, HTTP servers, non-iroh peers), not just iroh
nodes? A spike (`spikes/mdns-sd-addresslookup`, branch
`momics-spike-mdns-sd-addresslookup`) prototyped "Option Y": replace the
official `iroh-mdns-address-lookup` crate with our own `AddressLookup`
implementation backed by the general-purpose `mdns-sd` responder, which would
let a single mDNS responder serve both iroh discovery (Layer 1) and arbitrary
DNS-SD records (Layer 2).

This record settles the scope question and the packaging question so the
remaining #310 work is well-defined.

## Questions

1. Are `advertise`/`browse` in line with iroh-http's mission, or did we step
   outside it by exposing mDNS at all?
2. Should `iroh-http` support general DNS-SD (browsing/advertising non-iroh
   services), or stay scoped to iroh peers?
3. Should discovery be baked into iroh-http, or is a separate package the
   right home for anything beyond iroh-only discovery?
4. What is actually left to build for cross-platform parity?

## What we know

**iroh core exposes a targeted resolver, not a browser.** In iroh 1.0.x
(repo pins `iroh = "1.0"`, resolving to 1.0.1) the `Endpoint`'s discovery
contract is the `AddressLookup` trait
(`iroh/src/address_lookup.rs:333`) with exactly two methods:

- `publish(&EndpointData)` — fire-and-forget broadcast of *this* node's own
  endpoint info.
- `resolve(endpoint_id) -> Option<BoxStream<Item>>` — look up *one specific*
  endpoint **by a node id you already know**.

iroh core has **no "enumerate every peer on the LAN" API** — it doesn't need
one. Its job is "I want to dial node X, find me an address for X," and
resolved addresses feed the dialer transparently. iroh core does not even
contain mDNS: it is deliberately shipped in the separate
`iroh-mdns-address-lookup` crate (documented at `address_lookup.rs:46-50`).

**The browse/advertise verbs come from the official mDNS crate, scoped to
iroh.** `iroh-mdns-address-lookup` 0.4.0 implements `AddressLookup` and adds,
on top of it:

- `MdnsAddressLookupBuilder::advertise(bool)` (`lib.rs:180`) — the "advertise"
  verb.
- `MdnsAddressLookup::subscribe() -> stream of DiscoveryEvent::{Discovered,
  Expired}` (`lib.rs:255`, `:237`) — the "browse" verb.

`DiscoveryEvent` carries `endpoint_info` / `endpoint_id` — **iroh identities**,
not arbitrary DNS-SD records. So the official crate can only browse *iroh
nodes*; it cannot enumerate printers or non-iroh services. Our
`advertise`/`browse` map directly onto `advertise(true)` and `subscribe()` —
meaning we are surfacing exactly what iroh's own mDNS crate already offers,
already scoped to iroh peers.

**iroh-http-discovery is already cleanly separated.** It is a 229-LoC
single-file crate. `iroh-http-core` does **not** depend on it. It is
`optional = true` behind feature flags in all three adapters. It registers
with the endpoint via `ep.address_lookup()?.add(mdns)` and passes the service
name `"iroh-http"`. Nothing about discovery is baked into core.

**Generalising to DNS-SD is a real, separable step.** Only Option Y (drop the
official crate, hand-write `AddressLookup` over `mdns-sd`) would let a single
responder also carry arbitrary Layer-2 records. The spike proved Option Y is
technically feasible (a second endpoint dialed the first by node id only,
through our `mdns-sd`-backed `AddressLookup`; ~122 LoC; Layer 2 records
round-tripped). But it also proved the cost: we would re-implement `resolve`,
expiry mapping, republish/debounce, and per-interface address selection that
the official crate + `swarm-discovery` handle today, and we would lose the
"blessed by iroh" status. Its only justification was Layer 2.

**Mobile is the one platform where iroh discovery is not yet consistent.**
Desktop (node/deno) gets auto-dial-by-node-id for free through the official
crate. Tauri mobile uses native NWBrowser (iOS) / NsdManager (Android) and
does **not** yet feed discovered peers back into an in-process `AddressLookup`,
so mobile-discovered iroh peers don't auto-dial by node id the way desktop
does. The related pk-TXT vs instance-name interop bug (mobile couldn't see
desktop-advertised nodes) was already fixed in #318 / #320.

## Options considered

| Option | Upside | Downside |
| ------ | ------ | -------- |
| **Keep official crate, iroh-only (chosen)** | Zero new code on desktop; `advertise`/`browse` already map to `advertise()`/`subscribe()`; stays "blessed" by iroh; smallest surface | No general DNS-SD; mobile bridge still needs building |
| Option Y — `mdns-sd` + hand-written `AddressLookup` | One responder serves iroh + arbitrary DNS-SD (Layer 2); sheds `hickory-proto`/`swarm-discovery`/`acto` | Re-implement resolve/expiry/republish; lose "blessed" status; only justified by Layer 2, which we're dropping |
| Fold general DNS-SD into iroh-http | One dependency for users who want both | Bloats a lean, identity-first HTTP library with a general service-discovery concern that has nothing to do with HTTP-over-iroh |

## Decisions

1. **`advertise`/`browse` stay, scoped to iroh peers.** They are in line with
   the mission: they surface iroh's *own* local discovery data, and they map
   one-to-one onto the official crate's `advertise()` / `subscribe()`. This is
   not a step outside iroh-http.

2. **No general DNS-SD in iroh-http.** We will not browse or advertise
   non-iroh services. iroh-http stays lean and identity-first. Anyone needing
   arbitrary DNS-SD can use a native/general package alongside iroh-http.

3. **Keep the official `iroh-mdns-address-lookup` crate on desktop.** No
   `mdns-sd`, no hand-written `AddressLookup`, no Layer 2. **Option Y is
   rejected**; its spike branch (`momics-spike-mdns-sd-addresslookup`) is
   retained as the permanent record of why.

4. **Discovery stays a separate optional crate**, not baked into core — which
   is already the case. Any future general DNS-SD, if ever wanted, is a
   separate standalone package outside iroh-http.

## Consequences

- `#310` collapses from "design a DNS-SD layer" to a single concrete task:
  **mobile parity** — wrap native NWBrowser/NsdManager results in an
  in-process iroh `AddressLookup` (registered via `ep.address_lookup().add()`)
  so mobile-discovered iroh peers auto-dial by node id like desktop. The
  pk-TXT interop piece already shipped (#318/#320).
- No dependency changes: we keep `iroh-mdns-address-lookup`,
  `swarm-discovery`, and their transitive deps. We do not take on `mdns-sd`.
- `iroh-http-discovery`'s API and feature-gating are unchanged. Documentation
  should frame `advertise`/`browse` as "surfacing iroh's own local discovery,"
  and note explicitly that they enumerate iroh nodes only.
- The Option-Y spike crate is kept out-of-workspace (empty `[workspace]`
  table) so it never builds in CI; it exists purely as an ADR reference.

## Next steps

- [ ] Reduce #310 to the mobile `AddressLookup` bridge; re-scope the issue body.
- [ ] Implement the mobile-native → in-process `AddressLookup` bridge (iOS +
      Android) and register it on the endpoint.
- [ ] Document `advertise`/`browse` as iroh-node-scoped in the discovery
      guide / specification.
- [ ] On-device verification of mobile discovery parity (owed since #317/#320
      were reasoned + manual-repro only, not on-device).
