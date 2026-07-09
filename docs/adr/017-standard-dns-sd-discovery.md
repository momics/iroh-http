---
id: "017"
title: "Standard DNS-SD discovery via mdns-sd (supersedes 016 §3)"
status: accepted
date: 2026-07-08
area: ecosystem
tags: [discovery, mdns, dns-sd, mobile, address-lookup, iroh, ptr]
supersedes: ["016"]
---

# [017] Standard DNS-SD discovery via `mdns-sd`

## Context

[ADR-016](016-mdns-discovery-scope.md) settled that `iroh-http-discovery` should
stay iroh-scoped and keep the official `iroh-mdns-address-lookup` crate (backed
by `swarm-discovery`), explicitly **rejecting** "Option Y" — replacing it with
the general-purpose `mdns-sd` responder. That decision assumed the official
crate produced standard, browsable DNS-SD records.

Issue #329 disproved that assumption. On-wire inspection of a desktop node
advertised through `iroh-mdns-address-lookup` / `swarm-discovery` showed it
publishes `SRV` + `TXT` + `A`/`AAAA` but **no `PTR` record**. `PTR` is the
record a DNS-SD *browse* enumerates (`_<service>._udp.local` → instance names).
Without it, Apple's mDNSResponder (`dns-sd -B`, iOS `NWBrowser`) and Android's
`NsdManager` cannot enumerate the node at all: desktop iroh-http nodes were
invisible to every standard DNS-SD browser, including our own mobile adapters.
This is the exact cross-platform parity gap ADR-016 deferred to "the mobile
`AddressLookup` bridge" — but the root cause is on the **desktop advertiser**,
not the mobile browser.

## Questions

1. Can the missing `PTR` be fixed while keeping `iroh-mdns-address-lookup`?
2. If not, does adopting `mdns-sd` (previously rejected) now carry its weight?
3. What must be preserved so node/deno/tauri-desktop consumers are unaffected?

## What we know

- **The gap is structural, not configuration.** `swarm-discovery` is a
  gossip-style peer set, not an RFC 6763 responder; it does not register the
  `PTR` pointer records a DNS-SD browse relies on. There is no option on the
  official crate to emit them.
- **`mdns-sd` is a standards-complete responder.** It publishes `PTR` + `SRV` +
  `TXT` + `A`/`AAAA` and browses by service type, which is exactly what iOS
  `NWBrowser` and Android `NsdManager` speak.
- **Verified on-wire (2026-07-08, macOS mDNSResponder).** With the `mdns-sd`
  rewrite, `dns-sd -B _<svc>._udp local` enumerates the node on every interface,
  and `dns-sd -L` resolves `SRV <id>.local.:<quic-port>` plus `TXT pk=<id>`.
  This is the record set `swarm-discovery` never emitted.
- **Node ids must be base32, not iroh's hex `Display`.** iroh's 64-char hex
  endpoint id exceeds the 63-byte DNS label limit and makes `mdns-sd` panic. The
  rest of iroh-http already uses 52-char lowercase RFC 4648 base32, and iroh's
  `PublicKey::FromStr` accepts base32, so ids still round-trip through the
  in-process `AddressLookup`.
- **The public API is unchanged.** `start_advertise` / `start_browse` /
  `BrowseSession` / `AdvertiseSession` / `PeerDiscoveryEvent` / `DiscoveryError`
  keep their signatures, so node, deno, and tauri-desktop consumers are
  untouched. Mobile targets already gate the crate out. (ADR-018 later renamed
  the core seam functions to `advertise_peer` / `browse_peers`; signatures were
  unaffected.)
- **Everything ADR-016 wanted to avoid still holds.** We re-implement `resolve`,
  expiry, and address selection — but that is now the *only* way to be
  browsable, and the desktop `MdnsSdAddressLookup` (parity with the mobile one)
  keeps `fetch(nodeId)` auto-dial working.

## Options considered

| Option | Upside | Downside |
| ------ | ------ | -------- |
| Keep `iroh-mdns-address-lookup` (ADR-016 §3) | "Blessed by iroh"; no rewrite | Emits no `PTR`; desktop invisible to every DNS-SD browser — fails the mission |
| **Adopt `mdns-sd` (chosen)** | Standard `PTR`+`SRV`+`TXT`+`A`/`AAAA`; browsable by iOS/Android/`dns-sd`; verified on-wire | Re-implements resolve/expiry; loses "blessed" status |
| General DNS-SD for arbitrary services | — | Still out of scope; ADR-016 §2 stands |

## Decisions

1. **Adopt `mdns-sd`; supersede ADR-016 decision 3.** `iroh-http-discovery`
   speaks standard DNS-SD so desktop nodes are browsable by iOS `NWBrowser`,
   Android `NsdManager`, and `dns-sd`. Option Y is no longer rejected — the
   `PTR` gap is a mission-level failure the official crate cannot fix.
2. **ADR-016 decisions 1, 2, 4 stand.** Discovery remains iroh-scoped (no
   arbitrary services), stays a separate optional crate, and is not baked into
   core. This ADR reverses only the "keep the official crate" mechanism, not
   the scope.
3. **Node ids on the wire are lowercase base32** (instance label + `pk` TXT),
   matching the rest of iroh-http and fitting the 63-byte DNS label limit.
4. **One `ServiceDaemon` per session, not process-wide.** `mdns-sd` does not
   deliver a daemon's own registrations to its own browsers; a session-scoped
   daemon ties lifetime to the session (drop → `shutdown()`) and lets an
   advertise and a browse in the same process discover each other.

## Consequences

- Dependency change: drop `iroh-mdns-address-lookup` (and its `swarm-discovery`
  chain); add `mdns-sd`, `n0-future`, `base32` behind the `mdns` feature.
- New `MdnsSdAddressLookup` gives desktop the same in-process auto-dial bridge
  the mobile adapters already have, closing ADR-016's mobile-parity gap from the
  desktop side.
- `#329`'s acceptance criterion (desktop visible to `dns-sd -B` / iOS
  `NWBrowser`) is met and verified on-wire; on-device cross-platform runs remain
  to be done with the user.
- The Option-Y spike referenced by ADR-016 is now the shipped approach, not a
  rejected experiment.

## Next steps

- [ ] On-device E2E: desktop advertise → iOS `NWBrowser` + Android `NsdManager`
      browse, discover, and dial by node id.
- [ ] Confirm no regression to mobile→desktop, desktop↔desktop, mobile↔mobile.
- [ ] Land the `mdns-sd` rewrite (lib.rs, address_lookup.rs, interop test,
      example) and update the discovery guide + specification wording.
