/**
 * Collector server tests.
 *
 * Two layers:
 *   1. Pure-unit coverage of the persistence helpers (filename/slug/extract).
 *   2. A real end-to-end dogfood: a collector node serves `createHandler` and a
 *      second iroh-http node POSTs a report over the transport, proving the
 *      submit path the on-device harness uses actually lands JSON on disk.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createNode } from "../../../packages/iroh-http-node/lib.js";
import {
  createHandler,
  extractReports,
  reportFilename,
  slug,
} from "./collector.mjs";

/**
 * Resolve the collector's dialable direct addrs so a fresh in-process client
 * can reach it without waiting on n0 DNS discovery (mirrors run-node.mjs).
 */
async function collectorAddrs(collector) {
  const di = await collector.discoveryInfo().catch(() => null);
  return di?.directAddresses?.length
    ? di.directAddresses
    : (di?.directAddress ? [di.directAddress] : []);
}

const sampleReport = {
  schema: "iroh-http-interop/2",
  startedAt: "2026-07-13T20:00:00.000Z",
  finishedAt: "2026-07-13T20:00:05.000Z",
  self: { publicKey: "aaaa1111", platform: "ios" },
  target: { publicKey: "bbbb2222", platform: "desktop" },
  summary: {
    total: 3,
    pass: 2,
    fail: 1,
    skip: 0,
    transport: { direct: 1, relay: 0, unknown: 0 },
  },
  results: [{ id: "x", outcome: "pass" }],
};

test("slug strips unsafe characters and truncates", () => {
  assert.equal(slug("ios"), "ios");
  assert.equal(slug("a/b c:d"), "a_b_c_d");
  assert.equal(slug(undefined), "unknown");
  assert.ok(slug("x".repeat(100)).length <= 48);
});

test("reportFilename derives <self>-to-<target> from platforms", () => {
  const name = reportFilename(
    sampleReport,
    new Date("2026-07-13T20:00:05.000Z"),
  );
  assert.match(name, /^ios-to-desktop-2026-07-13T20-00-05-000Z\.json$/);
});

test("reportFilename falls back to key prefix then 'self'", () => {
  const noPlat = { self: { publicKey: "deadbeefcafe" }, target: {} };
  const name = reportFilename(noPlat, new Date("2026-07-13T20:00:05.000Z"));
  assert.match(name, /^deadbeef-to-self-/);
});

test("extractReports handles single, batch, and empty", () => {
  assert.equal(extractReports(sampleReport).length, 1);
  assert.equal(
    extractReports({ reports: [sampleReport, sampleReport] }).length,
    2,
  );
  assert.equal(extractReports(null).length, 0);
  assert.equal(extractReports("nope").length, 0);
});

test("POST /results over iroh-http persists the report to disk", async () => {
  const dir = await mkdtemp(join(tmpdir(), "iroh-collector-"));
  const collector = await createNode();
  const client = await createNode();
  try {
    collector.serve({}, createHandler({ reportsDir: dir }));
    const collectorId = collector.publicKey.toString();
    const addrs = await collectorAddrs(collector);

    // Submit a single report.
    const res = await client.fetch(`httpi://${collectorId}/results`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      directAddrs: addrs,
      body: JSON.stringify(sampleReport),
    });
    assert.equal(res.status, 201);
    const body = await res.json();
    assert.equal(body.ok, true);
    assert.equal(body.saved.length, 1);

    // The file exists on disk with the exact submitted content.
    const files = (await readdir(dir)).filter((f) => f.endsWith(".json"));
    assert.equal(files.length, 1);
    const written = JSON.parse(await readFile(join(dir, files[0]), "utf-8"));
    assert.deepEqual(written.summary, sampleReport.summary);

    // The index reflects it.
    const idxRes = await client.fetch(`httpi://${collectorId}/results`, {
      directAddrs: addrs,
    });
    const idx = await idxRes.json();
    assert.equal(idx.ok, true);
    assert.equal(idx.count, 1);
  } finally {
    await client.close().catch(() => {});
    await collector.close().catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
});

test("POST /results accepts a batch payload and writes each report", async () => {
  const dir = await mkdtemp(join(tmpdir(), "iroh-collector-"));
  const collector = await createNode();
  const client = await createNode();
  try {
    collector.serve({}, createHandler({ reportsDir: dir }));
    const collectorId = collector.publicKey.toString();

    const batch = {
      schema: "iroh-http-interop/2-batch",
      reports: [
        sampleReport,
        {
          ...sampleReport,
          target: { publicKey: "cccc3333", platform: "android" },
        },
      ],
    };
    const res = await client.fetch(`httpi://${collectorId}/results`, {
      method: "POST",
      directAddrs: await collectorAddrs(collector),
      body: JSON.stringify(batch),
    });
    assert.equal(res.status, 201);
    const body = await res.json();
    assert.equal(body.saved.length, 2);

    const files = (await readdir(dir)).filter((f) => f.endsWith(".json"));
    assert.equal(files.length, 2);
    assert.ok(files.some((f) => f.includes("-to-desktop-")));
    assert.ok(files.some((f) => f.includes("-to-android-")));
  } finally {
    await client.close().catch(() => {});
    await collector.close().catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
});

test("POST /results rejects an unparseable body", async () => {
  const dir = await mkdtemp(join(tmpdir(), "iroh-collector-"));
  const collector = await createNode();
  const client = await createNode();
  try {
    collector.serve({}, createHandler({ reportsDir: dir }));
    const collectorId = collector.publicKey.toString();
    const res = await client.fetch(`httpi://${collectorId}/results`, {
      method: "POST",
      directAddrs: await collectorAddrs(collector),
      body: "{not json",
    });
    assert.equal(res.status, 400);
    const files = (await readdir(dir)).filter((f) => f.endsWith(".json"));
    assert.equal(files.length, 0);
  } finally {
    await client.close().catch(() => {});
    await collector.close().catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
});
