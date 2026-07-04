/**
 * Node.js adapter test — verifies NAPI-specific module loading and exports.
 *
 * Cross-runtime integration tests live in tests/suites/ and are executed
 * by tests/runners/node.mjs.
 *
 * Run:  node --test test/adapter.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { createNode, PublicKey, SecretKey } from "../lib.js";

async function waitFor(predicate, message, timeoutMs = 1_000) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    last = await predicate();
    if (last) return last;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.fail(`${message}; last value: ${String(last)}`);
}

test("module exports createNode, PublicKey, SecretKey", () => {
  assert.equal(typeof createNode, "function", "createNode must be a function");
  assert.ok(PublicKey != null, "PublicKey must be exported");
  assert.ok(SecretKey != null, "SecretKey must be exported");
});

test("createNode via NAPI returns a node with expected API surface", async () => {
  const node = await createNode({ disableNetworking: true });
  try {
    assert.ok(node.publicKey != null, "publicKey must exist");
    assert.ok(node.secretKey != null, "secretKey must exist");
    assert.equal(typeof node.fetch, "function", "fetch must be a function");
    assert.equal(typeof node.serve, "function", "serve must be a function");
    assert.equal(typeof node.close, "function", "close must be a function");
    assert.equal(typeof node.dial, "function", "dial must be a function");
    assert.equal(typeof node.addr, "function", "addr must be a function");
    assert.equal(typeof node.ticket, "function", "ticket must be a function");
  } finally {
    await node.close();
  }
});

test("fetch numeric validation rejects invalid values instead of defaulting", async () => {
  const node = await createNode({ disableNetworking: true });
  const handle = node.serve(() => new Response("ok"));
  const { id } = await node.addr();
  const url = `httpi://${id}/validation`;
  const maxTimeoutMs = 300_000;
  const maxBodyBytes = 16 * 1024 * 1024;

  try {
    for (
      const value of [
        Number.NaN,
        Number.POSITIVE_INFINITY,
        -1,
        1.5,
        maxTimeoutMs + 1,
      ]
    ) {
      await assert.rejects(
        () => node.fetch(url, { requestTimeout: value }),
        undefined,
        `requestTimeout=${String(value)} must reject`,
      );
    }

    for (
      const value of [
        Number.NaN,
        Number.POSITIVE_INFINITY,
        -1,
        1.5,
        maxBodyBytes + 1,
      ]
    ) {
      await assert.rejects(
        () => node.fetch(url, { maxResponseBodyBytes: value }),
        undefined,
        `maxResponseBodyBytes=${String(value)} must reject`,
      );
    }
  } finally {
    await node.close();
    await handle.finished.catch(() => {});
  }
});

test("pathChanges abort releases native subscriptions (regression #279)", async () => {
  const node = await createNode({ disableNetworking: true });
  const baselineStats = await node.stats();
  const baselineSubscriptions = baselineStats.activePathSubscriptions;
  const baselineWatchers = baselineStats.activePathWatchers;
  const controllers = [];
  const pending = [];

  try {
    for (const cycles of [1, 100]) {
      for (let i = 0; i < cycles; i++) {
        const bytes = new Uint8Array(32);
        bytes[30] = cycles;
        bytes[31] = i;
        const ac = new AbortController();
        controllers.push(ac);
        const iterator = node.pathChanges(PublicKey.fromBytes(bytes), {
          signal: ac.signal,
        })[Symbol.asyncIterator]();
        pending.push(iterator.next().catch(() => null));
      }

      await waitFor(
        async () =>
          (await node.stats()).activePathSubscriptions >=
            baselineSubscriptions + cycles,
        `expected ${cycles} active path-change subscriptions`,
      );

      for (const ac of controllers.splice(0)) ac.abort();

      await waitFor(
        async () => {
          const stats = await node.stats();
          return stats.activePathSubscriptions <= baselineSubscriptions &&
            stats.activePathWatchers <= baselineWatchers;
        },
        `path-change subscriptions should return to baseline after aborting ${cycles} iterators`,
        1_500,
      );
    }
  } finally {
    for (const ac of controllers) ac.abort();
    await node.close();
    await Promise.allSettled(pending);
  }
});
