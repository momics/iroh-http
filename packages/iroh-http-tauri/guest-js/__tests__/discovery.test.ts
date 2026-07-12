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
});
