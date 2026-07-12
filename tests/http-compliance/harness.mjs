/**
 * Shared, transport-agnostic compliance-case runner (client side).
 *
 * Given a `fetch`-capable client and a target server node id, this executes
 * every case in `cases.json` and returns a structured per-case result plus a
 * summary. It knows nothing about how nodes are created or discovered, so it is
 * reused by:
 *   - the in-process runners (Node / Deno / Tauri headless), and
 *   - the on-device Test tab, which drives it against a *remote* peer over the
 *     network.
 *
 * The case format matches `cases.json` and mirrors the logic previously inlined
 * in `run-tauri.ts` (single / concurrent / sequential / multi-request cases).
 *
 * Pure WHATWG — no Node, Deno, or framework dependencies.
 */

// @ts-nocheck
import { assertResponse } from "./assertions.mjs";

const now = () =>
  typeof performance !== "undefined" && performance.now
    ? performance.now()
    : Date.now();

/**
 * @param {string | { fill: number } | null | undefined} bodySpec
 * @returns {string | Uint8Array | undefined}
 */
export function buildBody(bodySpec) {
  if (bodySpec === null || bodySpec === undefined) return undefined;
  if (typeof bodySpec === "string") return bodySpec;
  if (typeof bodySpec === "object" && "fill" in bodySpec) {
    return new Uint8Array(bodySpec.fill);
  }
  return undefined;
}

/**
 * @param {Record<string, string | { fill: number }> | undefined} headersSpec
 * @returns {Record<string, string>}
 */
export function buildHeaders(headersSpec) {
  const h = {};
  if (!headersSpec) return h;
  for (const [k, v] of Object.entries(headersSpec)) {
    if (typeof v === "object" && v !== null && "fill" in v) {
      h[k] = "x".repeat(v.fill);
    } else {
      h[k] = v;
    }
  }
  return h;
}

/**
 * @typedef {Object} RunCasesOptions
 * @property {(url: string, init: object) => Promise<Response>} fetch
 *   Client fetch. Receives a fully-formed `httpi://<serverId><path>` URL.
 * @property {string} serverId          Target server node id (base32 public key).
 * @property {string[]} [directAddrs]   Optional direct socket addresses hint.
 * @property {Array<object>} cases      Cases to run (already filtered/ordered).
 * @property {number} [timeout]         Per-request timeout in ms (default 30000).
 * @property {boolean} [bail]           Stop after the first failing case.
 * @property {(result: CaseResult) => void} [onCaseResult]  Progress callback.
 */

/**
 * @typedef {Object} CaseResult
 * @property {string} id
 * @property {string} [description]
 * @property {boolean} ok
 * @property {number|null} status       Response status of the (last) request.
 * @property {number} latencyMs         Wall-clock duration of the whole case.
 * @property {string|null} error        First failure reason or thrown message.
 */

/**
 * Run a list of compliance cases against a single serving peer.
 *
 * @param {RunCasesOptions} opts
 * @returns {Promise<{ results: CaseResult[], summary: { total: number, passed: number, failed: number, durationMs: number } }>}
 */
export async function runCases(opts) {
  const { fetch, serverId, directAddrs, cases, onCaseResult } = opts;
  const timeout = opts.timeout ?? 30000;

  async function runSingleRequest(req) {
    const body = buildBody(req.body);
    const headers = buildHeaders(req.headers);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeout);
    try {
      const init = {
        method: req.method,
        headers,
        body,
        signal: controller.signal,
      };
      if (directAddrs && directAddrs.length) init.directAddrs = directAddrs;
      const res = await fetch(`httpi://${serverId}${req.path}`, init);
      const buf = await res.arrayBuffer();
      const bodyText = new TextDecoder().decode(buf);
      return { res, bodyText, bodyLength: buf.byteLength };
    } finally {
      clearTimeout(timer);
    }
  }

  const results = [];
  const suiteStart = now();

  for (const tc of cases) {
    if (!tc || !tc.id) continue;
    const started = now();
    let ok = true;
    let status = null;
    let error = null;

    try {
      if (tc.requests) {
        // ── Multi-step requests ─────────────────────────────────────────
        for (let j = 0; j < tc.requests.length; j++) {
          const sub = tc.requests[j];
          const { res, bodyText, bodyLength } = await runSingleRequest(sub);
          status = res.status;
          const r = assertResponse(
            { response: sub.expect },
            res,
            bodyText,
            bodyLength,
          );
          if (!r.pass) {
            ok = false;
            error = `[step ${j}] ${r.failures.join("; ")}`;
            break;
          }
        }
      } else {
        const concurrency = tc.concurrent ?? 1;
        const repeatCount = tc.repeat ?? 1;
        const totalRuns = Math.max(concurrency, repeatCount);
        const sequential = tc.sequential ?? 0;

        if (sequential > 0) {
          // ── Sequential requests ───────────────────────────────────────
          for (let j = 0; j < sequential; j++) {
            const { res, bodyText, bodyLength } = await runSingleRequest(
              tc.request,
            );
            status = res.status;
            const r = assertResponse(tc, res, bodyText, bodyLength);
            if (!r.pass) {
              ok = false;
              error = `[iteration ${j}] ${r.failures.join("; ")}`;
              break;
            }
          }
        } else if (concurrency > 1 || repeatCount > 1) {
          // ── Concurrent / repeated requests ────────────────────────────
          const settled = await Promise.all(
            Array.from({ length: totalRuns }, () =>
              runSingleRequest(tc.request)),
          );
          const bodies = [];
          for (let j = 0; j < settled.length; j++) {
            const { res, bodyText, bodyLength } = settled[j];
            status = res.status;
            bodies.push(bodyText);
            const r = assertResponse(tc, res, bodyText, bodyLength);
            if (!r.pass) {
              ok = false;
              error = error ?? `[instance ${j}] ${r.failures.join("; ")}`;
            }
          }
          if (tc.assertAllBodiesEqual && ok && new Set(bodies).size > 1) {
            ok = false;
            error = "bodies differ across runs";
          }
        } else {
          // ── Single request ────────────────────────────────────────────
          const { res, bodyText, bodyLength } = await runSingleRequest(
            tc.request,
          );
          status = res.status;
          const r = assertResponse(tc, res, bodyText, bodyLength);
          if (!r.pass) {
            ok = false;
            error = r.failures.join("; ");
          }
        }
      }
    } catch (err) {
      ok = false;
      error = err instanceof Error ? err.message : String(err);
    }

    const result = {
      id: tc.id,
      description: tc.description,
      ok,
      status,
      latencyMs: Math.round((now() - started) * 100) / 100,
      error,
    };
    results.push(result);
    onCaseResult?.(result);
    if (!ok && opts.bail) break;
  }

  const passed = results.filter((r) => r.ok).length;
  return {
    results,
    summary: {
      total: results.length,
      passed,
      failed: results.length - passed,
      durationMs: Math.round(now() - suiteStart),
    },
  };
}
