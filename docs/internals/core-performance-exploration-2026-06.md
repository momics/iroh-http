# Core Rust performance exploration, 2026-06-30

Branch: `perf/core-rust-exploration`

Goal: improve the core Rust hot path while keeping the ADR-013/ADR-014 shape:
custom code stays in the iroh transport and FFI bridge, while HTTP composition
continues to lean on hyper, tower, tower-http, and moka.

## Ideation summary

Two explorer passes looked at distinct areas:

- HTTP runtime/server path: `http/server`, `http/body.rs`, `http/client.rs`,
  `http/transport`, and Criterion benches.
- FFI/body/session path: `ffi`, `endpoint/ffi_bridge.rs`,
  `endpoint/session_runtime.rs`, and body/channel overhead.

The highest-signal candidates were:

1. Add a `try_send` fast path before `timeout(tx.send(...))` in the body
   channel writer paths.
2. Skip no-op body pump tasks for bodies already known to be EOF.
3. Avoid cloning pooled connection metadata on pool hits.
4. Remove per-request boxed futures in the FFI service or client stack.
5. Rework server stack boxing to box once after composing optional layers.
6. Add pure-Rust HTTP runtime benches to separate runtime cost from FFI cost.

This pass implemented only candidates with localized risk and benchmarked them
before deciding what stayed.

## Kept: body-channel `try_send` fast path

Files:

- `crates/iroh-http-core/src/ffi/handles.rs`

Change:

- `BodyWriter::send_chunk` and `HandleStore::send_chunk` first call
  `mpsc::Sender::try_send`.
- If the channel has capacity, the send completes without constructing a
  timeout future/timer.
- If the channel is full, behavior falls back to the previous
  `tokio::time::timeout(drain_timeout, tx.send(...))` path.
- Closed-reader and timeout error mapping is unchanged.

Benchmark command:

```sh
cargo bench -p iroh-http-core --bench channel channel -- --warm-up-time 1 --measurement-time 3
```

Original baseline vs first changed-run medians:

| Benchmark | Baseline | Changed | Result |
| --- | ---: | ---: | ---: |
| `channel/raw/1024` | 741.94 ns | 699.53 ns | -5.47% |
| `channel/raw/65536` | 733.97 ns | 648.21 ns | -11.36% |
| `channel/raw/1048576` | 739.07 ns | 707.38 ns | -6.91% |
| `channel/handle_store/1024` | 878.40 ns | 830.90 ns | -5.78% |
| `channel/handle_store/65536` | 886.20 ns | 803.80 ns | -9.52% |
| `channel/handle_store/1048576` | 4.7754 us | 3.7334 us | -22.74% |
| `channel/streaming/1mb_as_64kb_chunks` | 12.491 us | 12.003 us | -4.85% |

Criterion reported statistically significant improvements for every row above
with `p = 0.00 < 0.05`.

Final validation after reverting rejected experiments still stayed below the
original baseline medians:

| Benchmark | Original baseline | Final tree | Delta |
| --- | ---: | ---: | ---: |
| `channel/raw/1024` | 741.94 ns | 692.69 ns | -6.64% |
| `channel/raw/65536` | 733.97 ns | 656.78 ns | -10.52% |
| `channel/raw/1048576` | 739.07 ns | 647.97 ns | -12.33% |
| `channel/handle_store/1024` | 878.40 ns | 767.27 ns | -12.65% |
| `channel/handle_store/65536` | 886.20 ns | 807.90 ns | -8.84% |
| `channel/handle_store/1048576` | 4.7754 us | 3.8315 us | -19.77% |
| `channel/streaming/1mb_as_64kb_chunks` | 12.491 us | 11.906 us | -4.68% |

## Rejected: empty-body pump skipping

Prototype:

- In `ffi::dispatcher`, skip spawning `pump_hyper_body_to_channel` when the
  incoming request body reports `is_end_stream()`.
- In `ffi::fetch::package_response`, skip spawning
  `pump_hyper_body_to_channel_limited` when the response body reports
  `is_end_stream()`.

Benchmark commands:

```sh
cargo bench -p iroh-http-core --bench throughput fetch_get_latency -- --warm-up-time 1 --measurement-time 3
cargo bench -p iroh-http-core --bench throughput multiplex -- --warm-up-time 1 --measurement-time 3
```

Observed:

- `fetch_get_latency`: 166.34 us baseline vs 166.83 us changed,
  `p = 0.74`; no performance change detected.
- `multiplex/8`: one run showed -2.75%, but a later final-state run was
  neutral around the same absolute range.
- `multiplex/32`: small changes stayed within noise threshold.

Decision: not kept. The code is plausible, but the benchmark evidence does not
prove a durable improvement.

## Kept: pre-buffered `BodyReader::poll_frame` fast path

Files:

- `crates/iroh-http-core/src/ffi/handles.rs`
- `crates/iroh-http-core/benches/channel.rs`

Change:

- Added `channel/body_reader_http_body/*` Criterion coverage for the path where
  a `BodyReader` is moved into `Body::new(reader)` and consumed by hyper as an
  `http_body::Body`.
- `BodyReader::poll_frame` now checks `try_lock` + `try_recv` before allocating
  the boxed cancellable receive future.
- If a chunk is already buffered, the frame is returned synchronously.
- If the channel is empty or contended, the previous cancellable async path is
  used unchanged.

Benchmark command:

```sh
cargo bench -p iroh-http-core --bench channel body_reader_http_body -- --warm-up-time 1 --measurement-time 3
```

Baseline vs changed medians:

| Benchmark | Baseline | Changed | Result |
| --- | ---: | ---: | ---: |
| `channel/body_reader_http_body/1024` | 895.58 ns | 568.05 ns | -35.69% |
| `channel/body_reader_http_body/65536` | 874.02 ns | 567.80 ns | -34.76% |
| `channel/body_reader_http_body/1048576` | 859.46 ns | 574.28 ns | -33.56% |

Criterion reported statistically significant improvements for all three rows
with `p = 0.00 < 0.05`.

End-to-end `throughput/post_body/*` was also run after the change, but no
before/after baseline existed for that exact group in this exploration pass, so
it is not used as proof. The retained claim is deliberately narrower: the
custom FFI bridge body-reader path is faster when data is already buffered.

## Rejected: returning `Arc<PooledConnection>` from the pool

Prototype:

- Change `ConnectionPool::get_or_connect` and `get_existing` to return
  `Arc<PooledConnection>` instead of cloning `PooledConnection`.

Hypothesis:

- Pool hits would avoid cloning the cached `remote_id_str: String`.

Observed:

- `fetch_get_latency`: Criterion classified the change as noise.
- `multiplex/8`: regressed by about +4.00%.
- `multiplex/32`: regressed by about +3.55%.

Decision: reverted. The extra string clone was not the limiting factor in the
measured path, and the internal API change did not earn its keep.

## Follow-up candidates

These were not implemented in this pass because they need more targeted proof:

- Add a stack-only Criterion benchmark for `build_stack(...).oneshot()` with
  default vs minimal `StackConfig`.
- Investigate client/FFI boxed futures with allocation profiling before adding
  custom future boilerplate.
- Explore server stack optional-layer composition with `ServiceBuilder` and
  ecosystem types, but only after a stack-only bench shows boxing overhead is a
  meaningful part of request time.
- Add pure-Rust `serve_with_events` + `fetch_request` benchmarks next to the
  FFI benches to separate runtime cost from handle-store/JS-bridge cost.
