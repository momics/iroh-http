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
