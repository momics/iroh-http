/**
 * iroh-http test runner — Tauri (webview)
 *
 * Thin runner that imports createNode + key classes from the Tauri adapter,
 * binds them to a simple webview-compatible test harness, and executes all
 * shared suites sequentially.
 *
 * Must be bundled and loaded inside a Tauri webview.  In CI mode (?ci=true),
 * calls the `exit_with_code` Tauri command on completion.
 *
 * Usage (inside Tauri webview):
 *   <script type="module" src="./tauri.ts"></script>
 */

import { createNode, PublicKey, SecretKey } from "@momics/iroh-http-tauri";
import { invoke } from "@tauri-apps/api/core";

// @ts-ignore — .mjs shared suites (pure WHATWG, no Node/Deno deps)
import { lifecycleTests } from "../suites/lifecycle.mjs";
// @ts-ignore
import { errorTests } from "../suites/errors.mjs";
// @ts-ignore
import { stressTests } from "../suites/stress.mjs";
// @ts-ignore
import { eventTests } from "../suites/events.mjs";
// @ts-ignore
import { sessionTests } from "../suites/sessions.mjs";
// @ts-ignore
import { keyTests } from "../suites/keys.mjs";
// @ts-ignore
import { discoveryTests } from "../suites/discovery.mjs";

// ── Config ──────────────────────────────────────────────────────────────────

const params = new URLSearchParams(window.location.search);
const ciMode = params.get("ci") === "true";

// ── Sequential test runner ──────────────────────────────────────────────────

interface TestEntry { name: string; fn: () => Promise<void> }
const queue: TestEntry[] = [];
let passed = 0;
let failed = 0;
const failures: string[] = [];

/** Adapter from shared test API → sequential queue */
const ctx = {
  createNode,
  PublicKey,
  SecretKey,
  test: (name: string, fn: () => Promise<void>) => {
    queue.push({ name, fn });
  },
  assert: (v: unknown, msg?: string) => {
    if (!v) throw new Error(msg ?? `assertion failed: ${v}`);
  },
  assertEqual: (a: unknown, b: unknown, msg?: string) => {
    if (a !== b) throw new Error(msg ?? `expected ${JSON.stringify(b)}, got ${JSON.stringify(a)}`);
  },
  assertNotEqual: (a: unknown, b: unknown, msg?: string) => {
    if (a === b) throw new Error(msg ?? `expected values to differ, both are ${JSON.stringify(a)}`);
  },
  assertThrows: async (fn: () => Promise<unknown>) => {
    try {
      await fn();
      throw new Error("expected function to throw, but it did not");
    } catch (err) {
      if ((err as Error).message === "expected function to throw, but it did not") throw err;
      return err;
    }
  },
  assertResolves: async (promise: Promise<unknown>) => {
    try {
      return await promise;
    } catch (err) {
      throw new Error(`expected promise to resolve, but it rejected: ${(err as Error).message}`);
    }
  },
};

// ── Register all suites ─────────────────────────────────────────────────────

lifecycleTests(ctx);
errorTests(ctx);
stressTests(ctx);
eventTests(ctx);
sessionTests(ctx);
keyTests(ctx);
discoveryTests(ctx);

// ── Run ─────────────────────────────────────────────────────────────────────

async function run() {
  console.log(`Running ${queue.length} tests…`);

  for (const { name, fn } of queue) {
    try {
      await fn();
      passed++;
      console.log(`  ✔ ${name}`);
    } catch (err) {
      failed++;
      const msg = err instanceof Error ? err.message : String(err);
      console.error(`  ✗ ${name} — ${msg}`);
      failures.push(`${name}: ${msg}`);
    }
  }

  console.log(`\n${passed} passed, ${failed} failed out of ${queue.length}`);
  if (failures.length) {
    console.error("Failures:");
    for (const f of failures) console.error(`  - ${f}`);
  }

  // Publish the result on `window` so an external WebDriver harness (see
  // tests/tauri-app) can observe the outcome without relying on process exit.
  (globalThis as unknown as { __irohTestSummary?: unknown }).__irohTestSummary = {
    passed,
    failed,
    total: queue.length,
    failures,
  };

  if (ciMode) {
    try {
      await invoke("exit_with_code", { code: failed > 0 ? 1 : 0 });
    } catch {
      // exit_with_code may not be registered outside CI
    }
  }
}

run().catch(console.error);
