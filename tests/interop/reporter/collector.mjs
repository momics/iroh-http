/**
 * iroh-http interop collector — request handler + persistence, framework-free.
 *
 * The pure core behind `server.mjs`: a `handler(req) => Response` plus the
 * report-persistence helpers. Kept import-side-effect-free so tests can mount
 * it on a real iroh-http node without spawning the CLI server.
 */

import { mkdir, readdir, stat, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join } from "node:path";

/** Filesystem-safe slug for a report filename. */
export function slug(s) {
  return String(s ?? "unknown").replace(/[^a-zA-Z0-9._-]/g, "_").slice(0, 48);
}

/** Turn one report object into a `<self>-to-<target>-<ts>.json` filename. */
export function reportFilename(report, now = new Date()) {
  const self = report?.self?.platform ??
    (report?.self?.publicKey ? report.self.publicKey.slice(0, 8) : "self");
  const target = report?.target?.platform ??
    (report?.target?.publicKey ? report.target.publicKey.slice(0, 8) : "self");
  const ts = now.toISOString().replace(/[:.]/g, "-");
  return `${slug(self)}-to-${slug(target)}-${ts}.json`;
}

/** Flatten a submitted payload into the individual reports it carries. */
export function extractReports(payload) {
  if (payload && Array.isArray(payload.reports)) return payload.reports;
  if (payload && payload.schema && payload.summary) return [payload];
  // Unknown but object-shaped — still persist it so nothing is lost.
  if (payload && typeof payload === "object") return [payload];
  return [];
}

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/**
 * Build a collector request handler.
 *
 * @param {object} opts
 * @param {string} opts.reportsDir  Directory to write report JSON files into.
 * @param {(msg: string) => void} [opts.log]  Optional log sink.
 * @returns {(req: Request) => Promise<Response>}
 */
export function createHandler({ reportsDir, log = () => {} }) {
  async function handleResults(req) {
    const raw = await req.text();
    let payload;
    try {
      payload = JSON.parse(raw);
    } catch {
      return json({ ok: false, error: "invalid JSON body" }, 400);
    }
    const reports = extractReports(payload);
    if (reports.length === 0) {
      return json({ ok: false, error: "no report(s) in body" }, 422);
    }
    await mkdir(reportsDir, { recursive: true });
    const saved = [];
    // Stamp filenames off a per-request base so a batch never collides on ts.
    const base = Date.now();
    for (let i = 0; i < reports.length; i++) {
      const report = reports[i];
      const name = reportFilename(report, new Date(base + i));
      await writeFile(join(reportsDir, name), JSON.stringify(report, null, 2));
      const s = report?.summary;
      const tag = s ? `${s.pass}/${s.fail}/${s.skip}` : "?";
      log(`saved ${name}  (pass/fail/skip=${tag})`);
      saved.push(name);
    }
    return json({ ok: true, saved }, 201);
  }

  async function handleIndex() {
    let files = [];
    if (existsSync(reportsDir)) {
      const names = (await readdir(reportsDir)).filter((n) =>
        n.endsWith(".json")
      );
      files = await Promise.all(names.map(async (name) => {
        const st = await stat(join(reportsDir, name));
        return { name, size: st.size, mtime: st.mtime.toISOString() };
      }));
      files.sort((a, b) => (a.mtime < b.mtime ? 1 : -1));
    }
    return json({ ok: true, count: files.length, reports: files });
  }

  return async function handler(req) {
    const url = new URL(req.url, "http://localhost");
    const path = url.pathname.replace(/\/+$/, "") || "/";
    if (path === "/health") return new Response("ok", { status: 200 });
    if (path === "/results" && req.method === "POST") {
      try {
        return await handleResults(req);
      } catch (e) {
        log(`error handling POST /results: ${e}`);
        return json({ ok: false, error: String(e) }, 500);
      }
    }
    if (path === "/results" && req.method === "GET") return handleIndex();
    if (path === "/" && req.method === "GET") return handleIndex();
    return json(
      { ok: false, error: `no route for ${req.method} ${path}` },
      404,
    );
  };
}
