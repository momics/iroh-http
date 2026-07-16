/**
 * iroh-http interop result collector — a tiny iroh-http server.
 *
 * Dogfoods the `iroh-http-node` package so device runs can POST their JSON
 * report to a single, stable node ID instead of copying files around by hand.
 * Each node (desktop / iOS / Android) submits `httpi://<collectorId>/results`
 * and the report lands on disk under `reports/`, ready to inspect.
 *
 * Routes (see collector.mjs):
 *   POST /results   — accept one report (`iroh-http-interop/2`) or a batch
 *                     (`iroh-http-interop/2-batch`); one JSON file per report.
 *   GET  /results   — JSON index of collected report files.
 *   GET  /health    — liveness probe (`200 ok`).
 *
 * Identity is persisted (base32 secret in `.identity.b32`, gitignored) so the
 * collector keeps the SAME node ID across restarts — hand it to the harness
 * once and forget it. Reachable by node ID alone via default n0 DNS discovery;
 * a small POST falls back to the relay when a direct path isn't available.
 *
 * Usage:
 *   node tests/interop/reporter/server.mjs [--reports-dir <dir>] [--print-id]
 *
 * Env:
 *   IROH_INTEROP_REPORTS_DIR   override the reports output directory
 */

import { readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createNode, SecretKey } from "../../../packages/iroh-http-node/lib.js";
import { createHandler } from "./collector.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));

const args = process.argv.slice(2);
function argValue(flag) {
  const i = args.indexOf(flag);
  return i >= 0 ? args[i + 1] : null;
}
const printIdOnly = args.includes("--print-id");
const reportsDir = argValue("--reports-dir") ??
  process.env.IROH_INTEROP_REPORTS_DIR ?? join(HERE, "reports");
const identityPath = join(HERE, ".identity.b32");

/** Load a persisted identity, or generate + persist a fresh one. */
async function loadOrCreateKey() {
  if (existsSync(identityPath)) {
    const b32 = (await readFile(identityPath, "utf-8")).trim();
    if (b32) return SecretKey.fromString(b32);
  }
  const key = SecretKey.generate();
  await writeFile(identityPath, key.toBase32Secret(), { mode: 0o600 });
  return key;
}

const key = await loadOrCreateKey();
const node = await createNode({ key });
const nodeId = node.publicKey.toString();

if (printIdOnly) {
  console.log(nodeId);
  await node.close();
  process.exit(0);
}

node.serve(
  {},
  createHandler({ reportsDir, log: (m) => console.log(`[collector] ${m}`) }),
);

const info = await node.discoveryInfo().catch(() => null);
const addrs = info?.directAddresses?.length
  ? info.directAddresses.join(", ")
  : (info?.directAddress ?? "(none yet)");
const rule = "\u2500".repeat(46);

console.log("");
console.log("  iroh-http interop collector is up.");
console.log(`  ${rule}`);
console.log(`  node id : ${nodeId}`);
console.log(`  addrs   : ${addrs}`);
console.log(`  relay   : ${info?.relayUrl ?? "(pending)"}`);
console.log(`  reports : ${reportsDir}`);
console.log(`  ${rule}`);
console.log(
  "  Paste the node id into each peer's Test tab \u2192 Submit results.",
);
console.log("");

process.on("SIGINT", () => {
  console.log("\n[collector] shutting down");
  node.close().finally(() => process.exit(0));
});
