# DNS-SD consolidation behavior contract

Status: refactor gate for PR #367. This document records the observable
behavior that the DNS-SD consolidation must preserve. It is a compatibility
contract, not a description of the current internal class/module layout.

The architectural target is one generic Rust DNS-SD engine with thin platform
transports and one iroh-peer projection. Passing this contract means that
removing duplicated peer/native machinery has not changed the public API, wire
format, ownership rules, or platform support.

Normative background:

- [ADR-017](../adr/017-standard-dns-sd-discovery.md) selects standard DNS-SD
  via `mdns-sd` for desktop interoperability.
- [ADR-018](../adr/018-general-dns-sd-surface.md) defines generic DNS-SD as the
  primitive and iroh peer discovery as its specialization.
- [ADR-019](../adr/019-source-scoped-discovery-lifecycle.md) defines source,
  instance, owner, and terminal-state identity.
- The shipped TypeScript types and methods in
  [`discovery.ts`](../../packages/iroh-http-shared/src/discovery.ts) and
  [`IrohNode.ts`](../../packages/iroh-http-shared/src/IrohNode.ts) take
  precedence where an older ADR description has drifted from the implementation.

## Stable public surface

| Surface | Input | Output and lifetime | Must remain true |
| --- | --- | --- | --- |
| `node.advertise(options)` | Required `serviceName`, `instanceName`, `port`; optional arbitrary `txt`, `addrs`, `protocol`, `signal` | `Promise<void>` | Advertises an arbitrary DNS-SD service. `protocol` defaults to `udp`. With a signal, the promise remains pending until abort and awaits active cleanup. A pre-aborted signal does not start a native advertisement. Without a signal, it resolves after start and the registration remains owned by the adapter/context. |
| `node.browse(options)` | Required `serviceName`; optional `protocol`, `signal` | `AsyncIterable<ServiceRecord>` | One iterator owns one browse. It yields active/update and inactive/removal records without de-duplicating periodic announcements. Abort, `return()`, native close, or failure makes the iterator terminal. |
| `node.advertisePeer(options?)` | Optional `serviceName` and `signal` | `Promise<void>` | Specializes generic advertise with endpoint identity, endpoint addresses, relay data, and the endpoint's authoritative QUIC port. No signal means node/endpoint-owned lifetime. |
| `node.browsePeers(options?)` | Optional `serviceName` and `signal` | `AsyncIterable<DiscoveredPeer>` | Specializes generic browse, suppresses the local node, projects DNS-SD records into `{ nodeId, addrs, isActive }`, and feeds the endpoint lookup so `fetch(nodeId)` can resolve a LAN peer. |
| `asIrohPeer(record)` | A generic `ServiceRecord` | `DiscoveredPeer \| null` | Reads `pk`, falls back to `instanceName`, retains only dialable direct addresses and relay URLs, de-duplicates them, and returns inactive peers with the same identity semantics. |

The exact shipped shapes are defined in
[`packages/iroh-http-shared/src/discovery.ts`](../../packages/iroh-http-shared/src/discovery.ts):

- `ServiceConfig`: `serviceName`, `instanceName`, `port`, optional `txt`,
  `addrs`, and `protocol` (`udp | tcp`).
- `ServiceRecord`: `isActive`, `serviceType`, `instanceName`, optional `host`,
  `port`, `addrs`, and all `txt` properties.
- `DiscoveredPeer`: exactly `nodeId`, `addrs`, and `isActive`.

Opaque native/Rust handles, polling batches, owner generations, and source
leases are internal. The Tauri capability remains the single
`iroh-http:discovery` permission containing both generic and peer command
leaves; see
[`permissions/discovery.toml`](../../packages/iroh-http-tauri/permissions/discovery.toml).

## Wire and record compatibility

The consolidation must preserve these wire-level decisions:

1. A service name and protocol map to
   `_<serviceName>._udp.local.` or `_<serviceName>._tcp.local.`. Service names
   remain a non-empty DNS label containing only ASCII letters, digits, and
   `-`; the Rust validation is in
   [`dns_sd::service_type`](../../crates/iroh-http-discovery/src/dns_sd.rs).
