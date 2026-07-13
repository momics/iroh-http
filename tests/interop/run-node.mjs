/**
 * iroh-http interop suite runner — Node.js (headless).
 *
 * Drives the shared `suite.mjs` against two in-process iroh-http nodes so the
 * `discovery` / `direct-dial` / `relay-fallback` / `http-compliance` /
 * `serve-stop` groups can be exercised without a device pair. Discovery/dial
 * tests that need real LAN mDNS or a remote peer will `skip` gracefully here;
 * the on-device Tauri Test tab is the full-coverage consumer.
 *
 * Usage:  node tests/interop/run-node.mjs [--filter <substr>]
 *
 * This is additive — it does NOT replace `tests/http-compliance/run-node.mjs`.
 */

import { readFile } from "node:fs/promises";
import { createNode } from "../../packages/iroh-http-node/lib.js";
import { handleRequest } from "../http-compliance/handler.mjs";
import { runCases } from "../http-compliance/harness.mjs";
import { buildSuite, runSuite } from "./suite.mjs";

const args = process.argv.slice(2);
const filterIdx = args.indexOf("--filter");
const filter = filterIdx >= 0 ? args[filterIdx + 1] : null;

const casesRaw = await readFile(
  new URL("../http-compliance/cases.json", import.meta.url),
  "utf-8",
);
const cases = JSON.parse(casesRaw).filter((c) => c && c.id && !c.skip);

const selfLoopbackCase = {
  id: "self-loopback",
  description: "self-request baseline (ADR-015)",
  request: { method: "GET", path: "/hello", headers: {}, body: null },
  response: { status: 200 },
};

console.log("Creating server (peer) node…");
const server = await createNode();
server.serve({}, handleRequest);
const serverId = server.publicKey.toString();
console.log(`  peer: ${serverId}`);

console.log("Creating client node…");
const client = await createNode();
// Serve the compliance handler on the client too, so the ADR-015 self-loopback
// baseline can dispatch to its own node id — mirrors the on-device testing mode,
// which always serves while the suite runs. (With a local service bound, the
// standalone serve→stop lifecycle test skips, as designed.)
client.serve({}, handleRequest);
console.log(`  self: ${client.publicKey.toString()}\n`);

// Resolve the peer's dialable direct addr so direct-dial has something to use.
const peerInfo = await client.discoveryInfo().catch(() => null);
const serverDiscovery = await server.discoveryInfo().catch(() => null);
const peerAddrs = serverDiscovery?.directAddress
  ? [serverDiscovery.directAddress]
  : [];

let groups = buildSuite({
  node: client,
  self: { nodeId: client.publicKey.toString(), platform: "node" },
  peer: { nodeId: serverId, platform: "node", addrs: peerAddrs },
  fetch: (url, init) => client.fetch(url, init),
  cases,
  selfLoopbackCase,
  runCases,
  handler: handleRequest,
  helpers: { TXT_KEY_ADDRESS: "address", TXT_KEY_RELAY: "relay" },
  isServing: true,
  // Headless Node runner has no guaranteed mDNS stack — discovery cases that
  // observe nothing skip cleanly rather than failing.
  mdnsCapable: false,
});

if (filter) {
  groups = groups
    .map((g) => ({ ...g, tests: g.tests.filter((t) => t.id.includes(filter)) }))
    .filter((g) => g.tests.length);
}

const report = await runSuite(groups, {
  onResult: ({ phase, result }) => {
    if (phase !== "result") return;
    const icon = result.outcome === "pass"
      ? "✓"
      : result.outcome === "skip"
      ? "○"
      : "✗";
    console.log(
      `  ${icon} [${result.group}] ${result.id} — ${result.detail ?? ""}`,
    );
  },
});

console.log("\n[iroh-http-interop]", JSON.stringify(report.summary));
void peerInfo;
process.exit(report.summary.fail > 0 ? 1 : 0);
