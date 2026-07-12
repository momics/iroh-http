/**
 * iroh-http cross-runtime HTTP compliance runner.
 *
 * Works with any adapter that implements the `IrohNode` interface from
 * `@momics/iroh-http-shared` (Node.js, Deno, Tauri).
 *
 * The runner spins up two nodes — server and client — and exercises every
 * case in `cases.json` through the full iroh-http stack.  The server is a
 * minimal routing server whose behaviour is defined by the fixture format
 * documented in `cases.json`.
 *
 * Usage (from each adapter's test directory):
 *   import { runCompliance } from "../../tests/http-compliance/runner.ts";
 *   import { createNode } from "../mod.ts";  // or lib.js for Node
 *   await runCompliance(createNode, { reporter: (msg) => console.log(msg) });
 */

// ── Types ─────────────────────────────────────────────────────────────────────

// @ts-ignore — .mjs sibling from the shared test suite (pure WHATWG).
import { runCases } from "./harness.mjs";

/** A single compliance test case as stored in cases.json. */
export interface ComplianceCase {
  id: string;
  description: string;
  request: {
    method: string;
    path: string;
    headers: Record<string, string>;
    body: string | { fill: number } | null;
  };
  response: {
    status: number;
    bodyExact?: string;
    bodyNot?: string;
    bodyNotEmpty?: boolean;
    bodyLengthExact?: number;
    headers?: Record<string, string>;
  };
}

export interface RunOptions {
  /** Called for each pass/fail event. */
  reporter?: (msg: string) => void;
  /** Filter to run only specific case IDs. */
  filter?: string[];
}

export interface RunResult {
  passed: number;
  failed: number;
  failures: Array<{ id: string; reason: string }>;
}

// ── Compliance echo server ────────────────────────────────────────────────────

/**
 * Build the server `Request` handler used by every compliance test.
 *
 * Routes:
 *   ANY  /status/:code           → that status, empty body
 *   ANY  /echo                   → 200, echoes request body
 *        /echo-path              → 200, body = request path
 *        /echo-method            → 200, body = request method
 *        /echo-length            → 200, body = request body byte length as string
 *        /echo-query             → 200, body = query string (including leading "?")
 *        /echo-header-count      → 200, body = number of request headers as string
 *   GET  /header/:name           → 200, body = value of that request header (or "")
 *   GET  /set-header/:name/:val  → 200, response header name=val
 *   GET  /set-headers/:n         → 200, n response headers x-h-0 … x-h-(n-1)
 *   GET  /stream/:n              → 200, streams n zero bytes
 */
function handleComplianceRequest(req: Request): Response | Promise<Response> {
  const url = new URL(req.url);
  const parts = url.pathname.split("/").filter(Boolean);

  // /status/:code
  if (parts[0] === "status" && parts[1]) {
    const code = parseInt(parts[1], 10);
    return new Response(null, { status: isNaN(code) ? 400 : code });
  }

  // /echo — return request body verbatim
  if (parts[0] === "echo" && parts.length === 1) {
    return new Response(req.body, { status: 200 });
  }

  // /echo-path
  if (parts[0] === "echo-path") {
    return new Response(url.pathname, { status: 200 });
  }

  // /echo-method
  if (parts[0] === "echo-method") {
    return new Response(req.method, { status: 200 });
  }

  // /echo-length — return body byte length
  if (parts[0] === "echo-length") {
    return (async () => {
      const buf = await req.arrayBuffer();
      return new Response(String(buf.byteLength), { status: 200 });
    })();
  }

  // /echo-query — return query string (empty string if none)
  if (parts[0] === "echo-query") {
    return new Response(url.search, { status: 200 });
  }

  // /echo-header-count — return number of request headers
  if (parts[0] === "echo-header-count") {
    const count = [...req.headers].length;
    return new Response(String(count), { status: 200 });
  }

  // /header/:name — return value of named request header
  if (parts[0] === "header" && parts[1]) {
    const val = req.headers.get(parts[1]) ?? "";
    return new Response(val, { status: 200 });
  }

  // /set-header/:name/:val
  if (parts[0] === "set-header" && parts[1] && parts[2]) {
    return new Response(null, {
      status: 200,
      headers: { [parts[1]]: parts[2] },
    });
  }

  // /set-headers/:n — respond with n custom headers x-h-0 … x-h-(n-1)
  if (parts[0] === "set-headers" && parts[1]) {
    const n = parseInt(parts[1], 10);
    if (!isNaN(n) && n >= 0) {
      const hdrs: Record<string, string> = {};
      for (let i = 0; i < n; i++) hdrs[`x-h-${i}`] = `v${i}`;
      return new Response(null, { status: 200, headers: hdrs });
    }
  }

  // /stream/:n — stream n zero bytes
  if (parts[0] === "stream" && parts[1]) {
    const n = parseInt(parts[1], 10);
    if (!isNaN(n) && n >= 0) {
      const body = new Uint8Array(n);
      return new Response(body, { status: 200 });
    }
  }

  return new Response("not found", { status: 404 });
}

// ── Request body builder ──────────────────────────────────────────────────────