2. Standard DNS-SD publication includes `PTR`, `SRV`, `TXT`, and `A`/`AAAA`.
   The `PTR` is mandatory for Bonjour, `NWBrowser`, and `NsdManager`
   enumeration; this is the reason for the `mdns-sd` desktop backend.
3. Iroh peer instance ids and `pk` values remain lowercase, unpadded RFC 4648
   base32 (52 characters), not iroh's 64-character hex display form.
4. Reserved peer TXT keys remain `pk`, `relay`, and `address`. Generic
   advertisements preserve caller TXT without interpreting it. Peer projection
   prefers `pk` and retains the instance-name fallback for compatibility.
5. `address` remains a comma-separated compatibility boundary for plural
   direct `ip:port` candidates. Entries are trimmed, independently validated,
   and de-duplicated. A malformed member cannot poison valid SRV/A, other TXT
   members, or relay fallback.
6. One TXT entry remains limited to 255 bytes, leaving 247 bytes for the value
   of `address=`. Fitting is stable and whole-member only: preserve input order,
   skip an item that does not fit, and still consider later shorter items.
7. Port `0` is never dialable. The Rust endpoint-lookup specialization also
   treats port `1` as a platform/test placeholder and does not feed it to iroh.
   The exported generic JS validator accepts literal IPv4 or bracketed IPv6
   with a port in `1..=65535`; this existing boundary difference must not be
   changed accidentally during consolidation. Relay URLs remain valid
   non-socket dialing input. An authoritative `address` TXT port suppresses a
   conflicting same-host SRV path, without suppressing an unrelated fallback.
8. IPv6 link-local addresses retain their interface scope. An unscoped
   link-local TXT literal cannot suppress a correctly scoped DNS-SD result;
   incidental scope on non-link-local addresses is discarded.
9. Active desktop/Android generic records preserve instance, service type,
   host, port, socket addresses, and every TXT property. Removal records carry
   the instance and service type with `isActive = false`, no host, port `0`,
   and empty addresses/TXT.
10. iOS generic browse remains the documented metadata-only exception:
    instance, service type, and TXT are available, while host is absent, port
    is `0`, and addresses are empty in the device acceptance check. Expanding
    iOS generic resolution is a separate capability change, not part of a
    structural consolidation.

The record and peer conversion tests live in
[`crates/iroh-http-discovery/src/dns_sd.rs`](../../crates/iroh-http-discovery/src/dns_sd.rs),
[`crates/iroh-http-discovery/src/lib.rs`](../../crates/iroh-http-discovery/src/lib.rs),
and
[`packages/iroh-http-shared/src/discovery.ts`](../../packages/iroh-http-shared/src/discovery.ts).

## Lifecycle traces

The new engine/session boundary must be able to replay every trace below
deterministically. A terminal state is sticky: subsequent polling/iteration
cannot restart a session or emit records from it.

