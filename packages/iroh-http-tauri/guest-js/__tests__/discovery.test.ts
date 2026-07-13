/**
 * Regression tests for `asIrohPeer` address handling (#346).
 *
 * On real hardware an iOS advertiser was undialable because the browsing peer
 * received a BARE, port-less IP (the resolved mDNS A-record host, e.g.
 * "192.168.50.227") in `ServiceRecord.addrs`. That value fails to parse as a
 * socket address at dial time ("invalid socket address syntax"), poisoning the
 * whole direct-address list — while the dialable `address` TXT entry (which
 * carries the real bound QUIC port) was ignored entirely.
 */

import { describe, expect, it } from "vitest";
import {
  asIrohPeer,
  isDialableSocketAddr,
  type ServiceRecord,
} from "@momics/iroh-http-shared";

function record(partial: Partial<ServiceRecord>): ServiceRecord {
  return {
    isActive: true,
    serviceType: "_iroh-http._udp.local.",
    instanceName: "peerlabel",
    host: undefined,
    port: 0,
    addrs: [],
    txt: {},
    ...partial,
  };
}

describe("asIrohPeer address handling (#346)", () => {
  it("surfaces the dialable `address` TXT entry", () => {
    const peer = asIrohPeer(
      record({
        txt: { pk: "nodeabc", address: "192.168.50.227:62546" },
        addrs: [],
      }),
    );
    expect(peer).not.toBeNull();
    expect(peer?.addrs).toContain("192.168.50.227:62546");
  });

  // #348: a device on several routable interfaces advertises a comma-separated
  // `address` TXT (e.g. a VPN `10.x` plus the real LAN `192.168.x`). asIrohPeer
  // must surface every dialable candidate so iroh can race the paths and reach
  // the peer over whichever interface is actually reachable.
  it("splits a comma-separated `address` TXT into all dialable candidates", () => {
    const peer = asIrohPeer(
      record({
        txt: {
          pk: "nodeabc",
          address: "10.12.222.17:56604,192.168.50.227:56604",
        },
        addrs: [],
      }),
    );
    expect(peer).not.toBeNull();
    expect(peer?.addrs).toContain("10.12.222.17:56604");
    expect(peer?.addrs).toContain("192.168.50.227:56604");
  });

  it("drops undialable entries within a comma-separated `address` TXT", () => {
    const peer = asIrohPeer(
      record({
        // A bare (port-less) host and a whitespace-padded valid addr; only the
        // valid `ip:port` candidates survive.
        txt: {
          pk: "nodeabc",
          address: "192.168.50.227, 10.0.0.5:56604 ,192.168.50.227:56604",
        },
        addrs: [],
      }),
    );
    expect(peer).not.toBeNull();
    expect(peer?.addrs).not.toContain("192.168.50.227");
    expect(peer?.addrs).toContain("10.0.0.5:56604");
    expect(peer?.addrs).toContain("192.168.50.227:56604");
  });

  it("drops a bare, port-less A-record host so it never reaches the dialer", () => {
    const peer = asIrohPeer(
      record({
        txt: { pk: "nodeabc", address: "192.168.50.227:62546" },
        // The Android generic browse resolves the host and reports the bare IP.
        addrs: ["192.168.50.227"],
      }),
    );
    expect(peer).not.toBeNull();
    // The bare, port-less host must not appear — it is undialable and would
    // fail `parse_direct_addrs` for the entire list.
    expect(peer?.addrs).not.toContain("192.168.50.227");
    // The dialable address (with the real bound QUIC port) is what survives.
    expect(peer?.addrs).toContain("192.168.50.227:62546");
  });

  it("drops a port-zero address", () => {
    const peer = asIrohPeer(
      record({
        txt: { pk: "nodeabc" },
        addrs: ["192.168.50.227:0"],
      }),
    );
    expect(peer?.addrs).not.toContain("192.168.50.227:0");
  });

  it("keeps well-formed ip:port socket addresses", () => {
    const peer = asIrohPeer(
      record({
        txt: { pk: "nodeabc" },
        addrs: ["10.0.0.5:4433", "[fe80::1]:4433"],
      }),
    );
    expect(peer?.addrs).toContain("10.0.0.5:4433");
    expect(peer?.addrs).toContain("[fe80::1]:4433");
  });

  // Regression: #350 pre-merge review W3 — hardening the direct-address list to
  // `ip:port` must not drop relay URLs. A peer behind NAT / off-LAN is only
  // reachable via its home relay, so `asIrohPeer` must still surface it.
  it("surfaces the relay URL from the `relay` TXT entry", () => {
    const peer = asIrohPeer(
      record({
        txt: { pk: "nodeabc", relay: "https://relay.example" },
        addrs: [],
      }),
    );
    expect(peer).not.toBeNull();
    expect(peer?.addrs).toContain("https://relay.example");
  });

  it("keeps a relay URL that arrives in the resolved addrs list", () => {
    // iOS generic browse pushes the relay URL into `addrs` directly.
    const peer = asIrohPeer(
      record({
        txt: { pk: "nodeabc" },
        addrs: ["https://relay.example"],
      }),
    );
    expect(peer?.addrs).toContain("https://relay.example");
  });

  it("surfaces both a direct ip:port and the relay, dropping the bare host", () => {
    const peer = asIrohPeer(
      record({
        txt: {
          pk: "nodeabc",
          address: "192.168.50.227:62546",
          relay: "https://relay.example",
        },
        addrs: ["192.168.50.227"],
      }),
    );
    expect(peer?.addrs).toContain("192.168.50.227:62546");
    expect(peer?.addrs).toContain("https://relay.example");
    expect(peer?.addrs).not.toContain("192.168.50.227");
  });

  // Regression: #350 review M1 — the port was validated with `Number()`, which
  // accepts "0x1a", "4e2", " 443", "1_000". The raw string then reached the
  // dialer, where parse_direct_addrs errors and drops the ENTIRE direct-addr
  // list (all-or-nothing), leaving the peer relay-only — the exact failure the
  // guard exists to prevent, moved one layer in.
  it.each([
    "1.2.3.4:0x1a",
    "1.2.3.4:4e2",
    "1.2.3.4: 443",
    "1.2.3.4:443 ",
    "1.2.3.4:1_000",
    "1.2.3.4:+443",
  ])("drops the non-decimal / malformed port address %j", (bad) => {
    const peer = asIrohPeer(record({ txt: { pk: "nodeabc" }, addrs: [bad] }));
    expect(peer?.addrs).not.toContain(bad);
    expect(peer?.addrs).toHaveLength(0);
  });

  it("still accepts strictly-decimal ports", () => {
    const peer = asIrohPeer(
      record({
        txt: { pk: "nodeabc" },
        addrs: ["1.2.3.4:443", "[2001:db8::1]:443"],
      }),
    );
    expect(peer?.addrs).toContain("1.2.3.4:443");
    expect(peer?.addrs).toContain("[2001:db8::1]:443");
  });
});

