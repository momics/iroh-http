# On-device DNS-SD release-candidate verification

Use this runbook before tagging a release that changes discovery, the Tauri
mobile bridge, mobile permissions, or the example test harness. CI compiles the
Swift and Kotlin adapters and runs deterministic lifecycle contracts, but it
cannot prove multicast visibility, OS permission behavior, or direct LAN
connectivity on physical devices.

Run the example app's **Test** tab against one desktop, one physical iOS device,
and one physical Android device. The suite in
[`tests/interop/suite.mjs`](../../tests/interop/suite.mjs) reports grouped
pass/fail/skip results and emits one greppable line per case.

| Group             | Required evidence                                                              |
| ----------------- | ------------------------------------------------------------------------------ |
| `discovery`       | self advertisement is visible and dialable; re-advertising re-emits the record |
| `direct-dial`     | fetch reaches the selected peer with `transport=direct`                        |
| `relay-fallback`  | a relay-only fetch succeeds when that precondition is available                |
| `http-compliance` | the shared `cases.json` corpus passes against the selected peer                |
| `serve-stop`      | serve becomes reachable, stops, then refuses new requests                      |

Cases that cannot measure their precondition report **skip**, not pass. On Tauri
mobile and desktop the harness sets `mdnsCapable: true`, so a discovery case
that observes nothing is a failure. The discovery and direct-dial groups must
not be all-skip for a release-candidate device run.

## 1. Record the candidate and devices

Test an exact commit, not an unrecorded working tree. Record:

- candidate commit SHA;
- desktop OS and architecture;
- iOS device model and iOS version;
- Android device model, API level, and T extension level;
- Wi-Fi network used and whether client isolation is disabled.

Useful Android commands:

```sh
adb shell getprop ro.build.version.sdk
adb shell getprop build.version.extensions.t
```

If the extension property is unavailable, record it as unknown. The plugin's
legacy multicast-lock path runs on Android 12 and older, and on Android 13
devices before T extension 7. A modern device does not exercise that branch.

## 2. Build and install from a clean checkout

Install dependencies and build the unpublished TypeScript packages before
starting any target:

```sh
# Repository root
npm ci
npm run build:shared
npm run build:tauri

cd examples/tauri
npm ci
```

Then start each target from `examples/tauri`:

```sh
# Desktop
npm run tauri dev

# Physical iOS device; initialize once and configure Xcode signing first
npm run tauri ios init
npm run tauri ios dev

# Physical Android device with USB debugging enabled; initialize once
npm run tauri android init
npm run tauri android dev
```

The checked-in example configuration links `SystemConfiguration`, declares
`_iroh-http._udp`, `_iroh-http-test._udp`, and `_demo-printer._tcp` on iOS, and
grants the Tauri discovery capability. On the first iOS browse or advertisement,
tap **Allow** on the Local Network prompt. If it was denied, re-enable the app
under **Settings → Privacy & Security → Local Network**.

Keep all three apps in the foreground on the same non-isolated LAN. Disable VPN
or per-app network filtering for the test. A relay path may still exist, but the
direct-dial assertion below proves that the LAN path was used.

## 3. Capture private evidence

The suite emits `IROH_INTEROP_CASE` lines, a complete JSON report under
`[iroh-http-interop]`, and `IROH_DNSSD_CHECK` lines for generic and bound-port
checks.

```sh
# Android
adb logcat | grep --line-buffered -E \
  'IROH_INTEROP_CASE|iroh-http-interop|IROH_DNSSD_CHECK'

# iOS (Console.app is also suitable)
xcrun devicectl device console --device <UDID> | \
  grep -E 'IROH_INTEROP_CASE|iroh-http-interop|IROH_DNSSD_CHECK'

# Desktop: use the terminal running `tauri dev`.
```

Optional automatic collection is available from the repository root:

```sh
npm run build:node
npm run report:serve
```

Paste the printed collector node ID into each app's **Test** tab and enable
automatic submission. Store raw reports with private release evidence. A public
release issue should receive only the sanitized result matrix and links to any
follow-up issues: node IDs, local socket addresses, device labels, and network
metadata can identify a device or LAN.

The per-case line format is:

```text
IROH_INTEROP_CASE id=<case> group=<group> outcome=<pass|fail|skip> latencyMs=<n> [transport=<direct|relay>]
```

## 4. Run the cross-platform suite

1. On all three apps, open **Test** and enable **Testing mode**. Each app starts
   its compliance server, advertises `_iroh-http-test._udp`, and browses for the
   other candidates.
2. Wait for both other platforms to appear in every peer picker with the correct
   platform label.
3. Run the smallest directed cycle that exercises each runtime once as client
   and once as server:
   - Android → iOS;
   - iOS → desktop;
   - desktop → Android.
4. Use **Run all peers** instead when making a major discovery change or when
   investigating a platform-pair asymmetry.

