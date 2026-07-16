---
id: "019"
title: "Source-scoped discovery state and owned native sessions"
status: accepted
date: 2026-07-14
area: transport
tags: [discovery, dns-sd, mdns, mobile, lifecycle, address-lookup]
---

# [019] Source-scoped discovery state and owned native sessions

> Extends [ADR-017](017-standard-dns-sd-discovery.md) and
> [ADR-018](018-general-dns-sd-surface.md). Those records define one generic
> DNS-SD engine with a thin iroh-peer specialization. This record defines the
> lifetime and identity model shared by that engine across desktop, Android,
> and iOS.

## Context

PR #350 made peer and generic DNS-SD available on all supported adapters, but
the first implementation reduces peer state to `node id -> addresses` and
tracks native sessions in a process-global `(kind, handle)` list. That loses
two identities which DNS-SD and the application lifecycle require:

- a DNS-SD **service instance** can disappear after its replacement, carrying
  the same node id, has already appeared; and
- a **browse session** can close while another session still contributes a
  record for the same node.

Removing by node id therefore erases live replacement data. Conversely,
closing a browse does not reliably retract its addresses from iroh's dialer.
The global Tauri session list has the same ownership problem at a larger scale:
replacing one endpoint retires unrelated sessions, while normally closing an
endpoint retires none.

Native callbacks add a second state machine. Android resolution can complete
after `onServiceLost`; both mobile platforms currently turn a failed native
session into an endless empty poll or report advertisement success before the
native registration is ready. Finally, peer advertisements snapshot the
endpoint address once even though direct addresses and relay selection change
over the lifetime of an endpoint.

These are not independent callback bugs. They are symptoms of missing source,
instance, owner, and terminal-state identities at the adapter seams.

## Questions

1. What is the authoritative identity of a discovered address contribution?
2. When may a node disappear from iroh's `AddressLookup`?
3. Which object owns and retires a browse or advertisement session?
4. How do native start, poll, close, and failure transitions reach Rust and
   JavaScript without being mistaken for an empty active session?
5. How are changing endpoint addresses reflected without changing the public
   advertisement handle?

## What we know

- DNS-SD removal events identify a service instance, not an iroh node id.
- More than one instance or browse can legitimately contribute addresses for
  the same node during restart, interface migration, or overlapping callers.
- Iroh's address-lookup registry supports adding a provider but not removing
  one. A retired provider must therefore become empty and harmless.
- Iroh 1.0.2 copies lookup results into a private `RemotePathState`. Its public
  API has no way to invalidate addresses that were already consumed by an
  earlier dial; provider retirement can prevent future resolution but cannot
  synchronously purge that internal path cache.
- `Endpoint::watch_addr()` exposes direct-address and relay changes.
- `mdns-sd` re-announces an updated `ServiceInfo` when `register` is called
  again; unregistering first is neither required nor desirable.
- Tauri commands can receive the invoking WebView, so context ownership does
  not need to be inferred from a process-global singleton.
- Android `NsdManager` and Apple's discovery APIs acknowledge start and report
  terminal failure asynchronously.

## Options considered

| Option | Upside | Downside |
| ------ | ------ | -------- |
| Keep node-only maps and add callback guards | small diff | cannot represent overlapping instances or browse sessions; cleanup remains incorrect by construction |
| One lookup provider per browse | browse drop can empty its own provider | overlapping results are left for iroh to merge; mobile already uses one shared provider; registration cannot be undone |
| **One source-scoped lookup with browse leases** | one tested state model for desktop and mobile; exact retirement; effective union is deterministic | adds a small public core seam and source/instance metadata to adapters |
| Periodically rebuild discovery from snapshots | eventual convergence | races remain visible between sweeps; terminal errors and ownership are still lost |

## Decisions

1. **A contribution is keyed by `(browse source, service instance)`.** A source
   is a unique lease created for one peer-browse session. The instance is the
   DNS-SD instance name exactly as reported by the platform. A contribution
   stores its parsed endpoint id and validated transport addresses. Updating
   the same key replaces only that contribution.

2. **The effective node entry is the union of all live contributions.** An
   instance expiry removes exactly one contribution. Its dial entry disappears
   when no live contribution has a dialable address, while peer presence ends
   only with the final contribution. Duplicate addresses are collapsed;
   malformed direct addresses and port zero are never made dialable. If an
   instance changes its advertised node id, both the old
   and new node entries are recomputed atomically, and the adapter publishes
   the old node's transition before the new active union. Event node ids are
   canonical lowercase base32, so an encoding-only hex/base32 change cannot
   create a second peer key. Peer presence remains distinct from reachability:
   another live pathless contribution is active with an empty address union.
   The global union belongs only to the endpoint dialer. Each public browse
   emits the union of instances observed by its own source; otherwise a second
   browse would inherit records it never observed and could not be notified
   when the first browse closes.

3. **The source-scoped lookup is a shared core module.** Desktop discovery and
   Tauri mobile use the same implementation rather than maintaining parallel
   maps and parsers. Creating a source returns a lease. Dropping or explicitly
   retiring the lease synchronously removes all of that source's contributions,
   which keeps an unremovable iroh lookup provider empty after browse close.
   This guarantee applies to future lookup results; it does not claim to remove
   paths already copied into iroh's private remote-path cache.

