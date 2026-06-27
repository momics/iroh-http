/**
 * iroh-http Deno benchmark report generator.
 *
 * Runs the same scenarios as bench.ts but with manual timing so we can produce:
 *   - Terminal comparison table (overhead)
 *   - JSON output archived to the bench-results branch (one folder per tag)
 *
 * Usage:
 *   deno run --allow-net --allow-ffi --allow-env --allow-read --allow-write --allow-sys bench/deno/report.ts
 *   deno run ... bench/deno/report.ts --json            # JSON only (for CI)
 *   deno run ... bench/deno/report.ts --json throughput  # JSON filtered to throughput entries
 *   deno run ... bench/deno/report.ts --json latency     # JSON filtered to latency entries
 */

import { createNode } from "../../packages/iroh-http-deno/mod.ts";

// ── Config ────────────────────────────────────────────────────────────────────

const ITERATIONS = 25;
const LARGE_ITERATIONS = 10;
const WARMUP = 3;

// ── Payloads ──────────────────────────────────────────────────────────────────

const SMALL = new TextEncoder().encode('{"ok":true}');

function makePayload(size: number): Uint8Array {
  return new Uint8Array(size).fill(0x61);
}

const PAYLOADS: Record<number, Uint8Array> = {
  0: SMALL,
  1_024: makePayload(1_024),
  65_536: makePayload(64 * 1_024),
  1_048_576: makePayload(1_024 * 1_024),
  10_485_760: makePayload(10 * 1_024 * 1_024),
};

function getPayload(size: number): Uint8Array {
  return PAYLOADS[size] ?? SMALL;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const idx = Math.min(
    sorted.length - 1,
    Math.floor((p / 100) * sorted.length),
  );
  return sorted[idx];
}

async function measure(
  fn: () => Promise<void>,
  iterations: number,
  warmup = WARMUP,
): Promise<{ avg: number; p50: number; p95: number; p99: number }> {
  for (let i = 0; i < warmup; i++) await fn();
  const times: number[] = [];
  for (let i = 0; i < iterations; i++) {
    const t0 = performance.now();
    await fn();
    times.push(performance.now() - t0);
  }
  const sorted = [...times].sort((a, b) => a - b);
  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  return {
    avg,
    p50: percentile(sorted, 50),
    p95: percentile(sorted, 95),
    p99: percentile(sorted, 99),
  };
}

// ── Setup ─────────────────────────────────────────────────────────────────────

const tcpServer = Deno.serve({ hostname: "127.0.0.1", port: 0 }, (req) => {
  const url = new URL(req.url);
  const size = Number(url.searchParams.get("size") ?? 0);
  return new Response(getPayload(size), {
    headers: { "content-type": "application/octet-stream" },
  });
});
const tcpAddr = tcpServer.addr as Deno.NetAddr;
const tcpBase = `http://${tcpAddr.hostname}:${tcpAddr.port}`;

const server = await createNode({
  disableNetworking: true,
  bindAddr: "127.0.0.1:0",
});
const client = await createNode({
  disableNetworking: true,
  bindAddr: "127.0.0.1:0",
});
const { id: serverId, addrs: serverAddrs } = await server.addr();
const serveAbort = new AbortController();
const serveHandle = server.serve({ signal: serveAbort.signal }, (req) => {
  const url = new URL(req.url);
  const size = Number(url.searchParams.get("size") ?? 0);
  return new Response(getPayload(size));
});

// Warm the connection
await client.fetch(`httpi://${serverId}/warmup`, {
  directAddrs: serverAddrs,
});

// ── Run scenarios ─────────────────────────────────────────────────────────────

interface Result {
  scenario: string;
  iroh: { avg: number; p50: number; p95: number; p99: number };
  native: { avg: number; p50: number; p95: number; p99: number };
  unit: string;
}

const results: Result[] = [];

// 1. Cold connect
{
  const iroh = await measure(async () => {
    const fresh = await createNode({
      disableNetworking: true,
      bindAddr: "127.0.0.1:0",
    });
    try {
      const res = await fresh.fetch(`httpi://${serverId}/cold`, {
        directAddrs: serverAddrs,
      });
      await res.arrayBuffer();
    } finally {
      await fresh.close().catch(() => {});
    }
  }, LARGE_ITERATIONS, 1);

  const native = await measure(async () => {
    const res = await fetch(`${tcpBase}/cold`);
    await res.arrayBuffer();
  }, LARGE_ITERATIONS, 1);

  results.push({ scenario: "cold-connect", iroh, native, unit: "ms" });
}

// 2. Warm request
{
  const iroh = await measure(async () => {
    const res = await client.fetch(`httpi://${serverId}/ping`, {
      directAddrs: serverAddrs,
    });
    await res.arrayBuffer();
  }, ITERATIONS);

  const native = await measure(async () => {
    const res = await fetch(`${tcpBase}/ping`);
    await res.arrayBuffer();
  }, ITERATIONS);

  results.push({ scenario: "warm-request", iroh, native, unit: "ms" });
}

