# On-device DNS-SD verification runbook (#334)

Standard DNS-SD discovery ([ADR-017](../adr/017-standard-dns-sd-discovery.md),
#330) shipped mobile-native Tauri plugin code that **no CI job compiles or
runs** — there is no iOS or Android runner. This runbook drives the real
hardware pass from the example app's **Test** tab **Suite runner**, which
executes the structured interop suite
([`tests/interop/suite.mjs`](../../tests/interop/suite.mjs)) against a
discovered peer and reports grouped pass/fail/skip results plus a greppable log
line per case.

The suite folds the DNS-SD acceptance criteria into named groups:

| Group | Covers | #334 criterion |
|-------|--------|----------------|
| `discovery` | self advertises a dialable address; advertise→browse round-trip; re-advertise re-emits (rebind, W2) | 1 (discover), 2 (isActive/rebind) |
| `direct-dial` | fetch reaches the peer over a direct `ip:port` (asserts `transport === "direct"`) | 1 (auto-dial) |
| `relay-fallback` | fetch succeeds with only a relay available (asserts `transport === "relay"`) | — |
| `http-compliance` | the `cases.json` corpus over the dialed peer | — |
| `serve-stop` | serve → reachable → stop → refused | — |

> **Honest assertions.** Cases that cannot *measure* their precondition (no
> mDNS on a headless runtime, transport unknown because `peerStats()` is
> unavailable) report **skip**, not pass. A green run therefore means the
> behaviour was observed, not merely "did not error". On mobile the app passes
> `mdnsCapable: true`, so a discovery case that fails to observe the peer is a
> real **fail** (e.g. a missing `NSBonjourServices` plist entry), never a silent
> skip.

The standalone diagnostic cards from earlier drafts (isActive-watch, Android
resolve-queue burst, iOS TXT-mutate) have been **removed** — their behaviour is
now exercised by the `discovery` group's rebind case and by normal suite
traffic. The one remaining manual DNS-SD check is the **Generic DNS-SD**
advertise/browse on the **Discovery** tab (criterion 5, iOS metadata-only).

---

## 0. Setup

**Hardware:** one desktop (macOS/Linux/Windows), one iPhone/iPad, one Android
phone/tablet — all on the **same LAN / Wi-Fi** with client isolation OFF.

**Permissions:** confirm the iOS `Info.plist`
(`NSLocalNetworkUsageDescription` + `NSBonjourServices`, including the
`_iroh-http-test._udp` testing-mode service) and Android `AndroidManifest.xml`
entries per [Mobile mDNS / DNS-SD setup](../guidelines/mobile-mdns-setup.md). On
first launch iOS shows a local-network permission prompt — **Allow** it.

**Build & install the example app** (`examples/tauri`) on each device:

```sh
# desktop
cd examples/tauri && npm run tauri dev
# iOS device (Xcode signing required)
npm run tauri ios dev
# Android device (USB debugging on)
npm run tauri android dev
```

**Attach to logs.** The suite emits one greppable line per case
(`IROH_INTEROP_CASE`) and the app prints the full JSON report under the
`[iroh-http-interop]` tag. The generic-browse check still uses the
`IROH_DNSSD_CHECK` prefix.

```sh
# Android — native + webview console both reach logcat
adb logcat | grep --line-buffered -E 'IROH_INTEROP_CASE|iroh-http-interop|IROH_DNSSD_CHECK'
# iOS — device console (Console.app, or:)
xcrun devicectl device console --device <UDID> | grep -E 'IROH_INTEROP_CASE|iroh-http-interop|IROH_DNSSD_CHECK'
# desktop — lines print to the terminal running `tauri dev`
```

**Case-line grammar:**
`IROH_INTEROP_CASE id=<case> group=<group> outcome=<pass|fail|skip> latencyMs=<n> [transport=<direct|relay>]`
— single line, space-separated, greppable.

---

## Criterion 1 — advertise / discover + auto-dial (iOS↔desktop, Android↔desktop)

Run once for the iOS↔desktop pair, once for Android↔desktop.

**Steps**

1. On both devices in the pair, open **Test** and toggle **Enable testing
   mode**. Each device advertises the `_iroh-http-test._udp` service with a real
   `address` TXT and browses for the other.
2. Within a few seconds the peer appears in the **Suite runner** peer picker
   with the correct `platform`.
3. On the client device, pick the peer and press **Run suite** (or **Run all
   peers** to sweep every discovered test peer).

**Pass signatures**

- The peer appears in the picker with the correct platform.
- The **summary** shows `fail = 0`; the `discovery` and `direct-dial` groups are
  green (not all-skip).
- `direct-dial` reports `transport=direct`:
  `IROH_INTEROP_CASE id=direct-dial-fetch group=direct-dial outcome=pass latencyMs=… transport=direct`
- The console emits the report:
  `[iroh-http-interop] {"schema":"iroh-http-interop/2",…,"summary":{"total":…,"pass":…,"fail":0,"skip":…,"transport":{"direct":…,"relay":…,"unknown":…}}}`

**Fail signatures**

- Peer never appears → advertise/browse or permissions broken (check the iOS
  local-network prompt and `NSBonjourServices`).
- `direct-dial` **skips** on mobile → no dialable `address` was advertised or
  transport could not be measured; the direct path was not exercised. Capture
  the JSON and file a follow-up.
- Any group reports `fail > 0` or a `Run error:` status.

---

## Criterion 2 — isActive transitions / rebind (both platforms)

Covered by the `discovery` group's **re-advertise re-emits the record (rebind,
W2)** case: the app re-advertises and asserts the browse stream re-surfaces the
changed record instead of suppressing it (the iOS `Set→snapshot` and Android
resolve-queue fixes). It runs as part of **Run suite**.