// #350 F13 — isDialableSocketAddr must reject anything the Rust dialer's
// SocketAddr parse rejects: hostnames and invalid IP literals, not just
// bad separators/ports. This corpus is mirrored by a Rust parity test in
// crates/iroh-http-core/src/endpoint/bind.rs so both sides agree.
describe("isDialableSocketAddr strict IP-literal validation (#350 F13)", () => {
  it.each([
    "1.2.3.4:443",
    "192.168.50.227:59234",
    "[2001:db8::1]:443",
    "[::1]:8080",
    "[::ffff:192.168.1.1]:443",
    "0.0.0.0:1",
    "255.255.255.255:65535",
  ])("accepts the dialable literal %j", (good) => {
    expect(isDialableSocketAddr(good)).toBe(true);
  });

  it.each([
    "example.com:443", // hostname, not an IP literal
    "999.999.999.999:443", // octets out of range
    "[not-ip]:443", // bracketed non-IPv6
    "1.2.3:443", // too few octets
    "1.2.3.4.5:443", // too many octets
    "01.2.3.4:443", // non-canonical leading zero
    "1.2.3.4", // no port
    "1.2.3.4:0", // port 0
    "2001:db8::1:443", // unbracketed IPv6 (ambiguous)
    "[2001:db8::1]:0", // port 0
    "[fe80::1", // unterminated bracket
    "1.2.3.4:70000", // port out of range
  ])("rejects the undialable value %j", (bad) => {
    expect(isDialableSocketAddr(bad)).toBe(false);
  });
});