4. **Native peer events carry the instance identity.** `discovered` and
   `expired` events contain `instanceName`, `nodeId`, and the structured address
   list. Android marks an instance present before starting resolution and tags
   each resolve with a generation; loss invalidates that generation, so a late
   callback cannot resurrect a ghost. Swift compares current instance names,
   not node ids, when producing expiry.

5. **Native sessions expose explicit terminal state.** Poll responses contain
   `active`, `closed`, or `failed` plus records and an optional error. A missing
   native handle is `closed`, never an empty active poll. Start resolves only
   after the platform acknowledges browse/advertise readiness and rejects on a
   start failure. Later browse failure is retained until Rust observes it,
   after which the Rust iterator terminates with the error. A terminal poll
   batch is authoritative and its records are not emitted after their source
   has ceased to own them. A post-ready advertisement failure still needs a
   native status callback or poll channel; readiness acknowledgement alone
   cannot report that later transition.

6. **Every Tauri session has one declared owner.** Peer sessions are owned by
   the endpoint handle and invoking WebView. Generic sessions are owned by the
   invoking WebView. Endpoint close/replacement retires only that endpoint's
   peer sessions; WebView destruction retires all sessions created by that
   context. Starts capture owner generations before awaiting native readiness,
   and conditional registration rejects late completion after teardown. Public
   discovery handles are monotonic and never reused, so a delayed close cannot
   delete a newer session. Node and Deno likewise associate peer resources with
   their endpoint and retire them synchronously on endpoint close.

7. **Peer direct addresses remain plural and structured inside the system.**
   The advertiser accepts all selected direct `SocketAddr` values. The TXT
   boundary encodes them as a comma-separated value for compatibility; every
   specialized browser splits, trims, validates, and deduplicates that value
   before updating the lookup. An invalid TXT member does not suppress valid
   SRV/A or relay fallbacks. Because one DNS-SD TXT entry is limited to 255
   bytes, every platform uses the same stable fitting-subset rule: preserve
   input order, include only complete candidates while `address=<value>` fits,
   skip a non-fitting candidate, and never truncate an address.

   DNS-SD still has only one SRV port. Desktop publishes A/AAAA records only
   for direct candidates that really use that chosen port; different-family
   and reflexive ports remain TXT-only. Peer advertisements disable automatic
   host-address expansion and instead select from the same operational,
   non-P2P interface inventory that the mDNS daemon can announce, paired only
   with a same-family listener using the chosen SRV port. A browser suppresses
   a conflicting same-IP SRV path when TXT supplies the authoritative port,
   and preserves an IPv6 record's interface scope.

8. **Advertisement state follows the endpoint.** A peer advertisement watches
   `Endpoint::watch_addr()` and re-registers the same DNS-SD service identity
   when its direct addresses or relay URLs change. The update task belongs to
   the advertisement session and stops before the registration is removed.
   Native adapters provide the equivalent update operation while preserving
   the outer advertisement handle. Mobile cancellation preserves the response
   receiver for an in-flight Tauri native invoke, joins the refresh worker after
   that callback completes, and only then stops the native registration.

9. **Each JavaScript discovery iterator owns exactly one native browse.** The
   initial start is linearized across concurrent `next()` calls. Terminal
   state, explicit `return()`, and abort are sticky; each closes the native
   browse at most once, including when start resolves after cancellation. One
   abort listener is installed lazily per iterator and removed at termination.
   Active close hooks are awaitable lifecycle barriers in Tauri and Deno;
   failures do not mask a primary start/poll error, and detached cleanup of a
   late handle is always observed. Peer and generic advertisement use the same
   abort-aware start rule: cancellation settles even while readiness is pending
   and retires a handle that arrives later.

## Consequences

- `iroh-http-core` gains a small, reusable source-scoped `AddressLookup` module
  with deterministic unit tests for replacement, overlap, retirement, and
  malformed addresses.
- Desktop `BrowseSession` and the Tauri mobile browse registry hold source
  leases; their teardown becomes correct even though iroh cannot unregister an
  address-lookup provider.
- Peer event payloads and native poll responses change below the stable public
  TypeScript surface. Generic `ServiceRecord` remains lossless.
- The Tauri session registry becomes owner-indexed rather than process-global.
- Native callback state is slightly larger, but false start success, endless
  empty polling, and found/lost/late-resolve ghosts become unrepresentable.
- Peer advertisements incur a small background watcher task and may reannounce
  when network state changes.
- Source retirement is exact inside the provider, but iroh 1.0.2 may retain a
  path it consumed before retirement. Full cache invalidation requires an
  upstream iroh removal API (or endpoint rebuild); the device matrix must
  therefore verify that closed-browse addresses are not selected on subsequent
  real dials and track any remaining upstream behavior explicitly.

## Next steps

- [x] Implement the core lookup and replacement/retirement tests.
- [x] Migrate desktop and mobile peer browse to source leases.
- [x] Add instance identity and terminal poll state end to end.
- [x] Add Android resolve-generation and Swift instance-expiry regressions.
- [x] Scope the Tauri registry by endpoint and WebView owner.
- [x] Thread plural addresses through every peer adapter.
- [x] Refresh desktop and native peer advertisements on endpoint changes.
- [x] Linearize JavaScript discovery start, poll, abort, and close ownership.
- [x] Run host CI, native contract harnesses, and Android/iOS compile gates.
- [ ] Add a native status channel for post-ready advertisement failure.
- [ ] Run the Android/iOS callback, reload, and network-change device pass.