Every required run must show:

- `fail = 0` and no `Run error`;
- `discovery-advertise-browse` passes;
- `discovery-rebind-reemit` passes, not skips;
- `direct-dial-fetch` passes, not skips, with `transport=direct`;
- the HTTP-compliance and serve-stop groups pass.

The direct-path signature is:

```text
IROH_INTEROP_CASE id=direct-dial-fetch group=direct-dial outcome=pass latencyMs=… transport=direct
```

Testing mode also advertises the endpoint's already-bound QUIC port. On iOS, the
status must say **Serving + advertising already-bound UDP port `<port>`** with a
port greater than 1. Its evidence must show an attempted advertisement, a
matching active browse record, and no advertisement failure:

```text
IROH_DNSSD_CHECK check=bound-port role=advertise port=<port> alreadyBound=true outcome=attempt
IROH_DNSSD_CHECK check=bound-port role=browse port=<same-port> addrs=<n> alreadyBound=true isActive=true
```

The browse record must have at least one address, and the complete suite against
that iOS target must retain `transport=direct`.

## 5. Exercise generic DNS-SD

The suite specializes DNS-SD for iroh peers. Test the lossless generic surface
separately from **Discovery → Generic DNS-SD**:

1. On all three apps, press **Start advertising** for the default `demo-printer`
   TCP service.
2. On all three apps, press **Start browsing** for the same service.
3. Confirm that every browser observes both other platforms.

Android and desktop resolve full records:

```text
IROH_DNSSD_CHECK check=generic role=browse instance=Front Desk Printer … port=9100 host=<host> addrs=<n> isActive=true
```

iOS intentionally exposes metadata and TXT only, because `NWBrowser` does not
resolve the endpoint without an `NWConnection`:

```text
IROH_DNSSD_CHECK check=generic role=browse instance=Front Desk Printer … port=0 host=undefined addrs=0 isActive=true
```

The iOS on-screen record must still contain the advertised TXT fields. Android
or desktop returning `port=0`, or a platform failing to see an advertiser, is a
release-blocking failure.

## 6. Verify lifecycle and legacy Android multicast

On Android, with Testing mode enabled, both a browse and an advertisement are
active concurrently:

1. Confirm the iOS and desktop peers are visible.
2. Disable Testing mode. Confirm the Android peer disappears from the other
   pickers after the DNS-SD loss/expiry transition.
3. Re-enable Testing mode. Confirm the Android peer reappears as a new active
   record.
4. Run Android → desktop once more and require a direct-dial pass.

Perform this cycle on an Android 12-or-older device, or an Android 13 device
before T extension 7, to exercise the plugin's shared
`WifiManager.MulticastLock` acquisition and final-session release. If only a
modern Android device is available, record the legacy branch as **not physically
verified**. The deterministic Android contract test may support an explicit
maintainer risk acceptance, but must not be reported as an on-device pass.

## 7. Record the release gate

Attach the completed matrix to the current release-readiness issue or private
release record. Do not reopen or update historical verification issues solely to
store a new release's evidence.

```md
### On-device DNS-SD release-candidate results

Candidate commit: `<sha>`

| Device  | Version / API                   | T extension          | Notes                    |
| ------- | ------------------------------- | -------------------- | ------------------------ |
| Desktop | `<OS, version, architecture>`   | n/a                  |                          |
| iOS     | `<model, iOS version>`          | n/a                  | Local Network allowed    |
| Android | `<model, Android version, API>` | `<value or unknown>` | Legacy lock path: yes/no |

| Check                                              |        Result        | Evidence / notes      |
| -------------------------------------------------- | :------------------: | --------------------- |
| All peers visible on all three pickers             |      pass/fail       |                       |
| Android → iOS full suite                           |      pass/fail       | direct: pass/skip     |
| iOS → desktop full suite                           |      pass/fail       | direct: pass/skip     |
| Desktop → Android full suite                       |      pass/fail       | direct: pass/skip     |
| iOS already-bound advertisement                    |      pass/fail       | port: `<n>`           |
| Generic browse on Android                          |      pass/fail       | full records: yes/no  |
| Generic browse on desktop                          |      pass/fail       | full records: yes/no  |
| Generic browse on iOS                              |      pass/fail       | metadata-only: yes/no |
| Android disable → disappear → re-enable → reappear |      pass/fail       |                       |
| Legacy Android multicast branch                    | pass/fail/not tested | device/API evidence   |

- Collector/report attachments: `<private links or paths>`
- Follow-up issues: `<links, or none>`
- Maintainer release approval: `<name/date>`
```

Any required failure blocks tagging. Open one linked issue per divergence with
the exact case line, report excerpt, candidate SHA, device/OS details, expected
behavior, and reproduction steps. A release may proceed without a physical
legacy-Android result only after the matrix records `not tested` and a
maintainer explicitly accepts the residual risk.