**Pass:** `IROH_INTEROP_CASE id=discovery-rebind-reemit group=discovery outcome=pass …`
on the device under test.

**Fail:** the rebind case reports `fail` (record never re-emitted) — not `skip`.
A `skip` here means the runtime is not mDNS-capable (never expected on device).

---

## Criterion 5 — generic advertise / browse; iOS metadata-only

Uses the **Discovery** tab's **Generic DNS-SD** advertise/browse (the generic
browse loop emits a greppable line for every record). This is the one check the
Suite runner does not cover, because it verifies the *documented iOS limitation*
rather than a pass/fail behaviour.

**Steps**

1. On device A press **Start advertising** (service `demo-printer`, TXT
   `model`/`color`/`pdl`, port 9100, tcp).
2. On device B press **Start browsing** the same service.

**Pass signatures** (grep `IROH_DNSSD_CHECK check=generic`)

- Android/desktop browser resolves fully:
  `IROH_DNSSD_CHECK check=generic role=browse instance=Front Desk Printer port=9100 host=<h> addrs=<n> isActive=true`
- **iOS browser is metadata-only** — confirm exactly:
  `IROH_DNSSD_CHECK check=generic role=browse instance=Front Desk Printer port=0 host=undefined addrs=0 isActive=true`
  (TXT still present in the on-screen log). This confirms the documented iOS
  limitation, not a bug.

**Fail signatures**

- iOS shows a non-zero `port` / resolved `host` (unexpected — investigate), or
  Android shows `port=0` (resolve failed on a platform that should resolve).

---

## Record results; file follow-ups

Fill the matrix below and paste into #334. File a follow-up issue per divergence.

### Results matrix (paste into #334)

```
### #334 on-device DNS-SD verification results

App commit: <sha>   Runbook: docs/internals/dns-sd-device-verification.md

| Check | iOS ↔ desktop | Android ↔ desktop | Notes |
|-------|:-------------:|:-----------------:|-------|
| 1 discovery group (advertise/browse)  | ☐ pass / ☐ fail | ☐ pass / ☐ fail | |
| 1 direct-dial (transport=direct)      | ☐ pass / ☐ skip | ☐ pass / ☐ skip | |
| 2 discovery rebind (isActive/re-emit) | ☐ pass / ☐ fail | ☐ pass / ☐ fail | |
| http-compliance group                 | __/__ pass      | __/__ pass      | |
| serve-stop group                      | ☐ pass / ☐ fail | ☐ pass / ☐ fail | |
| 5 generic browse; iOS port=0/host=undef | ☐ pass / ☐ fail | ☐ pass / ☐ fail | |

Devices: iOS <model/version>, Android <model/API>, desktop <os/version>.

Follow-ups filed: #____, #____ (none if all pass).

<attach: [iroh-http-interop] /2 JSON + relevant IROH_INTEROP_CASE / IROH_DNSSD_CHECK excerpts>
```

### Filing follow-ups for divergences

Follow the [manage-issues](../../.github/skills/manage-issues/SKILL.md)
conventions. For each failure open a **linked** issue:

- **Title:** `fix(tauri): <symptom> on <platform> DNS-SD <path>` (e.g.
  `fix(tauri): Android discovery rebind not re-emitted under concurrent resolve`).
- **Body sections:** Summary, Evidence (paste the exact `IROH_INTEROP_CASE …` /
  `IROH_DNSSD_CHECK …` lines + device/OS), Impact, Remediation, Acceptance
  criteria.
- **Link:** reference `#334` and note which check failed.
- **Labels:** `bug`, `connectivity` (or repo equivalents).

If everything passes, comment the completed matrix on #334 and close it with a
link to the verifying commit/PR.
