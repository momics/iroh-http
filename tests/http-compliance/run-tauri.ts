/**
 * iroh-http compliance test runner — Tauri (webview)
 *
 * Runs the full compliance suite inside the Tauri webview.
 * Creates two iroh-http nodes in-process (server + client) and
 * renders results to the DOM in real time.
 *
 * Configuration via URL search params:
 *   ?filter=<pattern>   Run only test IDs matching this substring
 *   ?bail=true          Stop on first failure
 *   ?verbose=true       Show passing test details
 *
 * CI mode:
 *   When ?ci=true is set, the runner calls the `exit_with_code` Tauri command
 *   on completion, causing the process to exit with 0 (all passed) or 1 (failures).
 *
 * Also logs all output to the browser console (open devtools with Cmd+Option+I).
 */

import { createNode } from "@momics/iroh-http-tauri";
import { invoke } from "@tauri-apps/api/core";
// @ts-ignore — .mjs from shared test suite (pure WHATWG, no Node/Deno deps)
import { handleRequest } from "./handler.mjs";
// @ts-ignore — .mjs from shared test suite
import { assertResponse } from "./assertions.mjs";
import cases from "./cases.json";

// ── Config from URL params ──────────────────────────────────────────────────
const params = new URLSearchParams(window.location.search);
const filterPattern = params.get("filter");
const bail = params.get("bail") === "true";
const verbose = params.get("verbose") === "true";
const ciMode = params.get("ci") === "true";
const timeout = parseInt(params.get("timeout") ?? "30000", 10);

// ── DOM setup ───────────────────────────────────────────────────────────────
const container = document.getElementById("test-output")!;
const statusEl = document.getElementById("test-status")!;
const filterInput = document.getElementById("filter-input") as HTMLInputElement;
const rerunBtn = document.getElementById("rerun-btn")!;

filterInput.value = filterPattern ?? "";
rerunBtn.addEventListener("click", () => {
  const f = filterInput.value.trim();
  const url = new URL(window.location.href);
  if (f) url.searchParams.set("filter", f);
  else url.searchParams.delete("filter");
  url.searchParams.set("verbose", "true");
  window.location.href = url.toString();
});

function log(msg: string, className = "") {
  const line = document.createElement("div");
  line.className = `log-line ${className}`;
  line.textContent = msg;
  container.appendChild(line);
  container.scrollTop = container.scrollHeight;
  console.log(msg);
}

function setStatus(text: string) {
  statusEl.textContent = text;
}

// ── Helpers ─────────────────────────────────────────────────────────────────
interface TestCase {
  id: string;
  description?: string;
  request?: {
    method: string;
    path: string;
    headers?: Record<string, string | { fill: number }>;
    body?: string | { fill: number } | null;
  };
  response?: {
    status?: number;
    bodyExact?: string;
    bodyNotEmpty?: boolean;
    bodyNot?: string;
    bodyContains?: string;
    bodyMatchesRegex?: string;
    bodyLengthExact?: number;
    bodyMinLength?: number;
    headers?: Record<string, string>;
  };
  concurrent?: number;
  sequential?: number;
  repeat?: number;
  assertAllBodiesEqual?: boolean;
  requests?: Array<{
    method: string;
    path: string;
    headers?: Record<string, string | { fill: number }>;
    body?: string | { fill: number } | null;
    expect: {
      status?: number;
      bodyExact?: string;
      bodyNotEmpty?: boolean;
      bodyNot?: string;
      bodyContains?: string;
      bodyMatchesRegex?: string;
      bodyLengthExact?: number;
      bodyMinLength?: number;
      headers?: Record<string, string>;
    };
  }>;
}

function buildBody(
  bodySpec: string | { fill: number } | null | undefined,
): string | Uint8Array | undefined {
  if (bodySpec === null || bodySpec === undefined) return undefined;
  if (typeof bodySpec === "string") return bodySpec;
  if (typeof bodySpec === "object" && "fill" in bodySpec) {
    return new Uint8Array(bodySpec.fill);
  }
  return undefined;
}

function buildHeaders(
  headersSpec?: Record<string, string | { fill: number }>,
): Record<string, string> {
  const h: Record<string, string> = {};
  if (!headersSpec) return h;
  for (const [k, v] of Object.entries(headersSpec)) {
    if (typeof v === "object" && v !== null && "fill" in v) {
      h[k] = "x".repeat(v.fill);
    } else {
      h[k] = v as string;
    }
  }
  return h;
}

