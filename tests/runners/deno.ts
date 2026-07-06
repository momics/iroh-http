/**
 * iroh-http test runner — Deno
 *
 * Thin runner that imports createNode + key classes from the Deno adapter,
 * binds them to Deno.test + @std/assert, and executes all shared suites.
 *
 * Usage:
 *   deno test -A tests/runners/deno.ts
 */

import { assert, assertEquals, assertNotEquals } from "jsr:@std/assert@^1";
import {
  createNode,
  PublicKey,
  SecretKey,
} from "../../packages/iroh-http-deno/mod.ts";

import { lifecycleTests } from "../suites/lifecycle.mjs";
import { errorTests } from "../suites/errors.mjs";
import { stressTests } from "../suites/stress.mjs";
import { eventTests } from "../suites/events.mjs";
import { sessionTests } from "../suites/sessions.mjs";
import { keyTests } from "../suites/keys.mjs";
import { discoveryTests } from "../suites/discovery.mjs";
import { selfFetchTests } from "../suites/self-fetch.mjs";
import { adapterValidationTests } from "../suites/adapter-validation.mjs";

/** Adapter from shared test API → Deno.test + @std/assert */
const ctx = {
  createNode,
  PublicKey,
  SecretKey,
  test: (name: string, fn: () => Promise<void>) =>
    Deno.test({ name, sanitizeOps: false, sanitizeResources: false }, fn),
  assert: (v: unknown, msg?: string) => assert(v, msg),
  assertEqual: (a: unknown, b: unknown, msg?: string) =>
    assertEquals(a, b, msg),
  assertNotEqual: (a: unknown, b: unknown, msg?: string) =>
    assertNotEquals(a, b, msg),
  assertThrows: async (fn: () => Promise<unknown>) => {
    try {
      await fn();
      throw new Error("expected function to throw, but it did not");
    } catch (err) {
      if (
        (err as Error).message === "expected function to throw, but it did not"
      ) throw err;
      return err;
    }
  },
  assertResolves: async (promise: Promise<unknown>) => {
    try {
      return await promise;
    } catch (err) {
      throw new Error(
        `expected promise to resolve, but it rejected: ${
          (err as Error).message
        }`,
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
