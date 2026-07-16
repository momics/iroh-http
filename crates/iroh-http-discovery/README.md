# iroh-http-discovery

Optional local-network peer discovery for [iroh-http](https://github.com/momics/iroh-http).

Implements standard [DNS-SD](https://datatracker.ietf.org/doc/html/rfc6763) over
mDNS (via [`mdns-sd`](https://crates.io/crates/mdns-sd)), publishing `PTR` + `SRV`
+ `TXT` + `A`/`AAAA` records so nodes find each other on the same local network
without relay servers. Because it speaks the same wire format as Apple's
mDNSResponder (`NWBrowser`) and Android's `NsdManager`, desktop nodes are
discoverable from mobile.

> **Note:** This crate is included automatically when you use the platform adapters with default features. You only need to depend on it directly if you are building a custom Rust integration on top of `iroh-http-core`.
> - **Node.js** → [`@momics/iroh-http-node`](https://www.npmjs.com/package/@momics/iroh-http-node)
> - **Deno** → [`@momics/iroh-http-deno`](https://jsr.io/@momics/iroh-http-deno)
> - **Tauri** → [`@momics/iroh-http-tauri`](https://www.npmjs.com/package/@momics/iroh-http-tauri)

## When to use

- **Desktop apps** (macOS, Linux, Windows) that need local peer discovery
- **Node.js** servers on a LAN

## Platform architecture

- **Desktop** uses this crate's `mdns-sd` transport.
- **iOS/Android** use `NWBrowser`/`NetService` and `NsdManager` as transport
  adapters, then reuse this crate's platform-neutral session engine and peer
  projection through the Tauri plugin.
- **Environments with only relay/DNS discovery** do not need local DNS-SD.

## Usage

This crate is typically not used directly. Enable the `mdns` feature in your platform adapter or pass it to `iroh-http-core` via `NodeOptions`:

```ts
const node = await createNode({
  discovery: { mdns: true, serviceName: "my-app" }
});
```

## License

`MIT OR Apache-2.0`
