# Mobile mDNS / DNS-SD setup (iOS & Android)

Applies to Tauri apps built with `iroh-http-tauri` that use local-network
discovery — the iroh peer path (`node.advertisePeer()` / `node.browsePeers()`)
and the generic DNS-SD path (`node.advertise()` / `node.browse()`). Both run over
the same native bridge (`NsdManager` on Android, `NWBrowser` / `NetService` on
iOS) and need the same permissions and service-type declarations below.

> **Generic records on iOS.** Android resolves full generic records (host, port,
> TXT, addresses). On iOS, `NWBrowser` yields the instance name, service type
> and TXT but not the host/port/addresses (resolving those needs an
> `NWConnection`), so iOS generic records arrive with `host = null`, `port = 0`
> and empty `addrs`. The iroh peer path is unaffected: the shared Rust
> projection reads the peer identity and dial addresses from its canonical TXT
> record and feeds them into the endpoint lookup.

> **Tauri permission.** Grant `iroh-http:discovery` in your capability file; it
> covers both the peer and generic discovery commands.

On desktop, mDNS works with no configuration. On mobile, the OS gates
local-network access behind **permissions** and **static service-type
declarations** that a plugin cannot inject into your app for you. You must add
them to your app's own iOS `Info.plist` and Android `AndroidManifest.xml`.

> **Service name → service type.** `node.advertisePeer({ serviceName })` and
> `node.browsePeers({ serviceName })` map `serviceName` to the DNS-SD service type
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

iOS denies `NWBrowser` / `NetService` (`NWError -65555 NoAuth`) unless your app
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

The plugin merges the network-state and multicast-state declarations it needs
into the application manifest. Tauri applications normally already declare
`INTERNET`; if yours does not, add it to
`src-tauri/gen/android/app/src/main/AndroidManifest.xml`:

```xml
<uses-permission android:name="android.permission.INTERNET" />
```

On Android 12 and older, and Android 13 devices before T extension 7, the plugin
shares one `WifiManager.MulticastLock` across active DNS-SD browse and
advertisement sessions and releases it after the last native session becomes
terminal. Starting at T extension 7, foreground apps receive multicast through
the system and the plugin does not acquire a lock.
See Android's [`NsdManager` multicast-lock guidance](https://developer.android.com/reference/android/net/nsd/NsdManager#wi-fi-multicast-lock).

`NEARBY_WIFI_DEVICES` is a Wi-Fi management permission and is not required for
the `NsdManager` API used here. Starting with Android 17, applications that
*target API 37 or newer* must instead declare and request the runtime
`ACCESS_LOCAL_NETWORK` permission (or adopt a system-mediated picker). The
current plugin compiles against API 34, so do not add that future permission
until the application upgrades its target and implements the runtime prompt.
See Android's [Local network permission](https://developer.android.com/privacy-and-security/local-network-permission)
guide for the target-SDK transition.

---

## Verifying discovery on a LAN

- **desktop ↔ desktop**, **desktop ↔ mobile**, and **iOS ↔ Android** are all
  supported when both apps are active on the same LAN and use the same service
  name.
- Guest-network client isolation, platform firewalls, and background execution
  limits can still prevent multicast visibility.

To sanity-check what is actually on the wire from a Mac, use Apple's built-in
browser:

```bash
dns-sd -B _iroh-http._udp local     # list advertisers of the default service
```

If your advertiser does not appear here, iOS `NWBrowser` will not see it either
— both rely on the same mDNSResponder.
