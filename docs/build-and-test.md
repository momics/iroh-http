# Build & Test

Commands for building and testing iroh-http locally and in CI.

---

## Rust

```sh
cargo check --workspace              # typecheck all crates
cargo test --workspace               # run all unit + integration tests
cargo clippy --workspace -- -D warnings   # lint (warnings are errors)
cargo fmt --all -- --check           # format check
```

The Tauri plugin lives outside the workspace and is checked separately:

```sh
cd packages/iroh-http-tauri && cargo check
```

No-default-features checks verify the code compiles without optional
dependencies (e.g. discovery):

```sh
cargo check -p iroh-http-node --no-default-features
cargo check -p iroh-http-deno --no-default-features
```

---

## Build performance

Rust compile times dominate the local CI loop (`npm run ci`). The
following optional tools speed up edit→compile→test cycles substantially.
None are required; the build works without them.

### Incremental builds and linkers

The repo ships a `.cargo/config.toml` with `incremental = true` enabled
for the `dev` profile and commented-out `[target.*]` recipes for faster
linkers. Uncomment the block matching your host:

- **Linux (x86_64)** — install [`mold`](https://github.com/rui314/mold)
  (`apt install mold` or `brew install mold`) and uncomment the
  `x86_64-unknown-linux-gnu` block. Falls back to `lld` if preferred.
- **macOS (Apple Silicon)** — install LLVM (`brew install llvm`) and
  uncomment the `aarch64-apple-darwin` block to use `lld`.
- **macOS (Intel)** — same as above with the `x86_64-apple-darwin` block.

Linker swaps typically cut incremental link times from seconds to
milliseconds.

### sccache

[`sccache`](https://github.com/mozilla/sccache) caches `rustc` outputs
across branches and `cargo clean` cycles:

```sh
brew install sccache       # or: cargo install sccache
export RUSTC_WRAPPER=sccache
```

Add the export to your shell profile to make it persistent. First clean
build is unchanged; subsequent rebuilds reuse cached artifacts.

### cargo nextest

[`cargo-nextest`](https://nexte.st) runs the test suite in parallel with
better output and faster test-process startup:

```sh
cargo install cargo-nextest --locked
```

`npm run test:rust` and `npm run test:tauri` automatically prefer
`cargo nextest run` when it is on `PATH`, falling back to `cargo test`
otherwise. No further configuration is needed.

---

## TypeScript

```sh
npm install                          # install workspace dependencies
npm run typecheck                    # tsc --noEmit across shared + adapters
```

---

## Integration Tests

Rust integration tests in `crates/iroh-http-core/tests/` exercise two real
Iroh nodes over real QUIC connections (no mocks, no stubs). These cover:

- Basic fetch/serve round-trips
- Request/response bodies
- Streaming
- Bidirectional streams and WebTransport sessions
- Timeouts and diagnostics
- Cryptographic sign/verify
- Node ticket parsing

---

## Property Tests

Property-based tests live in `crates/iroh-http-core/tests/property.rs` and
run as part of `cargo test --workspace`. They use
[proptest](https://crates.io/crates/proptest) to generate thousands of random
inputs and verify that every public API boundary either succeeds or returns an
error — never panics.

Coverage by module:

| Module | Invariants |
|--------|------------|
| `lib.rs` — parsers | `parse_node_addr` never panics on arbitrary strings and JSON tickets |
| `lib.rs` — base32 | `base32_encode` never panics, empty input → empty output |
| `lib.rs` — crypto | sign→verify roundtrip, `secret_key_sign` / `public_key_verify` never panic on arbitrary keys |
| `lib.rs` — errors | `core_error_to_json` / `format_error_json` always produce valid JSON |
| `endpoint.rs` | `parse_direct_addrs` never panics |
| `server.rs` | `respond` never panics on arbitrary status codes + headers |
| `stream.rs` | HandleStore capacity bounds, invalid-handle safety, reader/writer insert→cancel roundtrip, pending-reader store→claim roundtrip |

When adding a new `pub fn` to the crate, add a corresponding
`_never_panics` proptest so every entry point has at least a basic
contract test.

---

## Fuzz Testing

Fuzz targets live in `crates/iroh-http-core/fuzz/` and use
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer).
They require a **nightly** Rust toolchain.

### Targets

| Target | Entry point | What it exercises |
|--------|-------------|-------------------|
| `fuzz_parse_node_addr` | `parse_node_addr()` | Arbitrary strings through JSON/base32/socket-addr parsing |
| `fuzz_handle_store` | `HandleStore` lookups | Arbitrary `u64` handles on every take/cancel/finish/lookup path |
| `fuzz_respond` | `respond()` | Arbitrary status codes + header name/value pairs through validation |

### Running locally

```sh
# Install cargo-fuzz (once):
cargo install cargo-fuzz

# Run a target for 5 minutes:
cd crates/iroh-http-core
cargo +nightly fuzz run fuzz_respond -- -max_total_time=300 -max_len=1024

# List available targets:
cargo +nightly fuzz list
```

### Seed corpus

Seed inputs for the `respond` target are in
`crates/iroh-http-core/fuzz/corpus/fuzz_respond/`. The fuzzer reads these
as starting points, mutates them, and saves any new inputs that reach new
code paths back into the corpus directory.

### Nightly CI

A separate GitHub Actions workflow
([`.github/workflows/fuzz.yml`](../.github/workflows/fuzz.yml)) runs all
three fuzz targets nightly (5 minutes each, in parallel). The corpus is
cached between runs so discoveries accumulate over time.

---

## End-to-End Tests

E2E tests for platform adapters require building the native library first:

### Node.js

```sh
cargo build --release -p iroh-http-node
cd packages/iroh-http-node
npx napi build --platform --release
npx tsc
node test/e2e.mjs
```

### Deno

```sh
cargo build --release -p iroh-http-deno
mkdir -p packages/iroh-http-deno/lib
# Copy the native lib for your platform (example: Linux x86_64)
cp target/release/libiroh_http_deno.so packages/iroh-http-deno/lib/libiroh_http_deno.linux-x86_64.so
deno test --allow-read --allow-ffi --allow-env --allow-net packages/iroh-http-deno/test/smoke.test.ts
```

---

## Benchmarks

Benchmark suites exist for every runtime:

```sh
# Node.js (mitata)
npm run bench:node

# Deno (Deno.bench)
npm run bench:deno

# Rust core (Criterion)
npm run bench:rust
```

CI runs `.github/workflows/bench.yml` on pushes to `main` and on release tags.
It stores benchmark history via `benchmark-action` and fails when regressions
exceed the 20% slowdown threshold (`alert-threshold: 120%`).

---

## CI

CI runs on every push to `main` and every pull request. All of the following
must pass:

1. `cargo check --workspace` + tauri plugin
2. `cargo test --workspace`
3. `cargo fmt --all -- --check`
4. `cargo clippy --workspace -- -D warnings`
5. Security-focused clippy lints (`unwrap_used`, `panic`, `arithmetic_side_effects`)
6. No-default-features check (Node + Deno)
7. TypeScript typecheck (`npm run typecheck`)
8. Dependency auditing (`cargo audit`, `cargo deny`, `npm audit --audit-level=high`)
9. Node.js E2E tests + compliance tests (12 cases)
10. Deno E2E tests
11. Cross-runtime compliance (node↔deno via `tests/http-compliance/run.sh`)
12. PR dependency review (`actions/dependency-review-action`)
13. Mobile discovery FFI contract — string-parity test + native compile
    (PR-gated; see below)

See [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) for the full
pipeline. Fuzz + sanitizer/miri hardening runs on a separate nightly schedule
— see [`.github/workflows/fuzz.yml`](../.github/workflows/fuzz.yml).

### Mobile discovery: what CI covers vs. manual on-device testing

The Tauri plugin's mobile discovery commands cross a Rust ↔ Swift/Kotlin FFI
boundary that is matched **by string at runtime** (`run_mobile_plugin("name")`
in [`mobile_mdns.rs`](../packages/iroh-http-tauri/src/mobile_mdns.rs) dispatches
to `@objc func name` in `IrohHttpPlugin.swift` and `@Command fun name` in
`IrohHttpPlugin.kt`). CI closes the gap on the **contract/wiring**; the
**behavior** still needs a device on a real LAN.

**Covered by CI** (deterministic, no device — issue #333):

| Check | Where | Runner |
|-------|-------|--------|
| String-parity of command names across Rust/Swift/Kotlin (both directions) | `cargo test` in the `verify` job (`tests::ffi_contract` in `packages/iroh-http-tauri/src/tests.rs`) | ubuntu |
| iOS Swift plugin **compiles** against the Tauri iOS API | `Compile iOS Swift plugin` step, `rust-check-macos` job (`scripts/ci-ios-swift-build.sh`) | macOS |
| Android Kotlin plugin **compiles** against the Tauri Android API | `mobile-android` job (`scripts/ci-android-gradle-build.sh`) | ubuntu |

The parity test and both compile jobs are build-only: no simulator/emulator, no
NDK, and no Rust mobile cross-compile — they materialize the Tauri mobile API
that the crate's `Cargo.lock` already resolves and compile the native sources
against it. The two compile jobs are gated to `pull_request` and tag builds
(like the other macOS/Windows jobs), so a native contract break blocks merge.

**NOT covered by CI — requires manual on-device verification** (tracked in
issue #334): real multicast mDNS behavior over a LAN — advertise, browse,
self-echo suppression, TXT/`pk` resolution, and endpoint-resolution timing.
A green pipeline proves the symbols line up and the native sources build; it
does **not** prove discovery works on a phone.

To run the mobile contract checks locally:

```sh
# String-parity contract test (no toolchain beyond Rust):
cargo test --manifest-path packages/iroh-http-tauri/Cargo.toml ffi_contract

# iOS Swift compile (macOS + Xcode):
bash packages/iroh-http-tauri/scripts/ci-ios-swift-build.sh

# Android Kotlin compile (JDK 17 + Android SDK + Gradle):
bash packages/iroh-http-tauri/scripts/ci-android-gradle-build.sh
```

### Running compliance tests locally

```sh
# Node compliance (cases.json fixture):
node packages/iroh-http-node/test/compliance.mjs

# Cross-runtime — requires both Node and Deno native libs built:
bash tests/http-compliance/run.sh
```
