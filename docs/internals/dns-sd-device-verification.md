# On-device DNS-SD verification runbook (#334)

Standard DNS-SD discovery ([ADR-017](../adr/017-standard-dns-sd-discovery.md),
#330) shipped mobile-native Tauri plugin code that **no CI job compiles or
runs** — there is no iOS or Android runner. Two of those changes are defect
fixes that need confirmation on real hardware:

- **Android serialized resolve queue**
  (`packages/iroh-http-tauri/android/.../IrohHttpPlugin.kt`) —
  `NsdManager.resolveService()` allows only one outstanding resolve; concurrent
  calls previously failed with `FAILURE_ALREADY_ACTIVE` and silently dropped
  records. `enqueueResolve`/`drainResolveQueue` now serialize resolves across
  the peer (`browse_peers_start`) and generic (`browse_start`) paths.
- **iOS re-emit on record change**
  (`packages/iroh-http-tauri/ios/Sources/IrohHttpPlugin.swift`) — the generic
  `browse_start` dedup changed from a one-shot `Set<String>` to
  `[String: DnsSdRecordSnapshot]` (snapshot = **TXT + addrs**), so a known
  instance whose record changes is re-surfaced instead of suppressed forever.

This runbook maps each acceptance criterion to concrete steps in the example
app's **Test** tab and the exact greppable log signatures that mean pass vs
fail. It ends with a results matrix ready to paste into
[#334](https://github.com/Momics/iroh-http/issues/334).

> **Important — snapshot excludes port.** iOS dedups on TXT + addrs only (its
> generic browse never resolves an endpoint, so `port` is always `0` and `host`
> is `undefined`). A **port-only** change is therefore invisible on iOS; the
> re-emit check must change a **TXT** value. The mutate control changes TXT
> (and also toggles port, which additionally exercises Android's SRV
> re-resolve).

---

## 0. Setup

**Hardware:** one desktop (macOS/Linux/Windows), one iPhone/iPad, one Android
phone/tablet — all on the **same LAN / Wi-Fi** with client isolation OFF.

**Permissions:** confirm the iOS `Info.plist` (`NSLocalNetworkUsageDescription`
+ `NSBonjourServices`) and Android `AndroidManifest.xml` entries per
[Mobile mDNS / DNS-SD setup](../guidelines/mobile-mdns-setup.md). On first
launch iOS shows a local-network permission prompt — **Allow** it.

**Build & install the example app** (`examples/tauri`) on each device:

```sh
# desktop
cd examples/tauri && npm run tauri dev
# iOS device (Xcode signing required)
npm run tauri ios dev
# Android device (USB debugging on)
npm run tauri android dev
```

**Attach to logs** (all check lines share the `IROH_DNSSD_CHECK` prefix):

```sh
# Android — native + webview console both reach logcat
adb logcat | grep --line-buffered IROH_DNSSD_CHECK
# iOS — device console (Console.app, or:)
xcrun devicectl device console --device <UDID> | grep IROH_DNSSD_CHECK
# desktop — lines print to the terminal running `tauri dev`
```

Open the **Test** tab on each device and scroll to **DNS-SD device checks
(#334)**. Every control mirrors its log lines into an on-screen `<pre>`, so you
can verify without a cable if needed.

**Log line grammar:** `IROH_DNSSD_CHECK <check> <k>=<v> …` — single line,
space-separated, greppable. Native Android/iOS lines use the same prefix.

---

## Criterion 1 — advertise/discover + auto-dial (iOS↔desktop, Android↔desktop)

**Steps** (run once for the iOS↔desktop pair, once for Android↔desktop):

1. On both devices in the pair, open **Test** → toggle **Enable testing mode**.
2. Each device advertises `test=1`+`platform` and browses; within a few seconds
   each shows the other under **Discovered test peers**.
3. On the client device, select the peer and press **Run against selected
   peer**. Case 0 is a self/loopback baseline; cases 1..N dial the peer.

**Pass signatures**

- Peer appears in the list with the correct `platform`.
- Console emits the interop report:
  `[iroh-http-interop] {"schema":"iroh-http-interop/1",…,"summary":{"total":…,"passed":…,"failed":0}}`
- `summary.failed == 0` and the `self-loopback` case passed (isolates transport
  from platform).

**Fail signatures**

- Peer never appears → advertise/browse or permissions broken.
- Report present but `failed > 0`, or a `Run error:` status → auto-dial or
  transport failure; capture the JSON and file a follow-up.

---

## Criterion 2 — browsePeers() isActive transitions (both platforms)

Uses the **browsePeers() isActive transitions** card.

**Steps**

1. On the device under test (DUT), press **Start isActive watch**.
2. On a second device, go to **Discovery** → **Advertise peer** with service
   `iroh-http` and press **Start advertising**. Wait ~5 s.
3. On the second device, press **Stop advertising** (or background the app).

**Pass signatures** (grep `IROH_DNSSD_CHECK check=peers`)

- On arrival:
  `IROH_DNSSD_CHECK check=peers role=browse isActive=true nodeId=<16hex> addrs=<n>`
- On expiry:
  `IROH_DNSSD_CHECK check=peers role=browse isActive=false nodeId=<16hex> addrs=0`
- The same `nodeId` shows `true` then later `false`.

**Fail signatures**

- `isActive=true` never arrives → browse not seeing the peer.
- `isActive=false` never arrives after the advertiser stops → expiry not
  reported (the transition bug). File a follow-up.

Run with the DUT = iOS, then DUT = Android.

---

## Criterion 3 — Android multi-peer burst drops ZERO records (resolve queue)

Uses the **Android resolve-queue burst** card. DUT = **Android**.

**Steps**

1. On the Android DUT, press **Browse burst**.
2. On a second device (desktop is easiest), set **Count = N** (e.g. 5–10) and
   press **Burst advertise** — this fires N `advertise()` calls simultaneously,
   so N records appear together and contend for Android's single resolve slot.

**Pass signatures**

- Advertiser: `IROH_DNSSD_CHECK check=burst role=advertise count=<N> tag=<tag> …`
- Android native queue trace (proves serialization, not dropping):
  `IROH_DNSSD_CHECK resolve dequeue instance=burst-<tag>-<i> depth=<d>` followed
  by `IROH_DNSSD_CHECK resolve ok instance=burst-<tag>-<i> port=…` for each `i`.
- Browse count climbs to N:
  `IROH_DNSSD_CHECK check=burst role=browse instance=burst-<tag>-<i> isActive=true resolved=<M>`
  with **`resolved` reaching N** and all `i` in `0..N-1` seen exactly once.

**Fail signatures**

- `resolved` plateaus below N (missing instances) → records dropped.
- Native log shows `resolve fail … errorCode=3` (`FAILURE_ALREADY_ACTIVE`) for
  queued items → the queue is not serializing. File a follow-up with the full
  `IROH_DNSSD_CHECK resolve …` sequence attached.

> Repeat 2–3 times; the queue defect was intermittent by nature.

---

## Criterion 4 — iOS re-emits on TXT change of a known instance

Uses the **iOS re-emit on TXT change** card. DUT = **iOS** (browsing side).

**Steps**

1. On the iOS DUT, press **Browse mutations**.
2. On a second device, press **Advertise mutable** (publishes `rev=0`), wait for
   the iOS DUT to log it, then press **Mutate** two or three times (bumps TXT
   `rev` and toggles port). Wait ~2–3 s between mutations.

**Pass signatures**

- Advertiser: `IROH_DNSSD_CHECK check=mutate role=advertise instance=mutate-<tag> rev=0 port=1`,
  then `rev=1 port=2`, `rev=2 port=1`, …
- iOS native re-emit trace: `IROH_DNSSD_CHECK reemit instance=mutate-<tag> event=new rev=0`,
  then `event=reemit rev=1`, `event=reemit rev=2`, …
- iOS browse surfaces each new rev:
  `IROH_DNSSD_CHECK check=mutate role=browse instance=mutate-<tag> rev=1 port=0 isActive=true`
  (note `port=0` — iOS metadata-only), then `rev=2`, …

**Fail signatures**

- After the first sighting, iOS never logs a new `rev` even though the
  advertiser bumped it → re-emit suppressed (the one-shot-Set defect). The
  native line would stay at `event=new` / never emit `event=reemit`. File a
  follow-up.

Cross-check on Android (should also re-emit, and additionally reflect the port
change): browse `rev` advances and `port` toggles.

---

## Criterion 5 — generic advertise/browse; iOS metadata-only

Uses the existing **Discovery** tab's **Generic DNS-SD** advertise/browse (the
generic browse loop emits a greppable line for every record).

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

## Criterion 6 — record results; file follow-ups

Fill the matrix below and paste into #334. File a follow-up issue per divergence
(see next section).

### Results matrix (paste into #334)

```
### #334 on-device DNS-SD verification results

App commit: <sha>   Runbook: docs/internals/dns-sd-device-verification.md

| Criterion | iOS ↔ desktop | Android ↔ desktop | Notes |
|-----------|:-------------:|:-----------------:|-------|
| 1 advertise/discover + auto-dial      | ☐ pass / ☐ fail | ☐ pass / ☐ fail | |
| 2 browsePeers isActive true→false     | ☐ pass / ☐ fail | ☐ pass / ☐ fail | |
| 3 Android burst drops zero (N=__)     |      n/a        | ☐ pass / ☐ fail | resolved=__/__ |
| 4 iOS re-emit on TXT change (revs __) | ☐ pass / ☐ fail |   cross-check   | |
| 5 generic browse; iOS port=0/host=undef | ☐ pass / ☐ fail | ☐ pass / ☐ fail | |

Devices: iOS <model/version>, Android <model/API>, desktop <os/version>.

Follow-ups filed: #____, #____ (none if all pass).

<attach: [iroh-http-interop] JSON + relevant IROH_DNSSD_CHECK log excerpts>
```

### Filing follow-ups for divergences

Follow the [manage-issues](../../.github/skills/manage-issues/SKILL.md)
conventions. For each failure open a **linked** issue:

- **Title:** `fix(tauri): <symptom> on <platform> DNS-SD <path>` (e.g.
  `fix(tauri): Android burst drops records under concurrent resolve`).
- **Body sections:** Summary, Evidence (paste the exact `IROH_DNSSD_CHECK …`
  lines + device/OS), Impact, Remediation, Acceptance criteria.
- **Link:** reference `#334` and note which criterion/row failed.
- **Labels:** `bug`, `tauri`, `discovery` (or repo equivalents).

If everything passes, comment the completed matrix on #334 and close it with a
link to the verifying commit/PR.
