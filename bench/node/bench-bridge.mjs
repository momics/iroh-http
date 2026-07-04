/**
 * iroh-http Node.js FFI bridge benchmark — isolates Rust↔JS data crossing.
 *
 * Measures the overhead of sendChunk/nextChunk across the napi-rs boundary
 * WITHOUT any QUIC transport — uses a local body channel loopback.
 *
 * Scenarios:
 *   1. sendChunk + nextChunk round-trip (1 KB, 64 KB, 1 MB)
 *   2. Streaming throughput: 1 MB as 16 × 64 KB chunks
 *   3. allocBodyWriter + finishBody lifecycle
 *   4. Chunk-size sweep: 10 MiB streaming body at 64K / 256K / 1M chunk sizes,
 *      reporting per-size throughput, CPU, and p50 (see issue #287 / perf-4).
 *
 * Run: node bench/node/bench-bridge.mjs
 */

import { bench, group, run } from "mitata";
import {
  closeEndpoint,
  createEndpoint,
  jsAllocBodyWriter,
  jsFinishBody,
  jsSendChunk,
} from "../../packages/iroh-http-node/index.js";

// ── Payloads ──────────────────────────────────────────────────────────────────

function payload(size) {
  return new Uint8Array(size).fill(0x42);
}

// ── Sweep reporting ───────────────────────────────────────────────────────────

/** Median of an array of numbers (does not mutate the input). */
function percentile(samples, p) {
  const sorted = [...samples].sort((a, b) => a - b);
  const idx = Math.min(
    sorted.length - 1,
    Math.floor((p / 100) * sorted.length),
  );
  return sorted[idx];
}

function reportSweep(rows) {
  if (rows.length === 0) return;
  const mib = SWEEP_BODY_BYTES / (1024 * 1024);

  const base = rows.find((r) => r.label === "64k") ?? rows[0];
  const best = rows.reduce((a, b) => (b.p50Ms < a.p50Ms ? b : a));
  const improvementPct = ((base.p50Ms - best.p50Ms) / base.p50Ms) * 100;

  console.log("\n" + "=".repeat(84));
  console.log(
    `Chunk-size sweep — ${mib} MiB send-only streaming body (#287 / perf-4)`,
  );
  console.log(
    `(${SWEEP_MEASURE} measured iters/size after ${SWEEP_WARMUP} warmup; ` +
      `default 64 KiB core max_chunk_size, so channel pushes are constant across sizes)`,
  );
  console.log("=".repeat(84));
  const hdr = [
    "chunk",
    "calls",
    "p50(ms)",
    "p99(ms)",
    "MiB/s",
    "us/call",
    "CPU us/MiB",
  ];
  console.log(hdr.map((h) => h.padStart(12)).join(""));
  for (const r of rows) {
    const mibs = mib / (r.p50Ms / 1000);
    console.log(
      [
        r.label,
        String(r.calls),
        r.p50Ms.toFixed(3),
        r.p99Ms.toFixed(3),
        mibs.toFixed(1),
        r.perCallUs.toFixed(2),
        r.cpuUsPerMiB !== undefined ? r.cpuUsPerMiB.toFixed(1) : "n/a",
      ]
        .map((c) => String(c).padStart(12))
        .join(""),
    );
  }
  console.log("-".repeat(84));
  console.log(
    `baseline (64k): p50 ${base.p50Ms.toFixed(3)} ms | ` +
      `best (${best.label}): p50 ${best.p50Ms.toFixed(3)} ms`,
  );
  console.log(
    `best-vs-64KiB p50 improvement: ${improvementPct.toFixed(1)}%  ` +
      `(gate >=15%: ${improvementPct >= 15 ? "PASS ✅" : "FAIL ❌"})`,
  );
  console.log("=".repeat(84) + "\n");
}

const PAYLOAD_1K = payload(1_024);
const PAYLOAD_64K = payload(64 * 1_024);
const PAYLOAD_1M = payload(1_024 * 1_024);

// ── Chunk-size sweep config (#287 / perf-4) ───────────────────────────────────
// A 10 MiB streaming body is chunked by the TS `pipeToWriter` at a hard-coded
// 64 KiB, producing ~161 `sendChunk` bridge crossings. This sweep isolates how
// larger JS-side chunk sizes (fewer sendChunk crossings) affect send-side bridge
// throughput/CPU. It mirrors pipeToWriter's "subarray the source buffer, send
// each slice" loop, but WITHOUT touching production code.
//
// Note: core `send_chunk` splits anything larger than max_chunk_size (default
// 64 KiB) internally, so the number of channel pushes and bytes copied is
// identical across all three sizes — the ONLY variable is the count of
// JS→native sendChunk crossings (160 vs 40 vs 10). That is exactly the cost
// perf-4 is asking about.
//
// We buffer the whole body (no reader drain) by sizing the endpoint channel
// large enough, so this measures pure send-path latency — matching pipeToWriter,
// which only writes and does not read the response body in the same loop.
const SWEEP_BODY_BYTES = 10 * 1_024 * 1_024; // 10 MiB
const SWEEP_CHUNK_SIZES = [
  ["64k", 64 * 1_024],
  ["256k", 256 * 1_024],
  ["1m", 1_024 * 1_024],
];
const SWEEP_BODY = payload(SWEEP_BODY_BYTES);
const SWEEP_WARMUP = 5; // discarded warmup iterations per chunk size
const SWEEP_MEASURE = 25; // measured iterations per chunk size
// Channel capacity (in chunks) large enough to buffer the whole body without a
// reader: 10 MiB / 64 KiB = 160 pushes; round up with headroom.
const SWEEP_CHANNEL_CAPACITY = 512;

