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
import { asIrohPeer, type ServiceRecord } from "@momics/iroh-http-shared";

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
