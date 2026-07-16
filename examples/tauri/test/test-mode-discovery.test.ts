import assert from "node:assert/strict";
import test from "node:test";

import { testModeAdvertisement } from "../src/test-mode-discovery.ts";

test("testing mode advertises the endpoint's already-bound IPv4 QUIC port", () => {
  assert.deepEqual(
    testModeAdvertisement({
      directAddress: "192.168.1.20:43127",
      directAddresses: [
        "192.168.1.20:43127",
        "10.0.0.8:43127",
      ],
    }),
    {
      port: 43127,
      addressTxt: "192.168.1.20:43127,10.0.0.8:43127",
      usesBoundPort: true,
    },
  );
});

test("testing mode extracts an already-bound port from bracketed IPv6", () => {
  assert.deepEqual(
    testModeAdvertisement({
      directAddress: "[2001:db8::20]:55443",
      directAddresses: ["[2001:db8::20]:55443"],
    }),
    {
      port: 55443,
      addressTxt: "[2001:db8::20]:55443",
      usesBoundPort: true,
    },
  );
});

test("relay-only testing mode retains the explicit placeholder fallback", () => {
  assert.deepEqual(testModeAdvertisement(null), {
    port: 1,
    usesBoundPort: false,
  });
});
