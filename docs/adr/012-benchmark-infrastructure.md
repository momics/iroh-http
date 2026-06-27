---
id: "012"
title: "Benchmark infrastructure"
status: resolved
date: 2026-04-25
area: testing
tags: [benchmarks, performance, deno, node, rust, criterion, ci]
---

# [012] Benchmark infrastructure

## Context

Performance benchmarks exist for all three layers (Rust/Criterion,
Node/Mitata, Deno/`Deno.bench`) and a CI workflow (`bench.yml`) archives
results to the long-lived `bench-results` branch (one folder per tag). The
benchmarks serve two purposes:

1. **Regression tracking** — per-release tag runs detect >20% regressions.
2. **Overhead comparison** — side-by-side iroh vs native HTTP numbers show
   users the cost of peer-to-peer transport.

## Decision

Redesigned the benchmark suite with 9 consistent scenarios across all three
layers. Each scenario measures both iroh and a native HTTP baseline.

### Scenarios

| # | Scenario | Body | Iterations | Measures |
|---|----------|------|------------|----------|
| 1 | Cold connect | ~100B | 10 | QUIC handshake + first response |
| 2 | Warm request | ~100B | 25 | Hot-path fetch/serve latency |
| 3 | Throughput 1KB | 1KB | 25 | Small payload round-trip |
| 4 | Throughput 64KB | 64KB | 25 | Medium payload |
| 5 | Throughput 1MB | 1MB | 25 | Large payload |
| 6 | Throughput 10MB | 10MB | 10 | Blob transfer |
| 7 | Multiplex ×8 | ~100B | 25 | QUIC stream multiplexing |
| 8 | Multiplex ×32 | ~100B | 25 | High-concurrency multiplexing |
| 9 | Serve req/s | ~100B | 25 | Server handler throughput |

### File structure

```
bench/
  deno/
    bench.ts          # Deno.bench — all 9 scenarios
    report.ts         # terminal table + JSON for CI
  node/
    bench.mjs         # mitata — all 9 scenarios
    report.mjs        # terminal table + JSON for CI
  scripts/
    normalize-criterion-json.mjs   # Criterion → benchmark-action JSON
  results/
    .gitkeep
```

### Report output

Terminal table (from `report.ts` / `report.mjs`):

```
iroh-http benchmark results (vX.Y.Z, Deno 2.7.12, aarch64)

  Scenario                    iroh       native     overhead      p50        p95        p99
  ─────────────────────────────────────────────────────────────────────────────────────────
  cold-connect            92.63 ms     237.9 µs      389.4x   93.22 ms   96.71 ms   96.71 ms
  warm-request             2.29 ms     150.8 µs       15.2x    2.08 ms    2.89 ms    2.92 ms
  ...
```

JSON output: `[{name, unit, value}]` with names like
`deno/iroh/warm-request`, `node/native/throughput-1mb`.

### CI triggers

- `push` to `v*.*.*` tags — per-release snapshot committed to
  `bench-results/data/<tag>`
- `workflow_dispatch` (no input) — manual snapshot committed to
  `bench-results/data/main-<sha>`
- `workflow_dispatch` (tag=`vX.Y.Z`) — re-benchmark an existing release using
  that tag's own harness and overwrite `bench-results/data/<tag>`

Results are recorded, not gated: there is no dashboard and no regression
enforcement. Any visualisation can be regenerated on demand from the
committed JSON.

### Rust Criterion additions

- 10MB body size added to `post_body_throughput_bytes` and
  `response_body_streaming_bytes` groups (sample_size=10).
- New `multiplex` benchmark group (×8, ×32 concurrent fetches).

## Implications

- Deno benchmarks now run cleanly (FFI bridge issues from #009 resolved).
- All three jobs (Rust, Node, Deno) are active in `bench.yml`.
- Raw results live on the `bench-results` branch; GitHub Pages is not used
  (the gh-pages dashboard was dropped in #240).
- Self-hosted runner (`vars.BENCHMARK_RUNNER`) falls back to `ubuntu-latest`.
- Alert threshold is 120% (>20% regression fails the job and comments).

## Next steps

- [ ] Verify the self-hosted benchmark runner is operational. If not, decide
      whether to use `ubuntu-latest` with statistical smoothing or defer
      until the runner is available.
- [ ] Consider publishing overhead comparison tables to the README, generated
      on demand from the committed `bench-results` JSON.
- [ ] Add raw session benchmarks once WebTransport API stabilises.
