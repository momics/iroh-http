/**
 * iroh-http interop suite runner — Deno (headless).
 *
 * The Deno counterpart to `run-node.mjs`: drives the shared `suite.mjs`
 * against two in-process iroh-http nodes so the `discovery` / `direct-dial` /
 * `relay-fallback` / `http-compliance` / `serve-stop` groups run headlessly on
 * Deno as well. Together with the Node runner this satisfies issue #340's
 * requirement that Node **and** Deno execute the same structured suite.
 *
 * Discovery/dial tests that need real LAN mDNS or a remote peer `skip`
 * gracefully here (`mdnsCapable: false`); the on-device Tauri Test tab is the
 * full-coverage consumer.
 *
 * Run (after `deno task build` in packages/iroh-http-deno):
 *   deno run -A tests/interop/run-deno.ts [--filter <substr>]
 */

import { createNode } from "../../packages/iroh-http-deno/mod.ts";
// @ts-ignore — plain ESM from the shared interop harness
import { handleRequest } from "../http-compliance/handler.mjs";
// @ts-ignore — plain ESM from the shared interop harness
import { runCases } from "../http-compliance/harness.mjs";
// @ts-ignore — plain ESM from the shared interop harness
import { buildSuite, runSuite } from "./suite.mjs";

const args = Deno.args;
const filterIdx = args.indexOf("--filter");
const filter = filterIdx >= 0 ? args[filterIdx + 1] : null;

const casesUrl = new URL(
  "../http-compliance/cases.json",
  import.meta.url,
);
// deno-lint-ignore no-explicit-any
const cases: any[] = JSON.parse(await Deno.readTextFile(casesUrl)).filter(
  // deno-lint-ignore no-explicit-any
  (c: any) => c && c.id && !c.skip,
);

const selfLoopbackCase = {
  id: "self-loopback",
  description: "self-request baseline (ADR-015)",
  request: { method: "GET", path: "/hello", headers: {}, body: null },
  response: { status: 200 },
};

async function discoveryInfoWithRelay(
  node: Awaited<ReturnType<typeof createNode>>,
  timeoutMs = 10_000,
) {
  const deadline = Date.now() + timeoutMs;
  let info = null;
  do {
    info = await node.discoveryInfo().catch(() => null);
    if (info?.relayUrl) return info;
    await new Promise((resolve) => setTimeout(resolve, 200));
  } while (Date.now() < deadline);
  return info;
}

console.log("Creating server (peer) node…");
const server = await createNode();
server.serve({}, handleRequest);
const serverId = server.publicKey.toString();
console.log(`  peer: ${serverId}`);

console.log("Creating client node…");
const client = await createNode();
// Serve the compliance handler on the client too, so the ADR-015 self-loopback
// baseline can dispatch to its own node id — mirrors the on-device testing
// mode, which always serves while the suite runs. The serve→stop case uses the
// isolated-node factory below, so the primary client can remain bound.
client.serve({}, handleRequest);
console.log(`  self: ${client.publicKey.toString()}\n`);

// Resolve the peer's dialable direct addr so direct-dial has something to use.
const serverDiscovery = await discoveryInfoWithRelay(server);
const peerDirectAddrs = serverDiscovery?.directAddresses?.length
  ? serverDiscovery.directAddresses
  : (serverDiscovery?.directAddress ? [serverDiscovery.directAddress] : []);
const peerAddrs = [
  ...peerDirectAddrs,
  ...(serverDiscovery?.relayUrl ? [serverDiscovery.relayUrl] : []),
];

let groups = buildSuite({
  node: client,
  self: { nodeId: client.publicKey.toString(), platform: "deno" },
  peer: { nodeId: serverId, platform: "deno", addrs: peerAddrs },
  // deno-lint-ignore no-explicit-any
  fetch: (url: string, init: any) => client.fetch(url, init),
  cases,
  selfLoopbackCase,
  runCases,
  handler: handleRequest,
  createIsolatedNode: (
    request: { nodeOptions?: Parameters<typeof createNode>[0] },
  ) => createNode(request.nodeOptions),
  helpers: { TXT_KEY_ADDRESS: "address", TXT_KEY_RELAY: "relay" },
  isServing: true,
  // Headless Deno runner has no guaranteed mDNS stack — discovery cases that
  // observe nothing skip cleanly rather than failing.
  mdnsCapable: false,
});

if (filter) {
  groups = groups
    // deno-lint-ignore no-explicit-any
    .map((g: any) => ({
      ...g,
      // deno-lint-ignore no-explicit-any
      tests: g.tests.filter((t: any) => t.id.includes(filter)),
    }))
    // deno-lint-ignore no-explicit-any
    .filter((g: any) => g.tests.length);
}

const report = await runSuite(groups, {
  // deno-lint-ignore no-explicit-any
  onResult: ({ phase, result }: any) => {
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
Deno.exit(report.summary.fail > 0 ? 1 : 0);