| Trace | Required result | Existing evidence |
| --- | --- | --- |
| Two concurrent initial `next()` calls | Exactly one native browse start; polls are serialized. | `concurrent initial next calls share one native browse` in [`discovery-iterator.test.ts`](../../packages/iroh-http-shared/test/discovery-iterator.test.ts). |
| Iterator created but never consumed | No browse start and no abort listener. | `an iterator abandoned before next installs no abort listener` in the shared iterator tests. |
| Abort before advertisement start | Public promise settles and native start is never called. | `pre-aborted advertisement does not start natively`. |
| Abort/return while start is pending | Public operation becomes terminal; a late successful handle is closed exactly once and is never polled. A late rejection/cleanup failure is observed. | `return during start closes the late handle without polling`, `sticky next does not await detached late-start cleanup`, `abort settles a pending advertisement and closes its late handle once`, and `late cleanup failure is observed instead of becoming unhandled`. |
| Ready acknowledgement or start failure | Start succeeds only after the platform readiness callback; one-shot failure rejects rather than returning a live-looking handle. | Android `testBrowseReadinessAndTerminalConsumption` and `testAdvertisementAckUpdateAndRaces` in [`IrohHttpPluginContractHarness.kt`](../../packages/iroh-http-tauri/android-contract-tests/IrohHttpPluginContractHarness.kt). |
| Found, resolved, updated | Emit an active record for the exact instance; replacement of the same `(source, instance)` replaces only that contribution. | Rust source-scoped tests in [`source_scoped_lookup.rs`](../../crates/iroh-http-core/src/endpoint/source_scoped_lookup.rs) and peer browse-state tests in discovery `lib.rs`. |
| Lost before resolve / late resolve | Loss invalidates the instance generation; a late Android resolve callback cannot resurrect it. | Android `testPeerPresenceGenerationsAndPluralTxt`, `testGenericPresenceGeneration`, and `testPeerInstanceIdentityAndLateHandleCallbacks`. |
| One instance changes node identity | Recompute both identities atomically and publish the old-node transition before the new active union. Encoding-only hex/base32 changes do not create a second peer. | `valid_instance_identity_change_emits_old_transition_then_new_union` and `encoding_only_node_id_update_keeps_one_canonical_peer_key` in discovery `lib.rs`. |
| Two browse sources see the same peer | Each public browse emits only its own source's observations; the endpoint dialer sees the union. Closing one source retains the other's contribution. | `removing_one_source_keeps_the_other_source_in_the_global_lookup_only`, `active_events_do_not_leak_contributions_from_other_browse_sources`, and `retiring_one_browse_keeps_an_overlapping_browse_contribution`. |
| Empty/pathless live contribution | Presence may remain active with `addrs = []`, but the endpoint lookup does not resolve an undialable entry. | `removal_preserves_presence_when_the_remaining_instance_is_not_dialable`, `undialable_or_empty_updates_never_resolve`, and Tauri `remaining_instance_without_a_dialable_path_is_still_active`. |
| Native closed/failed poll | A missing handle is `closed`, not an empty active batch. A failed session reports its error once and terminates. Records in an authoritative terminal batch are not applied or emitted. | Android terminal-consumption harness; Tauri `terminal_batch_updates_are_not_applied_or_emitted`; `unexpected_pump_terminal_wakes_and_closes_the_session` in `dns_sd.rs`. |
| Explicit `return()`, abort, or terminal null | Close is invoked at most once, active async close is awaited, later `next()` is done, and close failure is surfaced once. A poll/start error remains primary if cleanup also fails. | `terminal null is sticky and closes exactly once`, `return waits for an active asynchronous native close`, `next after abort between records awaits asynchronous close`, `poll failure remains primary when cleanup also fails`, and `return reports a native close failure only once`. |
| Endpoint closes while peer browse is pending | Pending `next()` wakes, its source is retired, and future lookup results are empty. | `endpoint_close_wakes_peer_next_and_retires_its_source` in discovery `lib.rs`. |
| Endpoint address/relay changes during peer advertise | Re-register the same service identity/outer handle with the new mutable SRV port, address, and TXT snapshot. Stop the refresh worker after any started update finishes and before unregistering. | `changed_advertisement_rebuilds_mutable_srv_and_record_data`, `advertisement_session_deduplicates_its_initial_mutable_snapshot`, `refresh_cancellation_joins_after_an_in_flight_update`, Android update/race tests, and the interop `discovery-rebind-reemit` case. |

## Identity and ownership invariants

- A discovered contribution is keyed by `(browse source lease, DNS-SD instance
  name)`, never by node id alone.
- The endpoint dialer's effective node entry is the de-duplicated union of all
  live, dialable contributions. Peer presence and reachability are distinct.
- Dropping or explicitly retiring the final clone of a source lease removes all
  of that source's contributions synchronously and prevents resurrection.
- A service removal removes exactly that instance contribution. It does not
  erase a newer replacement or a contribution owned by another browse.
- Each JavaScript iterator owns one native browse. Each advertisement owns one
  registration and, for peer advertisements, one endpoint-watch refresh task.
- An advertisement's service name, protocol, and instance label are its stable
  DNS-SD identity. Its SRV port, addresses, and TXT properties are the mutable
  snapshot; peer refresh may change the chosen SRV port when the endpoint's
  eligible listener/interface family changes.