// ── Main ────────────────────────────────────────────────────────────────────
async function run() {
  const allCases: TestCase[] = (cases as TestCase[]).filter((c) => c.id);

  let filtered = allCases;
  if (filterPattern) {
    filtered = allCases.filter((c) => c.id.includes(filterPattern));
    log(
      `Filter: "${filterPattern}" → ${filtered.length}/${allCases.length} cases`,
      "info",
    );
  }

  setStatus("Creating nodes…");
  log("Creating server node…");
  const server = await createNode();
  log(`  Server public key: ${server.publicKey.toString()}`);

  log("Creating client node…");
  const client = await createNode();
  log(`  Client public key: ${client.publicKey.toString()}`);

  const serverAddr = server.publicKey.toString();

  // Start compliance server
  server.serve({}, handleRequest);
  log("Server is listening.\n");

  // Helper for single request
  async function runSingleRequest(req: TestCase["request"]) {
    const body = buildBody(req!.body);
    const headers = buildHeaders(req!.headers);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeout);

    try {
      const res = await client.fetch(`httpi://${serverAddr}${req!.path}`, {
        method: req!.method,
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

  // Run tests
  let passed = 0;
  let failed = 0;
  const failedCases: string[] = [];
  const startTime = performance.now();

  for (let i = 0; i < filtered.length; i++) {
    const tc = filtered[i];
    setStatus(`Running ${i + 1}/${filtered.length}: ${tc.id}`);
    const label = `[${tc.id}] ${tc.description ?? ""}`;

    try {
      // ── Multi-step requests ───────────────────────────────────────────
      if (tc.requests) {
        let allPassed = true;
        for (let j = 0; j < tc.requests.length; j++) {
          const sub = tc.requests[j];
          try {
            const { res, bodyText, bodyLength } = await runSingleRequest(
              sub as TestCase["request"],
            );
            const result = assertResponse(
              { response: sub.expect },
              res,
              bodyText,
              bodyLength,
            );
            if (!result.pass) {
              allPassed = false;
              log(`  ✗ ${label} [step ${j}]`, "fail");
              result.failures.forEach((f: string) =>
                log(`      ${f}`, "fail-detail")
              );
            }
          } catch (err: unknown) {
            allPassed = false;
            log(`  ✗ ${label} [step ${j}] — ${(err as Error).message}`, "fail");
          }
        }
        if (allPassed) {
          passed++;
          if (verbose) log(`  ✓ ${label}`, "pass");
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

      // ── Sequential requests ───────────────────────────────────────────
      if (sequential > 0) {
        let allPassed = true;
        for (let j = 0; j < sequential; j++) {
          const { res, bodyText, bodyLength } = await runSingleRequest(
            tc.request,
          );
          const result = assertResponse(tc, res, bodyText, bodyLength);
          if (!result.pass) {
            allPassed = false;
            log(`  ✗ ${label} [iteration ${j}]`, "fail");
            result.failures.forEach((f: string) =>
              log(`      ${f}`, "fail-detail")
            );
            break;
          }
        }
        if (allPassed) {
          passed++;
          if (verbose) log(`  ✓ ${label} (${sequential}x sequential)`, "pass");
        } else {
          failed++;
          failedCases.push(tc.id);
          if (bail) break;
        }
        continue;
      }

      // ── Concurrent / repeated requests ────────────────────────────────
      if (concurrency > 1 || repeatCount > 1) {
        const promises = Array.from(
          { length: totalRuns },
          () => runSingleRequest(tc.request),
        );
        const results = await Promise.all(promises);
        let allPassed = true;
        const bodies: string[] = [];

        for (let j = 0; j < results.length; j++) {
          const { res, bodyText, bodyLength } = results[j];
          bodies.push(bodyText);
          const result = assertResponse(tc, res, bodyText, bodyLength);
          if (!result.pass) {
            allPassed = false;
            log(`  ✗ ${label} [instance ${j}]`, "fail");
            result.failures.forEach((f: string) =>
              log(`      ${f}`, "fail-detail")
            );
          }
        }

        if (tc.assertAllBodiesEqual && allPassed) {
          const unique = new Set(bodies);
          if (unique.size > 1) {
            allPassed = false;
            log(`  ✗ ${label} — bodies differ across runs`, "fail");
          }
        }

        if (allPassed) {
          passed++;
          if (verbose) log(`  ✓ ${label} (${totalRuns}x)`, "pass");
        } else {
          failed++;
          failedCases.push(tc.id);
          if (bail) break;
        }
        continue;
      }

      // ── Single request ────────────────────────────────────────────────
      const { res, bodyText, bodyLength } = await runSingleRequest(tc.request);
      const result = assertResponse(tc, res, bodyText, bodyLength);

      if (result.pass) {
        passed++;
        if (verbose) log(`  ✓ ${label}`, "pass");
      } else {
        failed++;
        failedCases.push(tc.id);
        log(`  ✗ ${label}`, "fail");
        result.failures.forEach((f: string) =>
          log(`      ${f}`, "fail-detail")
        );
        if (bail) break;
      }
    } catch (err: unknown) {
      failed++;
      failedCases.push(tc.id);
      log(`  ✗ ${label} — ${(err as Error).message}`, "fail");
      if (bail) break;
    }

    // Yield to keep UI responsive
    if (i % 10 === 0) await new Promise((r) => setTimeout(r, 0));
  }

  // ── Summary ─────────────────────────────────────────────────────────────
  const elapsed = ((performance.now() - startTime) / 1000).toFixed(2);
  log("");
  log("─".repeat(60));
  log(
    `Results: ${passed} passed, ${failed} failed (${elapsed}s)`,
    failed > 0 ? "fail" : "pass",
  );

  if (failedCases.length > 0) {
    log("\nFailed cases:");
    failedCases.forEach((id) => log(`  - ${id}`, "fail"));
  }

  setStatus(
    failed > 0
      ? `✗ ${failed} failed, ${passed} passed (${elapsed}s)`
      : `✓ All ${passed} passed (${elapsed}s)`,
  );
  statusEl.className = failed > 0 ? "status-fail" : "status-pass";

  // Cleanup
  try {
    (server as any).shutdown?.();
    (client as any).shutdown?.();
  } catch {
    // ignore
  }

  // CI mode: exit the process with the appropriate code
  if (ciMode) {
    try {
      await invoke("exit_with_code", { code: failed > 0 ? 1 : 0 });
    } catch (err) {
      console.error("exit_with_code invoke failed:", err);
    }
  }
}

run().catch(async (err) => {
  log(`Fatal: ${err.message}`, "fail");
  log(err.stack ?? "", "fail-detail");
  setStatus(`Fatal error: ${err.message}`);
  statusEl.className = "status-fail";
  console.error(err);
  if (ciMode) {
    try {
      await invoke("exit_with_code", { code: 1 });
    } catch {
      // ignore
    }
  }
});
