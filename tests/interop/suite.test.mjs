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
import { buildSuite, runSuite } from "./suite.mjs";

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

test("skipped transport probes do not count as transport evidence", async () => {
  const groups = group("g", [
    {
      id: "relay-not-exercised",
      name: "relay-not-exercised",
      run: async () => ({
        ok: false,
        skip: true,
        transport: "direct",
        latencyMs: 1,
      }),
    },
  ]);
  const report = await runSuite(groups, { log: silent });
  assert.deepEqual(report.summary.transport, {
    direct: 0,
    relay: 0,
    unknown: 0,
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

test("relay fallback uses a fresh node and closes it after the probe", async () => {
  const calls = [];
  let isolationRequest = null;
  const isolated = {
    fetch: async (_url, init) => {
      calls.push(init);
      return new Response("ok");
    },
    peerStats: async () => ({ paths: [{ active: true, relay: true }] }),
    close: async () => calls.push("closed"),
  };
  const primary = {
    fetch: async () => {
      throw new Error("warmed primary node must not be used");
    },
  };
  const groups = buildSuite({
    node: primary,
    self: { nodeId: "self", platform: "test" },
    peer: {
      nodeId: "peer",
      platform: "test",
      addrs: ["192.0.2.1:443", "https://relay.example"],
    },
    cases: [],
    selfLoopbackCase: { id: "self-loopback" },
    createIsolatedNode: async (request) => {
      isolationRequest = request;
      return isolated;
    },
  });
  const relay = groups
    .find((group) => group.id === "relay-fallback")
    .tests.find((entry) => entry.id === "relay-fallback-fetch");

  const result = await relay.run();

  assert.equal(result.ok, true);
  assert.equal(result.transport, "relay");
  assert.deepEqual(isolationRequest, {
    purpose: "relay-fallback",
    nodeOptions: { discovery: { dns: false } },
  });
  assert.equal(calls[0].directAddrs, undefined);
  assert.equal(calls[0].relayUrl, "https://relay.example");
  assert.equal(calls.at(-1), "closed");
});

test("serve-stop uses an isolated node while the app node is serving", async () => {
  let serving = false;
  let closed = false;
  const urls = [];
  const isolated = {
    publicKey: { toString: () => "isolated-self" },
    serve: ({ signal }) => {
      serving = true;
      let resolveFinished;
      const finished = new Promise((resolve) => {
        resolveFinished = resolve;
      });
      signal.addEventListener("abort", () => {
        serving = false;
        resolveFinished();
      });
      return { finished };
    },
    fetch: async (url) => {
      urls.push(url);
      if (!serving) throw new Error("connection refused");
      return new Response("ok");
    },
    close: async () => {
      closed = true;
    },
  };
  const groups = buildSuite({
    node: {},
    self: { nodeId: "occupied-primary", platform: "test" },
    peer: null,
    cases: [],
    selfLoopbackCase: { id: "self-loopback" },
    handler: () => new Response("ok"),
    isServing: true,
    createIsolatedNode: async () => isolated,
  });
  const lifecycle = groups
    .find((group) => group.id === "serve-stop")
    .tests.find((entry) => entry.id === "serve-stop-lifecycle");

  const result = await lifecycle.run();

  assert.equal(result.ok, true);
  assert.deepEqual(urls, [
    "httpi://isolated-self/hello",
    "httpi://isolated-self/hello",
  ]);
  assert.equal(closed, true);
});

test("relay fallback closes its isolated node when fetch fails", async () => {
  let closed = false;
  const isolated = {
    fetch: async () => {
      throw new Error("offline");
    },
    close: async () => {
      closed = true;
    },
  };
  const groups = buildSuite({
    node: {},
    self: { nodeId: "self", platform: "test" },
    peer: {
      nodeId: "peer",
      platform: "test",
      addrs: ["https://relay.example"],
    },
    cases: [],
    selfLoopbackCase: { id: "self-loopback" },
    createIsolatedNode: async () => isolated,
  });
  const relay = groups
    .find((group) => group.id === "relay-fallback")
    .tests.find((entry) => entry.id === "relay-fallback-fetch");

  const result = await relay.run();

  assert.equal(result.ok, false);
  assert.equal(result.skip, undefined);
  assert.equal(closed, true);
});

test("a fresh relay probe is inconclusive when only a direct upgrade is measured", async () => {
  const isolated = {
    fetch: async () => new Response("ok"),
    peerStats: async () => ({ paths: [{ active: true, relay: false }] }),
    close: async () => {},
  };
  const groups = buildSuite({
    node: {},
    self: { nodeId: "self", platform: "test" },
    peer: {
      nodeId: "peer",
      platform: "test",
      addrs: ["https://relay.example"],
    },
    cases: [],
    selfLoopbackCase: { id: "self-loopback" },
    createIsolatedNode: async () => isolated,
  });
  const relay = groups
    .find((group) => group.id === "relay-fallback")
    .tests.find((entry) => entry.id === "relay-fallback-fetch");

  const result = await relay.run();

  assert.equal(result.ok, false);
  assert.equal(result.skip, true);
  assert.match(result.detail, /fresh endpoint measured transport=direct/);
});

test("relay fallback skips only when the peer advertises no relay URL", async () => {
  let created = false;
  const groups = buildSuite({
    node: {},
    self: { nodeId: "self", platform: "test" },
    peer: { nodeId: "peer", platform: "test", addrs: ["192.0.2.1:443"] },
    cases: [],
    selfLoopbackCase: { id: "self-loopback" },
    createIsolatedNode: async () => {
      created = true;
      return {};
    },
  });
  const relay = groups
    .find((group) => group.id === "relay-fallback")
    .tests.find((entry) => entry.id === "relay-fallback-fetch");

  const result = await relay.run();

  assert.equal(result.skip, true);
  assert.match(result.detail, /no relay URL/);
  assert.equal(created, false);
});

test("result events expose live progress and aggregate outcome counts", async () => {
  const events = [];
  const groups = group("g", [
    { id: "a", name: "a", run: async () => ({ ok: true, latencyMs: 1 }) },
    { id: "b", name: "b", run: async () => ({ ok: false, latencyMs: 1 }) },
  ]);

  await runSuite(groups, {
    log: silent,
    onResult: (event) => {
      if (event.phase === "result") events.push(event);
    },
  });

  assert.deepEqual(
    events.map(({ progress, summary }) => ({ progress, summary })),
    [
      {
        progress: { completed: 1, total: 2 },
        summary: {
          total: 2,
          completed: 1,
          pass: 1,
          fail: 0,
          skip: 0,
          transport: { direct: 0, relay: 0, unknown: 0 },
        },
      },
      {
        progress: { completed: 2, total: 2 },
        summary: {
          total: 2,
          completed: 2,
          pass: 1,
          fail: 1,
          skip: 0,
          transport: { direct: 0, relay: 0, unknown: 0 },
        },
      },
    ],
  );
});

test("direct-dial evidence describes supplied hints without claiming the chosen path", async () => {
  const groups = buildSuite({
    node: {
      peerStats: async () => ({ paths: [{ active: true, relay: false }] }),
    },
    self: { nodeId: "self", platform: "test" },
    peer: { nodeId: "peer", platform: "test", addrs: ["192.0.2.1:443"] },
    fetch: async () => new Response("ok"),
    cases: [],
    selfLoopbackCase: { id: "self-loopback" },
  });
  const direct = groups
    .find((group) => group.id === "direct-dial")
    .tests.find((entry) => entry.id === "direct-dial-fetch");

  const result = await direct.run();

  assert.equal(result.ok, true);
  assert.match(result.detail, /1 advertised direct address hint supplied/);
  assert.doesNotMatch(result.detail, /via 192\.0\.2\.1:443/);
});

test("peer compliance fetches retain the advertised relay URL", async () => {
  const inits = [];
  const runCases = async ({ fetch, onCaseResult }) => {
    await fetch("httpi://peer/hello", { method: "GET" });
    onCaseResult({ ok: true, status: 200, latencyMs: 1 });
  };
  const groups = buildSuite({
    node: {},
    self: { nodeId: "self", platform: "test" },
    peer: {
      nodeId: "peer",
      platform: "test",
      addrs: ["192.0.2.1:443", "https://relay.example"],
    },
    fetch: async (_url, init) => {
      inits.push(init);
      return new Response("ok");
    },
    cases: [{ id: "peer-case" }],
    selfLoopbackCase: { id: "self-loopback" },
    runCases,
  });
  const compliance = groups.find((group) => group.id === "http-compliance");

  await compliance.tests[0].run();
  await compliance.tests[1].run();

  assert.equal(inits[0].relayUrl, undefined);
  assert.equal(inits[1].relayUrl, "https://relay.example");
});
