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
 *
 * Case execution is delegated to the shared, transport-agnostic `runCases`
 * harness so this in-process runner and the on-device Test tab stay in lockstep.
 */

import { createNode } from "@momics/iroh-http-tauri";
import { invoke } from "@tauri-apps/api/core";
// @ts-ignore — .mjs from shared test suite (pure WHATWG, no Node/Deno deps)
import { handleRequest } from "./handler.mjs";
// @ts-ignore — .mjs from shared test suite
import { runCases } from "./harness.mjs";
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

interface CaseResult {
  id: string;
  description?: string;
  ok: boolean;
  status: number | null;
  latencyMs: number;
  error: string | null;
}

// ── Main ────────────────────────────────────────────────────────────────────
async function run() {
  // deno-lint-ignore no-explicit-any
  const allCases = (cases as any[]).filter((c) => c.id);

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

  const startTime = performance.now();
  const failedCases: string[] = [];

  const { summary } = await runCases({
    // deno-lint-ignore no-explicit-any
    fetch: (url: string, init: any) => client.fetch(url, init),
    serverId: serverAddr,
    cases: filtered,
    timeout,
    bail,
    onCaseResult: (r: CaseResult) => {
      const label = `[${r.id}] ${r.description ?? ""}`;
      if (r.ok) {
        if (verbose) log(`  ✓ ${label} (${r.latencyMs}ms)`, "pass");
      } else {
        failedCases.push(r.id);
        log(`  ✗ ${label}`, "fail");
        if (r.error) log(`      ${r.error}`, "fail-detail");
      }
    },
  });

  // ── Summary ─────────────────────────────────────────────────────────────
  const elapsed = ((performance.now() - startTime) / 1000).toFixed(2);
  log("");
  log("─".repeat(60));
  log(
    `Results: ${summary.passed} passed, ${summary.failed} failed (${elapsed}s)`,
    summary.failed > 0 ? "fail" : "pass",
  );

  if (failedCases.length > 0) {
    log("\nFailed cases:");
    failedCases.forEach((id) => log(`  - ${id}`, "fail"));
  }

  setStatus(
    summary.failed > 0
      ? `✗ ${summary.failed} failed, ${summary.passed} passed (${elapsed}s)`
      : `✓ All ${summary.passed} passed (${elapsed}s)`,
  );
  statusEl.className = summary.failed > 0 ? "status-fail" : "status-pass";

  // Cleanup
  try {
    // deno-lint-ignore no-explicit-any
    (server as any).shutdown?.();
    // deno-lint-ignore no-explicit-any
    (client as any).shutdown?.();
  } catch {
    // ignore
  }

  // CI mode: exit the process with the appropriate code
  if (ciMode) {
    try {
      await invoke("exit_with_code", { code: summary.failed > 0 ? 1 : 0 });
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
