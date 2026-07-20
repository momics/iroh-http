# iroh-http Tauri example

Interactive example and cross-platform test harness for
`@momics/iroh-http-tauri`. It runs on Tauri desktop, iOS, and Android and
exercises HTTP, peer discovery, generic DNS-SD, sessions, and lifecycle flows.

## Prerequisites

- Install the normal
  [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for each
  platform you intend to build.
- For iOS or Android, initialize the generated mobile project once as described
  below.

From a clean checkout, install both dependency trees and build the local
TypeScript packages before starting the example:

```sh
# Repository root
npm ci
npm run build:shared
npm run build:tauri

# The example is deliberately not a root npm workspace.
cd examples/tauri
npm ci
```

The example's Rust plugin is linked directly from the workspace source. Its
frontend dependencies resolve the generated `dist` output built above, so do not
skip those build commands when testing an unpublished branch.

## Desktop

```sh
cd examples/tauri
npm run tauri dev
```

The app uses the workspace Rust plugin and the freshly built local TypeScript
packages, so this command exercises the current checkout rather than the last
published package.

## iOS

The checked-in `src-tauri/Info.ios.plist` declares local-network access and the
default `_iroh-http._udp` plus test service types. `tauri.conf.json` links the
`SystemConfiguration` framework required by iroh.

```sh
cd examples/tauri
npm run tauri ios init       # first time, or after changing native config
npm run tauri ios dev        # choose a connected device or simulator
```

Accept the Local Network prompt when the app first browses or advertises. If it
was denied earlier, enable the app under **Settings → Privacy & Security → Local
Network**. Physical iOS devices also require a valid Apple development
team/signing identity in the generated Xcode project; simulator builds do not.

## Android

```sh
cd examples/tauri
npm run tauri android init   # first time
npm run tauri android dev    # choose a connected device or emulator
```

The plugin contributes `ACCESS_NETWORK_STATE` and `CHANGE_WIFI_MULTICAST_STATE`
to the merged application manifest. Tauri normally supplies `INTERNET`; verify
the final merged manifest if your application customizes its Android project.
See the [mobile DNS-SD setup guide](../../docs/guidelines/mobile-mdns-setup.md)
for the plugin/application split and future Android 17 permission flow.
`android dev` creates a debug-signed build. Configure an Android keystore in the
normal Tauri/Gradle release flow before distributing a release APK or bundle.

## Tauri capabilities

The checked-in `src-tauri/capabilities/default.json` grants the example the
plugin's fetch, serve, session, crypto, and discovery capabilities. Production
apps should grant only the operations their frontend uses; local peer and
generic DNS-SD APIs require `iroh-http:discovery`.

## Discovery test

Put all devices on the same non-isolated LAN and keep the mobile apps in the
foreground. On each app:

1. Open **Test** and enable **Testing mode**.
2. Wait for the other devices to appear in the **Suite runner** peer picker.
3. Select a peer and press **Run suite**, or press **Run all peers**.
4. Require `fail = 0`. The discovery, rebind, and direct-dial checks must pass,
   not skip; direct dial must report `transport=direct`.
5. Open **Discovery → Generic DNS-SD** and exercise both **Start advertising**
   and **Start browsing** for the `demo-printer` TCP service.

For automatic evidence collection, start the optional collector from the
repository root after building the Node adapter:

```sh
npm run build:node
npm run report:serve
```

Paste the collector node ID into each app's **Test** tab and enable automatic
submission. Do not publish collected device logs: they can contain node IDs,
local socket addresses, device labels, and network metadata.

The reusable release-candidate procedure, expected log signatures, lifecycle
cycle, and result matrix live in the
[on-device DNS-SD verification runbook](../../docs/internals/dns-sd-device-verification.md).
Firewalls, guest-network client isolation, or an inactive mobile app can prevent
multicast visibility.

For the complete permission model and custom service-name configuration, see
[Mobile mDNS / DNS-SD setup](../../docs/guidelines/mobile-mdns-setup.md).
