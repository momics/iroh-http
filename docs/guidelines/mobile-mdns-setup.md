# Mobile mDNS / DNS-SD setup (iOS & Android)

Applies to Tauri apps built with `iroh-http-tauri` that use local-network peer
discovery — `node.advertise()` and `node.browse()`.

On desktop, mDNS works with no configuration. On mobile, the OS gates
local-network access behind **permissions** and **static service-type
declarations** that a plugin cannot inject into your app for you. You must add
them to your app's own iOS `Info.plist` and Android `AndroidManifest.xml`.

> **Service name → service type.** `node.advertise({ serviceName })` and
> `node.browse({ serviceName })` map `serviceName` to the DNS-SD service type
> `_<serviceName>._udp`. The default `serviceName` is `"iroh-http"`, i.e.
> `_iroh-http._udp`. Every custom `serviceName` you use needs its own declared
> entry on iOS (see below).

---

## iOS

Two things are required: linking a system framework, and declaring the
local-network permission plus every Bonjour service type you use.

### 1. Link `SystemConfiguration`

iroh references Apple's `SystemConfiguration` framework, which iOS does not
auto-link. Add it to `src-tauri/tauri.conf.json`:

```json
{
  "bundle": {
    "iOS": {
      "frameworks": ["SystemConfiguration"]
    }
  }
}
```

Then regenerate the Xcode project:

```bash
npm run tauri ios init
```

Without this the app fails to link (missing `_kSCNetwork*` / `_kSCProp*`
symbols).

### 2. Declare the Local Network permission and Bonjour services

iOS denies `NWBrowser` / `NWListener` (`NWError -65555 NoAuth`) unless your app
both describes *why* it needs local-network access **and** statically lists
every service type it browses or advertises.

Create `src-tauri/Info.ios.plist` — Tauri merges it into the generated iOS
`Info.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <!-- Shown in the "allow local network access" prompt on first browse. -->
  <key>NSLocalNetworkUsageDescription</key>
  <string>Discover and connect to nearby peers on your local network.</string>

  <!-- One entry per serviceName you browse or advertise.
       serviceName "iroh-http" → _iroh-http._udp -->
  <key>NSBonjourServices</key>
  <array>
    <string>_iroh-http._udp</string>
  </array>
</dict>
</plist>
```

Add a line to `NSBonjourServices` for each additional `serviceName` your app
uses (e.g. `_my-app._udp`).

### The Local Network prompt

iOS asks for Local Network permission the **first time** you browse. Until the
user taps **Allow**, browsing is denied. If they deny it (or you triggered a
browse before granting), re-enable it under
**Settings → Privacy & Security → Local Network → your app**.

---

## Android

Add the network permissions to `src-tauri/gen/android/app/src/main/AndroidManifest.xml`
(or your app's manifest):

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
<uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE" />
```

Android 13+ (API 33+) additionally requires the nearby-devices permission for
network service discovery:

```xml
<uses-permission
  android:name="android.permission.NEARBY_WIFI_DEVICES"
  android:usesPermissionFlags="neverForLocation" />
```

If your app genuinely needs device location alongside discovery, use the
pre-33 location path instead:

```xml
<uses-permission android:name="android.permission.NEARBY_WIFI_DEVICES" />
<uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" />
```

`CHANGE_WIFI_MULTICAST_STATE` is what lets the app receive the multicast mDNS
traffic; without it `NsdManager` discovery silently returns nothing on many
devices.

---

## Verifying discovery on a LAN

- **iOS ↔ iOS / Android**, and **desktop ↔ desktop**, discover each other
  out of the box (same-stack).
- **mobile → desktop** works today.
- **desktop → mobile** requires the desktop side to advertise standard DNS-SD
  records (a `PTR` record in particular). See
  [issue #329](https://github.com/momics/iroh-http/issues/329) — this is being
  standardized so a phone can discover a desktop/server node on the same Wi-Fi.

To sanity-check what is actually on the wire from a Mac, use Apple's built-in
browser:

```bash
dns-sd -B _iroh-http._udp local     # list advertisers of the default service
```

If your advertiser does not appear here, iOS `NWBrowser` will not see it either
— both rely on the same mDNSResponder.