// ── Setup — use raw napi bindings, no IrohNode wrapper ────────────────────────

const info = await createEndpoint({
  disableNetworking: true,
  bindAddrs: ["127.0.0.1:0"],
  channelCapacity: SWEEP_CHANNEL_CAPACITY,
});
const eh = info.endpointHandle;

try {
  // ── 1. Round-trip: allocBodyWriter → sendChunk → finishBody ────────────────

  for (
    const [label, data] of [
      ["1kb", PAYLOAD_1K],
      ["64kb", PAYLOAD_64K],
      ["1mb", PAYLOAD_1M],
    ]
  ) {
    group(`bridge-roundtrip-${label}`, () => {
      bench(`bridge-roundtrip-${label}`, async () => {
        const writerHandle = jsAllocBodyWriter(eh);
        await jsSendChunk(eh, BigInt(writerHandle), data);
        jsFinishBody(eh, BigInt(writerHandle));
      });
    });
  }

  // ── 2. Streaming: 1 MB as 16 × 64 KB chunks ──────────────────────────────

  group("bridge-streaming-1mb", () => {
    bench("bridge-streaming-1mb", async () => {
      const writerHandle = jsAllocBodyWriter(eh);
      const bh = BigInt(writerHandle);
      for (let i = 0; i < 16; i++) {
        await jsSendChunk(eh, bh, PAYLOAD_64K);
      }
      jsFinishBody(eh, bh);
    });
  });

  // ── 3. Handle lifecycle: alloc + finish ───────────────────────────────────

  group("bridge-alloc-free", () => {
    bench("bridge-alloc-free", () => {
      const writerHandle = jsAllocBodyWriter(eh);
      jsFinishBody(eh, BigInt(writerHandle));
    });
  });

  // ── 4. Chunk-size sweep: 10 MiB body at 64K / 256K / 1M ───────────────────
  //
  // Manual fixed-iteration harness (not mitata): we control the iteration count
  // so buffered-but-undrained bodies don't accumulate unboundedly, and we
  // compute p50/p99/CPU directly. Streams the same 10 MiB buffer, sliced into
  // fixed-size chunks, once per iteration — mirroring pipeToWriter's send loop.

  async function sendBody(chunkBytes) {
    const writerHandle = jsAllocBodyWriter(eh);
    const bh = BigInt(writerHandle);
    for (let offset = 0; offset < SWEEP_BODY.byteLength; offset += chunkBytes) {
      const end = Math.min(offset + chunkBytes, SWEEP_BODY.byteLength);
      await jsSendChunk(eh, bh, SWEEP_BODY.subarray(offset, end));
    }
    jsFinishBody(eh, bh);
  }

  // Run mitata scenarios 1-3 first.
  await run();

  const sweepRows = [];
  const mib = SWEEP_BODY_BYTES / (1024 * 1024);
  for (const [label, chunkBytes] of SWEEP_CHUNK_SIZES) {
    const calls = Math.ceil(SWEEP_BODY_BYTES / chunkBytes);

    for (let i = 0; i < SWEEP_WARMUP; i++) await sendBody(chunkBytes);

    const latenciesMs = [];
    const cpuStart = process.cpuUsage();
    for (let i = 0; i < SWEEP_MEASURE; i++) {
      const t0 = performance.now();
      await sendBody(chunkBytes);
      latenciesMs.push(performance.now() - t0);
    }
    const cpu = process.cpuUsage(cpuStart); // microseconds (user + system)

    const p50Ms = percentile(latenciesMs, 50);
    const p99Ms = percentile(latenciesMs, 99);
    const totalMiB = mib * SWEEP_MEASURE;
    sweepRows.push({
      label,
      chunkBytes,
      calls,
      p50Ms,
      p99Ms,
      perCallUs: (p50Ms * 1000) / calls,
      cpuUsPerMiB: (cpu.user + cpu.system) / totalMiB,
    });
  }

  // ── 5. Chunk-size sweep summary + >=15% p50 gate ──────────────────────────

  reportSweep(sweepRows);
} finally {
  await closeEndpoint(eh).catch(() => {});
}
