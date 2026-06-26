/**
 * Cross-runtime compliance client — Deno.
 *
 * Reads the server's nodeId and addrs from the first argument (JSON string),
 * then runs every compliance case against that external server.  The server
 * is typically running in a DIFFERENT runtime (Node.js, Python, etc.), which
 * is what makes this a cross-runtime interoperability test.
 *
 * Called by tests/http-compliance/run.sh — do not run directly unless you
 * have a server already running.
 *
 * Usage:
 *   deno run --allow-read --allow-ffi \
 *     tests/http-compliance/client.deno.ts \
 *     '{"nodeId":"...","addrs":["..."]}'
 */

import { createNode } from "../../packages/iroh-http-deno/mod.ts";

// ── Parse server address from CLI ─────────────────────────────────────────────

const raw = Deno.args[0];
if (!raw) {
  console.error("Usage: client.deno.ts '<json>'");
  Deno.exit(1);
}
const { nodeId: serverId, addrs: serverAddrs } = JSON.parse(raw);

// ── Load compliance cases ─────────────────────────────────────────────────────

const casesUrl = new URL("./cases.json", import.meta.url);
const cases = JSON.parse(await Deno.readTextFile(casesUrl));

// ── Assertion helpers ─────────────────────────────────────────────────────────

// deno-lint-ignore no-explicit-any
async function assertResponse(
  resp: Response,
  expected: any,
): Promise<string | null> {
  if (resp.status !== expected.status) {
    return `status: got ${resp.status}, want ${expected.status}`;
  }
  if (expected.bodyExact !== undefined) {
    const text = await resp.text();
    return text !== expected.bodyExact
      ? `body: got ${JSON.stringify(text)}, want ${
        JSON.stringify(expected.bodyExact)
      }`
      : null;
  }
  if (expected.bodyNot !== undefined) {
    const text = await resp.text();
    return text === expected.bodyNot
      ? `body must not equal ${JSON.stringify(expected.bodyNot)}`
      : null;
  }
  if (expected.bodyNotEmpty) {
    const text = await resp.text();
    return text ? null : "body: expected non-empty";
  }
  if (expected.bodyLengthExact !== undefined) {
    const buf = await resp.arrayBuffer();
    return buf.byteLength !== expected.bodyLengthExact
      ? `body length: got ${buf.byteLength}, want ${expected.bodyLengthExact}`
      : null;
  }
  if (expected.headers) {
    for (const [k, v] of Object.entries(expected.headers)) {
      const actual = resp.headers.get(k as string);
      if (actual !== v) {
        return `header ${k}: got ${JSON.stringify(actual)}, want ${
          JSON.stringify(v)
        }`;
      }
    }
    await resp.body?.cancel();
    return null;
  }
  await resp.body?.cancel();
  return null;
}

// deno-lint-ignore no-explicit-any
function buildBody(body: any): BodyInit | null {
  if (body === null) return null;
  if (typeof body === "string") return body;
  return new Uint8Array(body.fill);
}

// ── Run ───────────────────────────────────────────────────────────────────────

const client = await createNode();

let passed = 0;
let failed = 0;

try {
  for (const c of cases) {
    if (!c.id) continue; // skip comment entries
    if (c.skip) {
      console.log(`  skip  ${c.id}: ${c.skip}`);
      continue;
    }
    if (c.requests || c.concurrent > 1 || c.repeat > 1) continue;
    let resp: Response;
    try {
      resp = await client.fetch(`httpi://${serverId}${c.request.path}`, {
        method: c.request.method,
        headers: c.request.headers,
        body: buildBody(c.request.body),
        directAddrs: serverAddrs,
      });
    } catch (e) {
      const reason = `fetch threw: ${
        e instanceof Error ? e.message : String(e)
      }`;
      failed++;
      console.log(`  FAIL  ${c.id}: ${reason}`);
      continue;
    }

    const failure = await assertResponse(resp, c.response);
    if (failure) {
      failed++;
      console.log(`  FAIL  ${c.id}: ${failure}`);
    } else {
      passed++;
      console.log(`  pass  ${c.id}`);
    }
  }
} finally {
  await client.close();
}

console.log(`\n${passed} passed, ${failed} failed`);
Deno.exit(failed > 0 ? 1 : 0);
