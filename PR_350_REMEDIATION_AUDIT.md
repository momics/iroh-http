# PR #350 remediation audit

Audit date: 2026-07-14\
Repository: [momics/iroh-http](https://github.com/momics/iroh-http)\
Pull request: [#350](https://github.com/momics/iroh-http/pull/350)\
Reviewed/remediated head: `a77e9c9ebcf293fa5386b857f78a9ff3aa670a63`\
PR base: `main` at `4d7d0241fe0d68d3b68e9be554ff89e344ac264a`\
Local remediation branch: `codex/pr350-remediation`\
Local worktree: `/private/tmp/iroh-http-pr350-remediation` Remote PR state at
audit time: **open, draft**, head unchanged at `a77e9c9`

## Outcome

All **17 external re-review findings are present on GitHub exactly once** and
are grouped under issues #362–#366 in the repair order proposed by the review.
The implementation in this branch covers every reported code defect and adds
regressions at the affected ownership boundary.

The host, cross-compile, Android compile/lint, and iOS compile gates are green.
The work should **not yet be described as fully device-accepted**: #365 and #366
intentionally remain open for the Android/iOS network and callback matrix, the
real iOS already-bound-port integration, and the cross-adapter dial test.

No GitHub issue, PR comment, branch, or label was changed while performing this
remediation. The remote PR remains at the reviewed head; all implementation is
local until the owner chooses how to land it.

## GitHub reconciliation

The reporting chain is complete and internally consistent:

- PR summary comment:
  [external re-review and workstream table](https://github.com/momics/iroh-http/pull/350#issuecomment-4966393821)
- Epic comment:
  [17 findings grouped under #362–#366](https://github.com/momics/iroh-http/issues/361#issuecomment-4966391443)
- [#362 — core transport](https://github.com/momics/iroh-http/issues/362):
  findings 3, 2, 13
- [#363 — lifecycle races](https://github.com/momics/iroh-http/issues/363):
  findings 1, 4
- [#364 — direct-address selection](https://github.com/momics/iroh-http/issues/364):
  findings 5, 6, 7, 16
- [#365 — source-scoped discovery](https://github.com/momics/iroh-http/issues/365):
  findings 11, 9, 10, 8, 12, 14, 15
- [#366 — iOS generic advertisement](https://github.com/momics/iroh-http/issues/366):
  finding 17

The issue priorities also match the review: #362–#365 are P1 bugs; #366 is P2.
Connectivity and relevant component labels (`rust`, `javascript`, or
`architecture`) are present.

## Method

The remediation was deliberately staged rather than applied as one large patch:

1. Freeze the exact reviewed PR head and map every finding to one GitHub
   workstream.
2. Reproduce each defect with a focused regression or deterministic state
   harness before changing behavior.
3. Land the low-coupling transport, lifecycle, and address-policy slices first.
4. Record the discovery redesign in
   [ADR-019](docs/adr/019-source-scoped-discovery-lifecycle.md).
5. Implement the shared source/instance/owner state model across core, desktop,
   Node, Deno, Tauri, Android, and iOS.
6. Run independent challenge reviews of core lookup semantics, desktop SRV and
   IPv6 scope policy, adapter ownership, Tauri reload cleanup, Android callback
   generations, iOS registration behavior, and CI coverage.
7. Fix the interactions found by those reviews, rerun focused tests, then run
   the full host and cross-target matrix.
8. Preserve hardware-only assertions as explicit release gates instead of
   treating compilation or mocks as device proof.

## Finding-by-finding disposition

|  # | Priority | Resolution and regression evidence                                                                                                                                                                                                                                                                         | Workstream |
| -: | :------: | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- |
|  1 |    P1    | Lifecycle requests now install cancellation ownership synchronously. Same-tick `start → stop/restart/close` tests terminate without leaving `starting`.                                                                                                                                                    | #363       |
|  2 |    P1    | The common public Rust serve path allocates the serve-cycle token used by stop/handoff. `pure_rust_serve_is_stoppable_in_one_call` covers the original race.                                                                                                                                               | #362       |
|  3 |    P1    | Replayability requires a truly ended body; caller/body errors are distinct from transport failure. `zero_data_body_error_surfaces_and_is_not_replayed` asserts one server call and the original error.                                                                                                     | #362       |
|  4 |    P1    | Foreground health sweeps are disposable after every await, unsubscribe before unhealthy handoff, and observe async recovery failures. Pending-probe/backoff removal and one-shot handoff are tested.                                                                                                       | #363       |
|  5 |    P1    | Real QAD/reflexive ports remain authoritative; only `:0`/`:1` placeholders borrow a compatible listener port. Core, desktop, and Tauri regressions cover this.                                                                                                                                             | #364       |
|  6 |    P1    | Cross-family port borrowing is removed and unrepairable placeholders are dropped. Asymmetric IPv4/IPv6 listener tests cover each selector.                                                                                                                                                                 | #364       |
|  7 |    P1    | Numeric IPv6 DNS scope is preserved through a bounded local resolver proxy; named or unscoped link-local resolvers are rejected. Tests perform an actual DNS query and exercise proxy shutdown/load shedding.                                                                                              | #364       |
|  8 |    P1    | Plural peer addresses flow through desktop, Kotlin, Swift, and Tauri with one stable complete-value TXT fitting policy. Browsers split, validate, deduplicate, and retain valid SRV/relay fallbacks.                                                                                                       | #365       |
|  9 |    P1    | Android marks instance presence before resolution and validates a per-instance generation in late callbacks. Peer and generic found/lost/late-resolve sequences are in the 10-group Kotlin harness.                                                                                                        | #365       |
| 10 |    P1    | Native starts wait for readiness; browse polls distinguish `active`, `closed`, and `failed`; terminal batches are authoritative. JS discovery start/poll/abort/close is linearized and cleanup is awaited.                                                                                                 | #365       |
| 11 |    P1    | Core now keys contributions by `(browse source, service instance)`. The endpoint dialer sees the global union, while each public browse emits only its source-local union. Stable replacement, malformed replacement, A→B identity changes, overlap, canonical node IDs, and pathless presence are tested. | #365       |
| 12 |    P1    | Peer advertisements watch endpoint address/relay changes and refresh the same registration identity. Desktop and native update/cancellation paths are covered by component and callback tests.                                                                                                             | #365       |
| 13 |    P2    | Stale-hit and immediately-closed pool cleanup use `dial_seq` compare-and-remove and consume a concurrent replacement instead of redialing. Deterministic concurrency tests cover both branches.                                                                                                            | #362       |
| 14 |    P2    | Discovery handles are monotonic and sessions have endpoint/WebView generation owners. Endpoint close/replacement and page reload retire only matching resources; stale starts are rejected before and after native registration. Node and Deno peer sessions are endpoint-owned too.                       | #365       |
| 15 |    P2    | Desktop/mobile browse sessions own source leases and synchronously retire their lookup contributions on close or terminal state. Global lookup state remains live only for other sources.                                                                                                                  | #365       |
| 16 |    P2    | IPv4-only route probing was replaced with operational dual-family interface inventory. Desktop selection mirrors the mDNS daemon's oper-up, non-P2P, non-AWDL/LLW policy and pairs addresses only with same-family listeners.                                                                              | #364       |
| 17 |    P2    | iOS generic advertisement now uses `NetService` to publish the caller-owned port without binding it or accepting/cancelling client connections. Swift builds against the Tauri API.                                                                                                                        | #366       |

## Additional defects caught during remediation

Independent review of the fixes found several interactions not stated in the
original 17 findings. They were handled as part of this branch:

- A valid or malformed service-instance identity replacement could leave a ghost
  peer in JavaScript even after the core lookup changed. Old-node transitions
  are now emitted before the new-node snapshot.
- Publishing the dialer's global address union on only the session that caused a
  transition made other browses permanently stale. Public events are now
  source-local; only the internal endpoint lookup is global.
- Concurrent first `next()` calls could start two native browses and leak one;
  terminal/return/abort was not sticky, and per-record abort listeners leaked.
  Peer and generic iterators now share one linearized lifecycle helper.
- Aborting while an advertisement start was awaiting native readiness could hang
  indefinitely or leak a late handle. Advertisement start/abort cleanup is now
  linearized as well.
- Desktop SRV selection could choose a reflexive TXT-only port, a listener
  family with no usable interface, an operationally-down interface, or an
  interface filtered out by `mdns-sd`. Peer advertisements now use the same
  usable-interface predicate as the daemon and disable unsafe auto expansion.
- IPv6 scope comparison treated incidental scope on ULA/global addresses like
  link-local identity. Scope is now significant only where routing requires it;
  unscoped link-local TXT cannot suppress a scoped SRV path.
- A full mDNS daemon command queue could lose registration state during
  unregister. Unregister/stop/shutdown now retry without forgetting ownership.
- Cancelling a Tauri refresh task could drop an in-flight plugin response
  receiver and panic the later callback. Cancellation now preserves and joins
  the native call before unregister.
- WebView reload could start a same-label session before the previous native
  unregister completed, or proceed with a stale generation after waiting. A
  per-WebView cleanup barrier and pre-native owner revalidation cover both
  races.
- Android API-21 failure cleanup and registration-listener retirement differed
  from actual `NsdManager` callback rules; the fake oracle and implementation
  now agree.
- iOS generic/peer registries could collide on a wrong-kind stop, and native TXT
  fitting could split an address. Registries are kind-safe and TXT candidates
  are included only whole.

## Validation evidence

### Rust and host networking

- `cargo test --workspace`: **324 passed, 0 failed, 4 ignored**.
- All three ignored discovery socket/multicast tests were then run explicitly:
  **3 passed, 0 failed**. Only the unrelated diagnostic timeout test remains
  intentionally ignored.
- Source-scoped core focus: **9 passed**.
- Discovery library: **55 passed**, plus the two explicit socket tests.
- Tauri Rust: **71 library + 4 permissions + 1 doctest passed**.
- Strict workspace production clippy passed with `-D warnings`, `unwrap_used`,
  `panic`, and `arithmetic_side_effects` denied.
- Tauri strict production clippy and warning-clean all-target clippy passed.
- Tauri `--no-default-features --all-targets` clippy passed.

### TypeScript and JavaScript

- Root Deno formatting check passed.
- Shared/Node/Tauri TypeScript checks passed.
- Tauri Vitest: **85 passed**.
- Shared discovery lifecycle tests: **16 passed**.
- Node native runtime: **10/10 adapter tests** and **94 compliance checks
  passed, 0 failed**.
- Deno native runtime: **12/12 adapter tests** and **94 compliance checks
  passed, 0 failed**.
- Deno adapter/module checks passed.

### Android

- Android native discovery contract: **10/10 groups passed**.
- Rust `aarch64-linux-android` clippy passed against the latest source model.
- Gradle `:tauri-plugin-iroh-http:assembleDebug` succeeded with Java 21 and
  minSdk 21.
- Gradle `:tauri-plugin-iroh-http:lintDebug` succeeded; no plugin API-level or
  lifecycle lint failure was reported.
- Rust/Kotlin/Swift command parity tests: **3/3 passed**.

### iOS

- Rust `aarch64-apple-ios` clippy passed against the latest source model.
- The repository iOS Swift FFI build succeeded for the arm64 iOS simulator.
- The peer TXT boundary contract passed.
- The only Swift warning printed by the build is in the vendored Tauri
  `JsonValue.swift`, not this plugin.

### Repository hygiene

- Rust formatting and Deno formatting are clean.
- `git diff --check` is clean.
- CI workflow YAML parses and both new shell harnesses pass `bash -n`.
- Lockfile consistency and final adapter/runtime checks passed after the diff
  was frozen.

## Remaining acceptance and release gates

These are explicit gates, not silently waived criteria:

1. **Physical Android/iOS matrix:** multicast visibility, Local Network
   permission behavior, API-21/OEM callback ordering, late/never-returning
   resolve callbacks, rapid WebView reload/destruction, label reuse, suspension,
   and interface migration.
2. **Cross-adapter plural dial:** observe IPv4 + IPv6 and VPN + LAN candidates
   crossing a real desktop/mobile adapter boundary and being selected by an
   actual dial, not only by component tests.
3. **Live advertisement refresh:** change a running endpoint's selected address
   and observe the replacement record from another physical browser.
4. **iOS already-bound-port integration (#366):** run a real service on the
   advertised port, publish it with `NetService`, browse it from a peer, and
   connect to the original service.
5. **Post-ready advertisement failure:** readiness acknowledgement fixes false
   start success, but generic advertisement still has no native status channel
   for a failure that occurs after readiness. Peer advertisement may notice it
   only on a later refresh.
6. **Upstream iroh path cache:** source retirement removes future lookup
   results, but iroh 1.0.2 exposes no API to purge an address already copied
   into private `RemotePathState`. Track upstream removal support or verify
   endpoint rebuild behavior on devices.
7. **Process-shutdown hook:** Tauri's synchronous last-window callback can
   retire Rust ownership immediately but cannot await native unregister
   completion; normal explicit close/replacement and reload paths do await
   cleanup.

## Commit ledger

- `4b652e9` — `fix(tauri): close lifecycle ownership races (#363)`
- `c579084` — `fix(core): preserve transport request ownership (#362)`
- `f16b449` — `fix(discovery): preserve scoped address policy (#364)`
- `3120100` — `fix(discovery): own native session state (#365, #366)`
- This audit report is committed separately after the implementation so its
  evidence and commit ledger stay reviewable on their own.

## Recommendation

The host-verifiable remediation is suitable for review as a coherent follow-up
series. Keep #365 and #366 open, leave PR #350 in draft, and run the device
gates above before declaring the work merge-ready or closing the epic
workstreams.
