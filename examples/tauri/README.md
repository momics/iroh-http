# iroh-http Tauri example

Interactive example and cross-platform test harness for
`@momics/iroh-http-tauri`. It runs on Tauri desktop, iOS, and Android and
exercises HTTP, peer discovery, generic DNS-SD, sessions, and lifecycle flows.

## Prerequisites

- Install the repository dependencies from the repository root with `npm ci`.
- Install the normal
  [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for each
  platform you intend to build.
- For iOS or Android, initialize the generated mobile project once as described
  below.

## Desktop

```sh
cd examples/tauri
npm run tauri dev
```

The app uses the workspace plugin and shared package directly, so rebuilding it
includes the current Rust plugin and frontend TypeScript.

## iOS

The checked-in `src-tauri/Info.ios.plist` declares local-network access and the
default `_iroh-http._udp` plus test service types. `tauri.conf.json` links the
`SystemConfiguration` framework required by iroh.

```sh
cd examples/tauri
npm run tauri ios init       # first time, or after changing native config
npm run tauri ios dev        # choose a connected device or simulator
```

Accept the Local Network prompt on the device. If it was denied earlier, enable
the app under **Settings → Privacy & Security → Local Network**. Physical iOS
devices also require a valid Apple development team/signing identity in the
generated Xcode project; simulator builds do not.

## Android

```sh
cd examples/tauri
npm run tauri android init   # first time
npm run tauri android dev    # choose a connected device or emulator
```

The generated application manifest must include the permissions listed in the
[mobile DNS-SD setup guide](../../docs/guidelines/mobile-mdns-setup.md). The
plugin contributes its own required manifest entries; keep application-level
permissions in the generated app manifest when Android requires runtime consent.
`android dev` creates a debug-signed build. Configure an Android keystore in the
normal Tauri/Gradle release flow before distributing a release APK or bundle.

## Tauri capabilities

The checked-in `src-tauri/capabilities/default.json` grants the example the
plugin's fetch, serve, session, crypto, and discovery capabilities. Production
apps should grant only the operations their frontend uses; local peer and
generic DNS-SD APIs require `iroh-http:discovery`.

## Discovery test

Put all devices on the same LAN, start this app on each platform, and run the
suite. Peer discovery should be bidirectional across desktop, iOS, and Android.
The generic DNS-SD section also advertises and browses a demo service.
Firewalls, guest-network client isolation, or an inactive mobile app can prevent
multicast visibility.

For the complete permission model and custom service-name configuration, see
[Mobile mDNS / DNS-SD setup](../../docs/guidelines/mobile-mdns-setup.md).