- Peer sessions are owned by endpoint plus invoking WebView in Tauri. Generic
  sessions are owned by the invoking WebView. Endpoint replacement/close
  retires only that endpoint's peer sessions; WebView advance/destruction
  retires that context's peer and generic sessions.
- Starts capture an owner generation before awaiting native readiness. A late
  completion after retirement is rejected and immediately cleaned up.
- Public discovery handles are monotonic/generational and are never reused.
  Close is idempotent, so a delayed close cannot delete a newer session.
- Node and Deno peer resources are endpoint-owned and retire on endpoint close.

The deterministic ownership guards are in
[`source_scoped_lookup.rs`](../../crates/iroh-http-core/src/endpoint/source_scoped_lookup.rs),
the canonical peer projection in
[`iroh-http-discovery`](../../crates/iroh-http-discovery/src/lib.rs),
[`discovery_ownership.rs`](../../packages/iroh-http-tauri/src/discovery_ownership.rs),
and
[`discovery_handles.rs`](../../packages/iroh-http-tauri/src/discovery_handles.rs).
Their current location is not normative: the consolidation may move them into
the discovery crate, but it may not weaken their behavior.

## Platform contract and floors

| Platform | Transport adapter | Generic browse result | Minimum/support constraint |
| --- | --- | --- | --- |
| Node, Deno, Tauri desktop | Rust `mdns-sd` | Fully resolved | Discovery remains optional behind the existing `mdns`/`discovery` feature gates. No discovery policy moves into always-on `iroh-http-core`. |
| Android | `NsdManager` native adapter | Fully resolved after `resolveService` | `minSdk = 21` in [`android/build.gradle.kts`](../../packages/iroh-http-tauri/android/build.gradle.kts). The serialized legacy resolve queue and both API-21/current-AOSP listener orderings remain supported. Pre-T-extension-7 sessions share a plugin-owned multicast lock; newer foreground sessions use system-managed multicast. |
| iOS | `NWBrowser` plus native registration adapter | Metadata-only, as described above | iOS 14 in [`ios/Package.swift`](../../packages/iroh-http-tauri/ios/Package.swift). Static `NSBonjourServices` declarations and Local Network permission remain required. |

Mobile setup and permissions remain as documented in
[`mobile-mdns-setup.md`](../guidelines/mobile-mdns-setup.md). The public Tauri
permission does not split into separate peer and generic grants.

## Required test gates

All existing harnesses and scenarios are retained even if their implementation
target changes from a peer-specific native engine to the generic adapter plus
Rust peer projection.

| Gate | Command/location | What it proves |
| --- | --- | --- |
| Rust unit/integration | `cargo test --workspace`; specifically `cargo test -p iroh-http-core` and `cargo test -p iroh-http-discovery` | DNS-SD parsing/policy, source-scoped union and retirement, peer projection, desktop session termination, address selection and refresh. |
| Tauri Rust | `cargo test --manifest-path packages/iroh-http-tauri/Cargo.toml` | Owner generations, endpoint isolation, handle non-reuse, terminal batches, mobile projection/retirement, and Rust/native command-name parity (`tests::ffi_contract`). |
| Shared JS lifecycle | `npm test --workspace=packages/iroh-http-shared` | One-start iterator behavior; abort, return, failure, cleanup, and peer/generic sharing. |
| Cross-runtime public shape | [`tests/suites/discovery.mjs`](../../tests/suites/discovery.mjs) through Node, Deno, and Tauri runners | Public methods return the expected promise/iterable shapes, abort settles, and `asIrohPeer` compatibility remains. This suite deliberately does not prove multicast behavior. |
| Structured interop | [`tests/interop/suite.mjs`](../../tests/interop/suite.mjs) | Real advertise/browse self round-trip, dialable address TXT, and rebind re-emission where mDNS is available. |
| Android native contract | `bash packages/iroh-http-tauri/android-contract-tests/run.sh` | Deterministic generic `NsdManager` callbacks: readiness/failure, found/lost/late-resolve, resolve-queue retirement, peer-shaped TXT fidelity, registration update/stop races, unregister retry, and API-era listener ordering. Peer parsing and TXT fitting are Rust policy tests. |
| Android compile | `bash packages/iroh-http-tauri/scripts/ci-android-gradle-build.sh` | Kotlin/Tauri API compatibility at compile SDK 34 (plus the CI-resolved Tauri Android SDK). |
| iOS native contract | `bash packages/iroh-http-tauri/ios/ContractTests/run.sh` | Deterministic production generic-lifecycle coverage for readiness/failure, peer-shaped TXT fidelity, found/change/lost, stale generations, stop acknowledgement, and callback races. Peer parsing and TXT fitting are Rust policy tests. |
| iOS compile | `bash packages/iroh-http-tauri/scripts/ci-ios-swift-build.sh` | Full Swift plugin compiles against the iOS 14 simulator/Tauri API. |
| Physical device matrix | [`dns-sd-device-verification.md`](dns-sd-device-verification.md) | Actual multicast, permissions, desktop↔mobile discovery, direct dial, rebind, and the iOS metadata-only/Android fully-resolved distinction. |

