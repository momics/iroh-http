/**
 * Pure unit tests for the interop suite runner (`suite.mjs`).
 *
 * These exercise `runSuite`'s outcome accounting, transport rollup (F27), and
 * per-case deadline (F20) with synthetic in-memory tests — no FFI, no network,
 * no mDNS — so they run deterministically in plain Node CI.
 *
 * Run:  node --test tests/interop/suite.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { runSuite } from "./suite.mjs";

/** Build a one-group suite from an array of `{ id, run }` tests. */
function group(id, tests) {
  return [{ id, name: id, tests }];
}

const silent = () => {};

test("runSuite counts pass / fail / skip independently", async () => {
  const groups = group("g", [
    { id: "p", name: "p", run: async () => ({ ok: true, latencyMs: 1 }) },
    { id: "f", name: "f", run: async () => ({ ok: false, latencyMs: 1 }) },
    {
      id: "s",
      name: "s",
      run: async () => ({ ok: false, skip: true, latencyMs: 1 }),
    },
  ]);
  const report = await runSuite(groups, { log: silent });
  assert.equal(report.summary.total, 3);
  assert.equal(report.summary.pass, 1);
  assert.equal(report.summary.fail, 1);
  assert.equal(report.summary.skip, 1);
});

test("skip wins over ok — a skipped case is never counted as pass", async () => {
  const groups = group("g", [
    {
      id: "skip-but-ok",
      name: "skip-but-ok",
      // ok:true is irrelevant once skip is set.
      run: async () => ({ ok: true, skip: true, latencyMs: 1 }),
    },
  ]);
  const report = await runSuite(groups, { log: silent });
  assert.equal(report.summary.pass, 0);
  assert.equal(report.summary.skip, 1);
});

test("transport rollup counts only transport-bearing cases (F27)", async () => {
  const groups = group("g", [
    {
      id: "d",
      name: "d",
      run: async () => ({ ok: true, transport: "direct", latencyMs: 1 }),
    },
    {
      id: "r",
      name: "r",
      run: async () => ({ ok: true, transport: "relay", latencyMs: 1 }),
    },
    {
      id: "x",
      name: "x",
      run: async () => ({ ok: true, transport: "unknown", latencyMs: 1 }),
    },
    // No transport field → must not inflate any bucket.
    { id: "none", name: "none", run: async () => ({ ok: true, latencyMs: 1 }) },
  ]);
  const report = await runSuite(groups, { log: silent });
  assert.deepEqual(report.summary.transport, {
    direct: 1,
    relay: 1,
    unknown: 1,
  });
});

test("a case that exceeds the deadline is a fail, not a hang (F20)", async () => {
  const groups = group("g", [
    {
      id: "hang",
      name: "hang",
      run: () => new Promise(() => {}), // never resolves
    },
  ]);
  const report = await runSuite(groups, { log: silent, deadlineMs: 20 });
  assert.equal(report.summary.fail, 1);
  const rec = report.results[0];
  assert.equal(rec.outcome, "fail");
  assert.match(rec.detail, /deadline/);
});

test("a throwing case is recorded as fail with the error detail", async () => {
  const groups = group("g", [
    {
      id: "boom",
      name: "boom",
      run: async () => {
        throw new Error("kaboom");
      },
    },
  ]);
  const report = await runSuite(groups, { log: silent });
  assert.equal(report.summary.fail, 1);
  assert.match(report.results[0].detail, /kaboom/);
});

test("onResult fires start + result phases for every case", async () => {
  const phases = [];
  const groups = group("g", [
    { id: "a", name: "a", run: async () => ({ ok: true, latencyMs: 1 }) },
  ]);
  await runSuite(groups, {
    log: silent,
    onResult: ({ phase }) => phases.push(phase),
  });
  assert.deepEqual(phases, ["start", "result"]);
});
