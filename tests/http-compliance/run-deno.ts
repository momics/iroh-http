/**
 * iroh-http compliance test runner — Deno
 *
 * Creates two iroh-http nodes in the same process:
 *   1. Server node  — runs the compliance handler
 *   2. Client node  — sends requests defined in cases.json
 *
 * Usage:
 *   deno run -A tests/run-deno.ts [--filter <pattern>] [--bail]
 *
 * Flags:
 *   --filter <pattern>   Run only test IDs matching this substring
 *   --bail               Stop on first failure
 *   --verbose            Show passing test details too
 *   --timeout <ms>       Per-request timeout in ms (default 30000)
 */

import { createNode } from "../../packages/iroh-http-deno/mod.ts";
import { handleRequest } from "./handler.mjs";
import { assertResponse } from "./assertions.mjs";

// ── CLI args ────────────────────────────────────────────────────────────────
const args = Deno.args;
const filterPattern = getArg(args, "--filter");
const bail = args.includes("--bail");
const verbose = args.includes("--verbose");
const timeout = parseInt(getArg(args, "--timeout") ?? "30000", 10);

function getArg(args: string[], flag: string): string | null {
  const idx = args.indexOf(flag);
  if (idx === -1 || idx + 1 >= args.length) return null;
  return args[idx + 1];
}

// ── Load cases ──────────────────────────────────────────────────────────────
const casesRaw = await Deno.readTextFile(
  new URL("./cases.json", import.meta.url),
);
const allCases = JSON.parse(casesRaw).filter((c: any) => c.id); // skip comment entries

let cases = allCases;
if (filterPattern) {
  cases = cases.filter((c: any) => c.id.includes(filterPattern));
  console.log(
    `Filter: "${filterPattern}" → ${cases.length}/${allCases.length} cases\n`,
  );
}

// ── Create nodes ────────────────────────────────────────────────────────────
console.log("Creating server node…");
const server = await createNode();
console.log(`  Server public key: ${server.publicKey.toString()}`);

console.log("Creating client node…");
const client = await createNode();
console.log(`  Client public key: ${client.publicKey.toString()}\n`);

// ── Start server ────────────────────────────────────────────────────────────
server.serve({}, handleRequest);
console.log("Server is listening.\n");

// ── Helpers ─────────────────────────────────────────────────────────────────
const serverAddr = server.publicKey.toString();

function buildBody(bodySpec: any): BodyInit | undefined {
  if (bodySpec === null || bodySpec === undefined) return undefined;
  if (typeof bodySpec === "string") return bodySpec;
  if (typeof bodySpec === "object" && bodySpec.fill) {
    return new Uint8Array(bodySpec.fill);
  }
  return undefined;
}

function buildHeaders(headersSpec: any): Record<string, string> {
  const h: Record<string, string> = {};
  if (!headersSpec) return h;
  for (const [k, v] of Object.entries(headersSpec)) {
    if (typeof v === "object" && (v as any).fill) {
      h[k] = "x".repeat((v as any).fill);
    } else {
      h[k] = v as string;
    }
  }
  return h;
}

async function runSingleRequest(req: any) {
  const body = buildBody(req.body);
  const headers = buildHeaders(req.headers);
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeout);

  try {
    const res = await client.fetch(`httpi://${serverAddr}${req.path}`, {
      method: req.method,
      headers,
      body,
      signal: controller.signal,
    });
    const buf = await res.arrayBuffer();
    const bodyText = new TextDecoder().decode(buf);
    const bodyLength = buf.byteLength;
    return { res, bodyText, bodyLength };
  } finally {
    clearTimeout(timer);
  }
}

// ── Run tests ───────────────────────────────────────────────────────────────
let passed = 0;
let failed = 0;
let skipped = 0;
const failedCases: string[] = [];

const startTime = Date.now();