// 3–6. Throughput
for (const [label, size, iters] of [
  ["throughput-1kb", 1_024, ITERATIONS],
  ["throughput-64kb", 65_536, ITERATIONS],
  ["throughput-1mb", 1_048_576, ITERATIONS],
  ["throughput-10mb", 10_485_760, LARGE_ITERATIONS],
] as const) {
  const iroh = await measure(async () => {
    const res = await client.fetch(
      `httpi://${serverId}/data?size=${size}`,
      { directAddrs: serverAddrs },
    );
    await res.arrayBuffer();
  }, iters);

  const native = await measure(async () => {
    const res = await fetch(`${tcpBase}/data?size=${size}`);
    await res.arrayBuffer();
  }, iters);

  results.push({ scenario: label, iroh, native, unit: "ms" });
}

// 7–8. Multiplexing
for (const n of [8, 32]) {
  const iroh = await measure(async () => {
    await Promise.all(
      Array.from({ length: n }, () =>
        client
          .fetch(`httpi://${serverId}/ping`, {
            directAddrs: serverAddrs,
          })
          .then((res) => res.arrayBuffer()),
      ),
    );
  }, ITERATIONS);

  const native = await measure(async () => {
    await Promise.all(
      Array.from({ length: n }, () =>
        fetch(`${tcpBase}/ping`).then((res) => res.arrayBuffer()),
      ),
    );
  }, ITERATIONS);

  results.push({ scenario: `multiplex-x${n}`, iroh, native, unit: "ms" });
}

// 9. Serve req/s (same as warm request — measures single-request serve throughput)
{
  const iroh = await measure(async () => {
    const res = await client.fetch(`httpi://${serverId}/ping`, {
      directAddrs: serverAddrs,
    });
    await res.arrayBuffer();
  }, ITERATIONS);

  const native = await measure(async () => {
    const res = await fetch(`${tcpBase}/ping`);
    await res.arrayBuffer();
  }, ITERATIONS);

  results.push({ scenario: "serve-rps", iroh, native, unit: "ms" });
}

// ── Output ────────────────────────────────────────────────────────────────────

const args = Deno.args;
const jsonMode = args.includes("--json");
const filter = args.find((a) => a !== "--json" && !a.startsWith("--"));

if (jsonMode) {
  // benchmark-action JSON format: [{name, unit, value}]
  const entries: Array<{ name: string; unit: string; value: number }> = [];

  for (const r of results) {
    // Throughput entries: bigger is better (convert ms → MB/s)
    if (r.scenario.startsWith("throughput-")) {
      const sizeMatch = r.scenario.match(/throughput-(\d+)(kb|mb)/);
      if (sizeMatch) {
        const num = Number(sizeMatch[1]);
        const mult = sizeMatch[2] === "mb" ? 1_048_576 : 1_024;
        const bytes = num * mult;
        const irohMbps = bytes / (1024 * 1024) / (r.iroh.avg / 1000);
        const nativeMbps = bytes / (1024 * 1024) / (r.native.avg / 1000);
        entries.push({
          name: `deno/iroh/${r.scenario}`,
          unit: "MB/s",
          value: irohMbps,
        });
        entries.push({
          name: `deno/native/${r.scenario}`,
          unit: "MB/s",
          value: nativeMbps,
        });
      }
    } else {
      // Latency entries: smaller is better (µs)
      entries.push({
        name: `deno/iroh/${r.scenario}`,
        unit: "us",
        value: r.iroh.avg * 1000,
      });
      entries.push({
        name: `deno/native/${r.scenario}`,
        unit: "us",
        value: r.native.avg * 1000,
      });
    }
  }

  let filtered = entries;
  if (filter === "throughput") {
    filtered = entries.filter((e) => e.name.includes("throughput"));
  } else if (filter === "latency") {
    filtered = entries.filter((e) => !e.name.includes("throughput"));
  }

  console.log(JSON.stringify(filtered, null, 2));
} else {
  // Terminal comparison table
  const version =
    Deno.env.get("npm_package_version") ??
    Deno.env.get("IROH_VERSION") ??
    "dev";
  const runtime = `Deno ${Deno.version.deno}`;

  console.log();
  console.log(
    `iroh-http benchmark results (v${version}, ${runtime}, ${Deno.build.arch})`,
  );
  console.log();

  const hdr = `  ${"Scenario".padEnd(24)} ${"iroh".padStart(12)} ${"native".padStart(12)} ${"overhead".padStart(12)}  ${"p50".padStart(10)} ${"p95".padStart(10)} ${"p99".padStart(10)}`;
  console.log(hdr);
  console.log("  " + "─".repeat(hdr.length - 2));

  for (const r of results) {
    const fmt = (ms: number) => {
      if (ms >= 1000) return `${(ms / 1000).toFixed(2)} s`;
      if (ms >= 1) return `${ms.toFixed(2)} ms`;
      return `${(ms * 1000).toFixed(1)} µs`;
    };

    const overhead = `${(r.iroh.avg / r.native.avg).toFixed(1)}x`;

    console.log(
      `  ${r.scenario.padEnd(24)} ${fmt(r.iroh.avg).padStart(12)} ${fmt(r.native.avg).padStart(12)} ${overhead.padStart(12)}  ${fmt(r.iroh.p50).padStart(10)} ${fmt(r.iroh.p95).padStart(10)} ${fmt(r.iroh.p99).padStart(10)}`,
    );
  }
  console.log();
}

// ── Teardown ──────────────────────────────────────────────────────────────────

serveAbort.abort();
await serveHandle.finished.catch(() => {});
await server.close().catch(() => {});
await client.close().catch(() => {});
await tcpServer.shutdown().catch(() => {});
