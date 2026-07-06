/**
 * Deno adapter test — verifies Deno FFI-specific functionality.
 *
 * Tests here exercise APIs only available through the Deno FFI adapter
 * (generateSecretKey, secretKeySign, publicKeyVerify) and Deno-specific
 * runtime behavior (sanitizeOps).
 *
 * Cross-runtime integration tests live in tests/suites/ and are executed
 * by tests/runners/deno.ts.
 *
 * Run:  deno test -A test/adapter.test.ts
 */

import {
  assert,
  assertEquals,
  assertInstanceOf,
  assertThrows,
  fail,
} from "jsr:@std/assert@^1";
import { createNode, PublicKey } from "../mod.ts";
import { generateSecretKey, publicKeyVerify, secretKeySign } from "../mod.ts";
import { bigintToSafeNumber } from "../src/adapter.ts";

async function waitFor(
  predicate: () => Promise<boolean>,
  message: string,
  timeoutMs = 1_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let last = false;
  while (Date.now() < deadline) {
    last = await predicate();
    if (last) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  fail(`${message}; last value: ${String(last)}`);
}

async function withTimeout<T>(
  promise: Promise<T>,
  message: string,
  timeoutMs = 2_000,
): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error(message)), timeoutMs);
      }),
    ]);
  } finally {
    if (timeout !== undefined) clearTimeout(timeout);
  }
}

// ── bigintToSafeNumber handle guard (regression #252) ────────────────────────

Deno.test("bigintToSafeNumber — passes through in-range handles", () => {
  assertEquals(bigintToSafeNumber(0n), 0);
  assertEquals(bigintToSafeNumber(42n), 42);
  assertEquals(
    bigintToSafeNumber(BigInt(Number.MAX_SAFE_INTEGER)),
    Number.MAX_SAFE_INTEGER,
  );
});

Deno.test("bigintToSafeNumber — rejects out-of-range u64 handles instead of truncating", () => {
  // A slotmap u64 key whose generation bits push it past 2^53 must throw,
  // not silently round to a different (wrong) resource identity.
  const unsafe = BigInt(Number.MAX_SAFE_INTEGER) + 1n;
  assertThrows(
    () => bigintToSafeNumber(unsafe, "reqBodyHandle"),
    RangeError,
    "reqBodyHandle",
  );
  assertThrows(() => bigintToSafeNumber(-1n), RangeError);
});

// ── generateSecretKey (FFI-only) ─────────────────────────────────────────────

Deno.test("generateSecretKey — returns 32 bytes", async () => {
  const key = await generateSecretKey();
  assertInstanceOf(key, Uint8Array);
  assertEquals(key.length, 32);
});

Deno.test("generateSecretKey — successive calls differ", async () => {
  const k1 = await generateSecretKey();
  const k2 = await generateSecretKey();
  assert(
    !k1.every((b: number, i: number) => b === k2[i]),
    "Two generated keys must differ",
  );
});

// ── secretKeySign (FFI-only) ─────────────────────────────────────────────────

Deno.test("secretKeySign — returns 64-byte signature", async () => {
  const key = await generateSecretKey();
  const sig = await secretKeySign(key, new TextEncoder().encode("hello"));
  assertInstanceOf(sig, Uint8Array);
  assertEquals(sig.length, 64);
});

Deno.test("secretKeySign — deterministic for same key + message", async () => {
  const key = await generateSecretKey();
  const msg = new TextEncoder().encode("deterministic");
  const s1 = await secretKeySign(key, msg);
  const s2 = await secretKeySign(key, msg);
  assertEquals(s1, s2);
});

// ── publicKeyVerify (FFI-only) ───────────────────────────────────────────────

Deno.test("publicKeyVerify — valid signature passes", async () => {
  const key = await generateSecretKey();
  const node = await createNode({ key, disableNetworking: true });
  const msg = new TextEncoder().encode("test message");
  const sig = await secretKeySign(key, msg);

  const pubBytes = node.publicKey.bytes;
  try {
    assert(
      await publicKeyVerify(pubBytes, msg, sig),
      "Valid signature must verify",
    );
    const tampered = new Uint8Array(sig);
    tampered[0] ^= 0xff;
    assert(
      !(await publicKeyVerify(pubBytes, msg, tampered)),
      "Tampered signature must fail",
    );
  } finally {
    await node.close();
  }
});

// ── Deno-specific serve lifecycle ────────────────────────────────────────────

// Regression #115: serve loop must not hold pending ops after shutdown.
// This test uses sanitizeOps: true (the Deno default) intentionally —
// if stopServe() doesn't drain the pending nextRequest() call, Deno's
// sanitizeOps check will fail.
Deno.test({
  name: "serve — no pending ops remain after signal abort (regression #115)",
  sanitizeOps: true,
}, async () => {
  const server = await createNode({ bindAddr: "127.0.0.1:0" });
  const ac = new AbortController();

  const handle = server.serve(
    { signal: ac.signal },
    (_req: Request) => new Response("ok"),
  );

  ac.abort();
  await server.close();
  await handle.finished;
});