for (const tc of cases) {
  if (!tc.id) continue; // skip comment entries
  if (tc.skip) {
    skipped++;
    console.log(`  skip  [${tc.id}]: ${tc.skip}`);
    continue;
  }
  // Sequential multi-request test
  if (tc.requests) {
    const label = `[${tc.id}] ${tc.description ?? ""}`;
    let allPassed = true;
    for (let i = 0; i < tc.requests.length; i++) {
      const sub = tc.requests[i];
      try {
        const { res, bodyText, bodyLength } = await runSingleRequest(sub);
        const result = assertResponse(
          { response: sub.expect },
          res,
          bodyText,
          bodyLength,
        );
        if (!result.pass) {
          allPassed = false;
          console.log(`  ✗ ${label} [step ${i}]`);
          result.failures.forEach((f: string) => console.log(`      ${f}`));
        }
      } catch (err: any) {
        allPassed = false;
        console.log(`  ✗ ${label} [step ${i}] — ${err.message}`);
      }
    }
    if (allPassed) {
      passed++;
      if (verbose) console.log(`  ✓ ${label}`);
    } else {
      failed++;
      failedCases.push(tc.id);
      if (bail) break;
    }
    continue;
  }

  const concurrency = tc.concurrent ?? 1;
  const repeatCount = tc.repeat ?? 1;
  const totalRuns = Math.max(concurrency, repeatCount);
  const sequential = tc.sequential ?? 0;

  const label = `[${tc.id}] ${tc.description ?? ""}`;

  try {
    if (sequential > 0) {
      let allPassed = true;
      for (let i = 0; i < sequential; i++) {
        const { res, bodyText, bodyLength } = await runSingleRequest(
          tc.request,
        );
        const result = assertResponse(tc, res, bodyText, bodyLength);
        if (!result.pass) {
          allPassed = false;
          console.log(`  ✗ ${label} [iteration ${i}]`);
          result.failures.forEach((f: string) => console.log(`      ${f}`));
          break;
        }
      }
      if (allPassed) {
        passed++;
        if (verbose) console.log(`  ✓ ${label} (${sequential}x sequential)`);
      } else {
        failed++;
        failedCases.push(tc.id);
        if (bail) break;
      }
      continue;
    }

    if (concurrency > 1 || repeatCount > 1) {
      const promises = Array.from(
        { length: totalRuns },
        () => runSingleRequest(tc.request),
      );
      const results = await Promise.all(promises);

      let allPassed = true;
      const bodies: string[] = [];
      for (let i = 0; i < results.length; i++) {
        const { res, bodyText, bodyLength } = results[i];
        bodies.push(bodyText);
        const result = assertResponse(tc, res, bodyText, bodyLength);
        if (!result.pass) {
          allPassed = false;
          console.log(`  ✗ ${label} [instance ${i}]`);
          result.failures.forEach((f: string) => console.log(`      ${f}`));
        }
      }

      if (tc.assertAllBodiesEqual && allPassed) {
        const unique = new Set(bodies);
        if (unique.size > 1) {
          allPassed = false;
          console.log(`  ✗ ${label} — bodies differ across runs`);
        }
      }

      if (allPassed) {
        passed++;
        if (verbose) console.log(`  ✓ ${label} (${totalRuns}x)`);
      } else {
        failed++;
        failedCases.push(tc.id);
        if (bail) break;
      }
      continue;
    }

    // Normal single request
    const { res, bodyText, bodyLength } = await runSingleRequest(tc.request);
    const result = assertResponse(tc, res, bodyText, bodyLength);

    if (result.pass) {
      passed++;
      if (verbose) console.log(`  ✓ ${label}`);
    } else {
      failed++;
      failedCases.push(tc.id);
      console.log(`  ✗ ${label}`);
      result.failures.forEach((f: string) => console.log(`      ${f}`));
      if (bail) break;
    }
  } catch (err: any) {
    failed++;
    failedCases.push(tc.id);
    console.log(`  ✗ ${label} — ${err.message}`);
    if (bail) break;
  }
}

// ── Summary ─────────────────────────────────────────────────────────────────
const elapsed = ((Date.now() - startTime) / 1000).toFixed(2);
console.log("\n" + "─".repeat(60));
console.log(
  `Results: ${passed} passed, ${failed} failed, ${skipped} skipped (${elapsed}s)`,
);
if (failedCases.length > 0) {
  console.log(`\nFailed cases:`);
  failedCases.forEach((id) => console.log(`  - ${id}`));
}
console.log("");

// Clean up
try {
  (server as any).shutdown?.();
  (client as any).shutdown?.();
} catch {
  // ignore cleanup errors
}

Deno.exit(failed > 0 ? 1 : 0);
