import assert from "node:assert/strict";
import test from "node:test";

import { getPeer, upsertPeer } from "../src/peer-registry.ts";

test("a new discovery observation replaces stale discovered addresses", () => {
  const nodeId = "discovery-address-replacement";

  upsertPeer({
    nodeId,
    source: "discovery",
    addrs: ["192.0.2.10:4000"],
  });
  upsertPeer({
    nodeId,
    source: "discovery",
    addrs: ["192.0.2.10:5000"],
  });

  assert.deepEqual(getPeer(nodeId)?.addrs, ["192.0.2.10:5000"]);
});

test("an empty discovery snapshot retires its previously known addresses", () => {
  const nodeId = "discovery-address-retirement";

  upsertPeer({
    nodeId,
    source: "generic",
    addrs: ["192.0.2.11:4000"],
  });
  upsertPeer({ nodeId, source: "generic", addrs: [] });

  assert.deepEqual(getPeer(nodeId)?.addrs, []);
});

test("replacing discovery addresses preserves manually supplied reachability", () => {
  const nodeId = "manual-address-preservation";

  upsertPeer({
    nodeId,
    source: "manual",
    addrs: ["https://relay.example"],
  });
  upsertPeer({
    nodeId,
    source: "test",
    addrs: ["192.0.2.20:4000"],
  });
  upsertPeer({
    nodeId,
    source: "test",
    addrs: ["192.0.2.20:5000"],
  });

  assert.deepEqual(getPeer(nodeId)?.addrs, [
    "https://relay.example",
    "192.0.2.20:5000",
  ]);
});

test("session updates accumulate their own addresses", () => {
  const nodeId = "session-address-accumulation";

  upsertPeer({ nodeId, source: "session", addrs: ["192.0.2.40:4000"] });
  upsertPeer({ nodeId, source: "session", addrs: ["192.0.2.40:5000"] });

  assert.deepEqual(getPeer(nodeId)?.addrs, [
    "192.0.2.40:4000",
    "192.0.2.40:5000",
  ]);
});