function buildRequestBody(
  body: ComplianceCase["request"]["body"],
): BodyInit | null {
  if (body === null) return null;
  if (typeof body === "string") return body;
  // { fill: n } — n bytes of zeros
  return new Uint8Array(body.fill);
}

// ── Assertion helpers ─────────────────────────────────────────────────────────

async function assertResponse(
  resp: Response,
  expected: ComplianceCase["response"],
): Promise<string | null> {
  if (resp.status !== expected.status) {
    return `status: got ${resp.status}, want ${expected.status}`;
  }

  if (expected.bodyExact !== undefined) {
    const text = await resp.text();
    if (text !== expected.bodyExact) {
      return `body: got ${JSON.stringify(text)}, want ${
        JSON.stringify(expected.bodyExact)
      }`;
    }
    return null;
  }

  if (expected.bodyNot !== undefined) {
    const text = await resp.text();
    if (text === expected.bodyNot) {
      return `body: must not equal ${
        JSON.stringify(expected.bodyNot)
      }, but it did`;
    }
    return null;
  }

  if (expected.bodyNotEmpty) {
    const text = await resp.text();
    if (!text) return `body: expected non-empty, got empty string`;
    return null;
  }

  if (expected.bodyLengthExact !== undefined) {
    const buf = await resp.arrayBuffer();
    if (buf.byteLength !== expected.bodyLengthExact) {
      return `body length: got ${buf.byteLength}, want ${expected.bodyLengthExact}`;
    }
    return null;
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
    // Consume body to avoid leaks.
    await resp.body?.cancel();
    return null;
  }

  // No body assertion — just drain.
  await resp.body?.cancel();
  return null;
}

// ── Main runner ───────────────────────────────────────────────────────────────

export async function runCompliance(
  // deno-lint-ignore no-explicit-any
  createNode: (opts?: any) => Promise<any>,
  cases: ComplianceCase[],
  opts: RunOptions = {},
): Promise<RunResult> {
  const report = opts.reporter ?? (() => {});
  const result: RunResult = { passed: 0, failed: 0, failures: [] };

  const activeCases = opts.filter
    ? cases.filter((c) => opts.filter!.includes(c.id))
    : cases;

  // Create a shared server/client pair for the entire suite.
  const server = await createNode();
  const client = await createNode();

  try {
    const { id: serverId, addrs: serverAddrs } = await server.addr();

    const ac = new AbortController();
    server.serve({ signal: ac.signal }, handleComplianceRequest);

    for (const c of activeCases) {
      const reqBody = buildRequestBody(c.request.body);
      let resp: Response;
      try {
        resp = await client.fetch(`httpi://${serverId}${c.request.path}`, {
          method: c.request.method,
          headers: c.request.headers,
          body: reqBody,
          directAddrs: serverAddrs,
        });
      } catch (e) {
        const reason = `fetch threw: ${
          e instanceof Error ? e.message : String(e)
        }`;
        result.failed++;
        result.failures.push({ id: c.id, reason });
        report(`  FAIL ${c.id}: ${reason}`);
        continue;
      }

      const failure = await assertResponse(resp, c.response);
      if (failure) {
        result.failed++;
        result.failures.push({ id: c.id, reason: failure });
        report(`  FAIL ${c.id}: ${failure}`);
      } else {
        result.passed++;
        report(`  pass ${c.id}`);
      }
    }

    ac.abort();
  } finally {
    await server.close();
    await client.close();
  }

  return result;
}

// ── Remote-peer mode (cross-device harness) ────────────────────────────────────

/**
 * Run the compliance suite against an **already-serving remote peer** — the
 * cross-device path used by the on-device Test tab. Unlike {@link runCompliance},
 * this does not create nodes or start a server: the caller supplies a connected
 * client and the target peer's node id (the peer must already be serving the
 * compliance handler). Execution and assertions are delegated to the shared
 * {@link runCases} harness so every runtime exercises the identical logic.
 *
 * @param client     A node exposing `fetch(url, init)` (e.g. `IrohNode`).
 * @param serverId   The remote peer's node id (base32 public key).
 * @param cases      Cases to run.
 * @param opts       Optional filter, direct-address hint, and per-request timeout.
 */
export async function runCasesAgainstPeer(
  // deno-lint-ignore no-explicit-any
  client: { fetch: (url: string, init: any) => Promise<Response> },
  serverId: string,
  cases: ComplianceCase[],
  opts: {
    filter?: string[];
    directAddrs?: string[];
    timeout?: number;
    bail?: boolean;
    // deno-lint-ignore no-explicit-any
    onCaseResult?: (result: any) => void;
  } = {},
  // deno-lint-ignore no-explicit-any
): Promise<{ results: any[]; summary: { total: number; passed: number; failed: number; durationMs: number } }> {
  const activeCases = opts.filter
    ? cases.filter((c) => opts.filter!.includes(c.id))
    : cases;

  return runCases({
    fetch: (url: string, init: unknown) => client.fetch(url, init),
    serverId,
    directAddrs: opts.directAddrs,
    cases: activeCases,
    timeout: opts.timeout,
    bail: opts.bail,
    onCaseResult: opts.onCaseResult,
  });
}
