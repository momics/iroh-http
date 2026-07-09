/**
 * iroh-http test runner — Node.js
 *
 * Thin runner that imports createNode + key classes from the Node adapter,
 * binds them to native node:test primitives, and executes all shared suites.
 *
 * Usage:
 *   node tests/runners/node.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  asIrohPeer,
  createNode,
  PublicKey,
  SecretKey,
} from "../../packages/iroh-http-node/lib.js";

import { lifecycleTests } from "../suites/lifecycle.mjs";
import { errorTests } from "../suites/errors.mjs";
import { stressTests } from "../suites/stress.mjs";
import { eventTests } from "../suites/events.mjs";
import { sessionTests } from "../suites/sessions.mjs";
import { keyTests } from "../suites/keys.mjs";
import { discoveryTests } from "../suites/discovery.mjs";
import { selfFetchTests } from "../suites/self-fetch.mjs";
import { adapterValidationTests } from "../suites/adapter-validation.mjs";

/** Adapter from shared test API → node:test + node:assert */
const ctx = {
  createNode,
  PublicKey,
  SecretKey,
  asIrohPeer,
  test: (name, fn) => test(name, { timeout: 120_000 }, fn),
  assert: (v, msg) => assert.ok(v, msg),
  assertEqual: (a, b, msg) => assert.deepStrictEqual(a, b, msg),
  assertNotEqual: (a, b, msg) => assert.notDeepStrictEqual(a, b, msg),
  assertThrows: async (fn) => {
    try {
      await fn();
      assert.fail("expected function to throw, but it did not");
    } catch (err) {
      if (err.code === "ERR_ASSERTION") throw err;
      return err;
    }
  },
  assertResolves: async (promise) => {
    try {
      return await promise;
    } catch (err) {
      assert.fail(
        `expected promise to resolve, but it rejected: ${err.message}`,
      );
    }
  },
};

lifecycleTests(ctx);
errorTests(ctx);
stressTests(ctx);
eventTests(ctx);
sessionTests(ctx);
keyTests(ctx);
discoveryTests(ctx);
selfFetchTests(ctx);
adapterValidationTests(ctx);
