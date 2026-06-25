/**
 * Cross-runtime compliance client — Node.js.
 *
 * Reads server nodeId + addrs from the first CLI argument (JSON string),
 * then runs all compliance cases against that external server.
 *
 * Usage (called by run.sh):
 *   node tests/http-compliance/client.mjs '{"nodeId":"...","addrs":["..."]}'
 */

import { createNode } from "../../packages/iroh-http-node/lib.js";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const raw = process.argv[2];
if (!raw) {
  console.error("Usage: client.mjs '<json>'");
  process.exit(1);
}
const { nodeId: serverId, addrs: serverAddrs } = JSON.parse(raw);

const __dir = dirname(fileURLToPath(import.meta.url));
const cases = JSON.parse(readFileSync(join(__dir, "cases.json"), "utf8"));

async function assertResponse(resp, expected) {
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
      const actual = resp.headers.get(k);
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

function buildBody(body) {
  if (body === null) return null;
  if (typeof body === "string") return body;
  return new Uint8Array(body.fill);
}

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
    let resp;
    try {
      resp = await client.fetch(`httpi://${serverId}${c.request.path}`, {
        method: c.request.method,
        headers: c.request.headers,
        body: buildBody(c.request.body),
        directAddrs: serverAddrs,
      });
    } catch (e) {
      failed++;
      console.log(`  FAIL  ${c.id}: fetch threw: ${e?.message ?? e}`);
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
process.exit(failed > 0 ? 1 : 0);