CI wiring for these gates is in
[`ci.yml`](../../.github/workflows/ci.yml) and
[`extended-tests.yml`](../../.github/workflows/extended-tests.yml).

## Structural acceptance criteria

The refactor is complete only when all behavior above is green and the code has:

1. one semantic browse/advertise lifecycle state machine;
2. one canonical service config, record, raw event, and terminal model in Rust;
3. one peer projection and one TXT/address policy;
4. no peer-specific native Android or iOS discovery engine;
5. native adapters limited to platform object lifetime, callback buffering,
   readiness/terminal reporting, and required OS-version compatibility;
6. one owner-aware session/handle registry model rather than parallel peer and
   generic registries;
7. no DNS-SD policy in `iroh-http-core` beyond the narrow address-lookup seam
   required by iroh;
8. unchanged public TypeScript API, Tauri capability, wire convention, feature
   gates, and platform floors; and
9. no live dual-running old/new network engines during migration.

## Explicit non-goals

- Do not remove generic DNS-SD or reduce it to iroh-only discovery.
- Do not replace DNS-SD with iroh's built-in mDNS format. Plain-iroh's backend
  omits the browseable `PTR` record and is intentionally not wire-interoperable.
- Do not turn this package into a node-free, standalone Bonjour library; the
  generic surface remains loaded through an iroh node adapter.
- Do not change the public method names, shapes, event ordering, Tauri
  permission, wire keys, protocol defaults, or platform minimum versions.
- Do not make iOS generic browse fully resolve hosts/ports as part of the
  structural refactor.
- Do not silently drop the instance-name fallback for peer identity or change
  TXT/address fitting and validation policy.
- Do not claim synchronous invalidation of addresses that iroh has already
  copied into its private remote-path cache. Source retirement guarantees
  future provider results only; upstream iroh 1.0.2 exposes no purge API.
- Do not delete or weaken test scenarios merely because the implementation
  moves. Native harnesses may be retargeted to the generic adapter, while peer
  interpretation cases move to Rust projection tests.

## Remaining verification constraints

These are not permission to weaken the contract; they are explicit boundaries
for review and release:

1. **Post-ready advertisement failure has no status channel.** ADR-019 records
   this explicitly. Start readiness is covered, but a later native
   advertisement failure cannot yet be delivered uniformly.
2. **Physical-device verification remains outstanding.** Compile and host
   callback tests do not prove multicast visibility, permissions, interface
   changes, or OEM behavior. Complete the matrix in
   `dns-sd-device-verification.md` before treating the mobile work as
   device-verified.
3. **“Lossless” needs the iOS qualifier everywhere.** Generic Rust/desktop and
   Android records are lossless; iOS generic browse is deliberately
   metadata-only. Tests and docs must keep that distinction explicit.
4. **Port `1` policy differs at two public/internal boundaries.** Rust peer
   lookup treats `:1` as a placeholder, while the exported
   `isDialableSocketAddr()` accepts it as a valid socket port. Consolidation
   must preserve this until a separately reviewed compatibility decision makes
   the two policies uniform.
