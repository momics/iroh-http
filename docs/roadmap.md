# Roadmap

## Current state — v0.7.x (testing phase)

The library is functionally complete and published on npm and JSR, but we are
in an active testing phase before calling anything "stable". Semver signals
this: all packages are `0.x`. Breaking changes are possible.

### What is shipped and working

- [x] crates.io publish metadata (`repository`, `documentation`, `keywords`, `categories`)
- [x] Guarded release pipeline (tagged Build, Extended, and Benchmarks workflows;
  manual OIDC-backed Publish promotion)
- [x] CI workflow (`ci.yml` — Rust check, TypeScript typecheck, E2E tests)
- [x] Clean repository state (`.gitignore` for workspace/, .obsidian/, generated files)
- [x] Issue templates (`bug.yml`, `feature.yml`)
- [x] Cross-platform builds: Node (5 targets), Deno (5 targets)
- [x] Build logic lives in each package (not root shell scripts)
- [x] `CHANGELOG.md` — generated via `git-cliff`, reviewed in-repo, and reused
  verbatim for GitHub release notes
- [x] `SECURITY.md` — GitHub Security Advisories disclosure policy
- [x] Repository public on GitHub
- [x] Planned release work through v0.6.1 shipped
- [x] Node.js — napi-rs platform packages + `optionalDependencies` wiring (all 5 targets)
- [x] `node.stats()` — node-wide `EndpointStats` (active connections, requests, handles, pool size)
- [x] Transport events — `pool:hit`, `pool:miss`, `pool:evict`, `path:change`, `handle:sweep`
- [x] `node.peerStats(nodeId)` — per-peer QUIC stats (RTT, bytes, path info)
- [x] `node.pathChanges(nodeId)` — `AsyncIterable<PathInfo>` path-change stream
- [x] Per-peer rate limiting (`maxConnectionsPerPeer`) at the QUIC layer
- [x] Connection pool with moka-backed single-flight establishment
- [x] Optional per-fetch `connectTimeout` with typed cross-adapter errors
- [x] Iroh 1.4.1 runtime and security dependency refresh
- [x] QPACK header compression (zstd, feature-gated)
- [x] WebTransport-compatible `IrohSession` (bidirectional streams, datagrams, close info)
- [x] Node tickets — compact shareable `NodeAddr` strings
- [x] mDNS peer discovery (`browse` / `advertise`)
- [x] Graceful shutdown with drain semantics
- [x] `fetch()` accepts `httpi://` URLs with peer ID in hostname

### Distribution channels

| Package | Platform | Status |
|---------|----------|--------|
| `@momics/iroh-http-shared` | npm + JSR | ✅ Published |
| `@momics/iroh-http-node` | npm (5 platform packages) | ✅ Published |
| `@momics/iroh-http-deno` | JSR + GitHub releases | ✅ Published, runtime binary download |
| `@momics/iroh-http-tauri` | npm | ✅ Published |
| `iroh-http-core` | crates.io | ✅ Metadata complete |
| `iroh-http-discovery` | crates.io | ✅ Metadata complete |

---

## Near-term — stabilisation and polish

These are the most valuable things to work on while the testing phase runs.

### Runnable examples

The `examples/` folder exists but is sparse. Each runtime needs at least one
self-contained, runnable example demonstrating `fetch` + `serve` + `session`
in a realistic scenario:

- `examples/deno/` — basic request/response, then session with datagrams
- `examples/node/` — same, plus a mDNS discovery demo
- `examples/tauri/` — minimal Tauri app showing the Rust ↔ TS bridge in action

Good examples are the fastest path from "curious" to "shipping".

### `iroh-path-type` response header

The observability spec documents a planned `iroh-path-type` response header
(indicating direct vs relay path for each request). It is not yet injected by
the server layer. The blocker is that iroh does not yet expose stable
per-connection path metadata in its public API. Track upstream; add the header
once the API stabilises.

## Horizon 2 — HTTP/3

Nothing in the current architecture closes the door to HTTP/3. The
`tower::Service` application layer and all business logic would be unchanged
— only the transport wiring needs to swap.

The blocker is upstream: there is no `h3-noq` crate yet (analogous to
`h3-quinn` but for Iroh's noq fork). Once that exists and Iroh exposes
`noq::Connection` publicly, the swap is straightforward. Track this via
the open question in [architecture.md](architecture.md).

---

## Design constraints to maintain

These govern future work and must not be violated even for expedient changes:

- Wire-level protocol behaviour must remain expressible as conformance tests —
  it must not be implicitly defined by runtime behaviour.
- Platform adapters must not define protocol behaviour; they only map APIs.
- Error codes and failure semantics must be canonical and cross-platform.
- The architecture must not close the door to future language bindings (e.g.
  Python). This means keeping the Rust core's public API clean and
  self-contained — no tokio or hyper types leaking into the FFI boundary.
