import { asIrohPeer } from "../src/discovery.ts";

function assertEquals(actual: unknown, expected: unknown): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(
      `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    );
  }
}

Deno.test("authoritative TXT addresses exclude the DNS-SD SRV carrier", () => {
  const peer = asIrohPeer({
    isActive: true,
    serviceType: "_iroh-http-test._udp.local.",
    instanceName: "test-peer",
    host: "test-peer.local.",
    port: 1,
    addrs: ["192.0.2.30:1"],
    txt: {
      pk: "test-peer-id",
      address: "192.0.2.30:4242",
    },
  });

  assertEquals(peer?.addrs, ["192.0.2.30:4242"]);
});

Deno.test("resolved addresses remain the fallback without address TXT", () => {
  const peer = asIrohPeer({
    isActive: true,
    serviceType: "_iroh-http._udp.local.",
    instanceName: "fallback-peer-id",
    host: "fallback.local.",
    port: 4242,
    addrs: ["192.0.2.31:4242"],
    txt: {},
  });

  assertEquals(peer?.addrs, ["192.0.2.31:4242"]);
});

Deno.test("relay reachability remains alongside authoritative TXT addresses", () => {
  const peer = asIrohPeer({
    isActive: true,
    serviceType: "_iroh-http-test._udp.local.",
    instanceName: "relay-peer-id",
    host: "relay-peer.local.",
    port: 1,
    addrs: ["192.0.2.32:1", "https://resolved-relay.example"],
    txt: {
      address: "192.0.2.32:4242",
      relay: "https://home-relay.example",
    },
  });

  assertEquals(peer?.addrs, [
    "192.0.2.32:4242",
    "https://home-relay.example",
    "https://resolved-relay.example",
  ]);
});