Deno.test({
  name: "pathChanges — abort releases native subscriptions (regression #279)",
  sanitizeOps: false,
}, async () => {
  const server = await createNode({ bindAddr: "127.0.0.1:0" });
  const client = await createNode({ bindAddr: "127.0.0.1:0" });
  const handle = server.serve(() => new Response("ok"));
  const { id: serverId, addrs: serverAddrs } = await server.addr();
  await (await client.fetch(`httpi://${serverId}/warmup`, {
    directAddrs: serverAddrs,
  })).text();

  const baselineStats = await client.stats();
  const baselineSubscriptions = baselineStats.activePathSubscriptions;
  const baselineWatchers = baselineStats.activePathWatchers;

  try {
    for (const cycles of [1, 10]) {
      for (let i = 0; i < cycles; i++) {
        const ac = new AbortController();
        const iterator = client.pathChanges(PublicKey.fromString(serverId), {
          signal: ac.signal,
        })[Symbol.asyncIterator]();
        const first = await withTimeout(
          iterator.next(),
          "pathChanges did not yield the active path",
        );
        assert(!first.done, "pathChanges should yield before teardown");
        ac.abort();

        await waitFor(
          async () => {
            const stats = await client.stats();
            return stats.activePathSubscriptions <= baselineSubscriptions &&
              stats.activePathWatchers <= baselineWatchers;
          },
          `path-change subscriptions should return to baseline after aborting ${
            i + 1
          }/${cycles} iterators`,
          1_500,
        );
      }
    }
  } finally {
    await client.close();
    await server.close();
    await handle.finished.catch(() => {});
  }
});

// ── Load-shed 503 (depends on maxConcurrency option) ─────────────────────────

Deno.test({
  name: "serve — loadShed returns 503 when maxConcurrency exceeded",
  sanitizeOps: false,
}, async () => {
  const STREAMS = 8;
  const CONCURRENCY_LIMIT = 2;

  const server = await createNode({
    disableNetworking: true,
    bindAddr: "127.0.0.1:0",
  });
  const client = await createNode({
    disableNetworking: true,
    bindAddr: "127.0.0.1:0",
  });
  const { id: serverId, addrs: serverAddrs } = await server.addr();

  const ac = new AbortController();
  const handle = server.serve(
    { signal: ac.signal, maxConcurrency: CONCURRENCY_LIMIT },
    async () => {
      await new Promise((r) => setTimeout(r, 50));
      return new Response("ok");
    },
  );

  try {
    const results = await Promise.all(
      Array.from({ length: STREAMS }, () =>
        client
          .fetch(`httpi://${serverId}/load`, { directAddrs: serverAddrs })
          .then(async (r) => ({ status: r.status }))),
    );

    const ok = results.filter((r) => r.status === 200);
    const shed = results.filter((r) => r.status === 503);

    assert(
      shed.length > 0,
      `Expected ≥1 load-shed 503, all ${STREAMS} got 200`,
    );
    assert(ok.length > 0, `Expected ≥1 success, all ${STREAMS} got 503`);
  } finally {
    ac.abort();
    await handle.finished;
    await server.close();
    await client.close();
  }
});

// ── Low-concurrency stale-handle variant (#119) ──────────────────────────────

Deno.test({
  name:
    "regression #119 — 8-stream burst × 20 iterations: no stale-handle errors",
  sanitizeOps: false,
}, async () => {
  const STREAMS = 8;
  const ITERS = 20;
  const BODY = "x".repeat(4096);

  const errors: string[] = [];
  const originalConsoleError = console.error.bind(console);
  console.error = (...args: unknown[]) => {
    const msg = args.map(String).join(" ");
    if (
      msg.includes("unknown handle") ||
      msg.includes("node closed or not found") ||
      msg.includes("sendChunk failed")
    ) {
      errors.push(msg);
    }
    originalConsoleError(...args);
  };

  const server = await createNode({
    disableNetworking: true,
    bindAddr: "127.0.0.1:0",
  });
  const client = await createNode({
    disableNetworking: true,
    bindAddr: "127.0.0.1:0",
  });
  const { id: serverId, addrs: serverAddrs } = await server.addr();

  try {
    for (let iter = 0; iter < ITERS; iter++) {
      const ac = new AbortController();
      const handle = server.serve(
        { signal: ac.signal, loadShed: false },
        () => new Response(BODY),
      );

      await Promise.all(
        Array.from({ length: STREAMS }, () =>
          client
            .fetch(`httpi://${serverId}/data`, { directAddrs: serverAddrs })
            .then((r) => r.text())),
      );

      ac.abort();
      await handle.finished;
    }

    assertEquals(errors, [], `handle errors detected: ${errors.join(" | ")}`);
  } finally {
    console.error = originalConsoleError;
    await server.close();
    await client.close();
  }
});

// ── SIGINT / close_all serve shutdown (regression #155) ──────────────────────

// Regression #155: iroh_http_close_all (the SIGINT/SIGTERM handler) used to
// only drain the endpoint registry, leaving serve queues live and the JS
// polling loop spinning forever. The Deno process would never exit on CTRL+C
// while .serve() was active. close_all must now also wake all serve queues.
import { _closeAllForTesting } from "../src/adapter.ts";

Deno.test({
  name: "serve — close_all wakes pending polling loop (regression #155)",
  sanitizeOps: false,
  sanitizeResources: false,
}, async () => {
  const server = await createNode({ bindAddr: "127.0.0.1:0" });
  const handle = server.serve((_req: Request) => new Response("ok"));

  // Simulate the SIGINT path. Without the fix this never wakes the polling
  // loop, the test times out, and Deno reports the leaked async op.
  _closeAllForTesting();

  // Bound the wait so a regression surfaces as a clear timeout rather than
  // hanging indefinitely.
  const timeout = new Promise<"timeout">((resolve) =>
    setTimeout(() => resolve("timeout"), 5_000)
  );
  const finished = handle.finished.then(() => "finished" as const);
  const result = await Promise.race([finished, timeout]);
  assertEquals(result, "finished", "serve loop did not exit after close_all");
});
