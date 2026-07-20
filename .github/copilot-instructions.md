# iroh-http

Peer-to-peer HTTP over Iroh QUIC transport. Rust core + FFI adapters for Node.js, Deno, and Tauri. Nodes addressed by Ed25519 public key, not DNS.

Follow the cross-agent repository and Git conventions in
[`AGENTS.md`](../AGENTS.md).

## Principles

These govern every action. No exceptions.

- **Observe** — When you encounter something broken, stale, or inconsistent outside your current task, surface it. Don't silently pass by.
- **Cohere** — Every change must leave the system more connected. New files get linked from their parent index. Renamed things get updated everywhere they're referenced.
- **Verify** — Read before editing. Test after changing. Don't assume prior state.
- **Trace** — Changes connect to reasons. Commits reference issues. Decisions reference evidence.

## Architecture

`iroh-http-core` (Rust) is the single source of truth. It exposes types and functions consumed by three FFI adapters: `iroh-http-node` (napi-rs), `iroh-http-deno` (Deno FFI), `iroh-http-tauri` (Tauri plugin). `iroh-http-shared` is pure TypeScript shared by all JS/TS adapters. `iroh-http-discovery` adds mDNS/DNS peer discovery.

When a type or function changes in core, all adapters must be updated: Rust bridge → TypeScript mapping → re-exported types. A change to `PeerStats` in core means updating `JsPeerStats` in node, the dispatch in deno, the command in tauri, and the type in shared.

Verify with: `cargo test -p iroh-http-core`, `cargo clippy --workspace -- -D warnings`, `npm run typecheck`.

## Reference

Read the relevant doc before acting in that area. Don't read all docs upfront.

- [Principles](../docs/principles.md) — engineering invariants, hierarchy of values. Read before any non-trivial change.
- [Architecture](../docs/architecture.md) — layer diagram, component responsibilities, concurrency model. Read before modifying core.
- [Specification](../docs/specification.md) — normative interface contract for all adapters. Read when adding or changing APIs.
- [Protocol](../docs/protocol.md) — `httpi://` URL scheme, HTTP/1.1-over-QUIC wire format, ALPN versioning.
- [Design decisions](../docs/internals/design-decisions.md) — the *why* behind hyper, slotmap, moka, wire format, compression policy.
- [Roadmap](../docs/roadmap.md) — v1.0 release checklist, open source path.
- [Build & test](../docs/build-and-test.md) — Rust, TypeScript, and E2E test commands. CI pipeline gates.
- [Documentation index](../docs/README.md) — entry point to all docs, features, internals, and recipes.

## Before writing custom HTTP / middleware / body code

`iroh-http-core` composes hyper + tower + tower-http. Custom primitives in this area are almost always a mistake. Before adding any layer, body adapter, error handler, or accept loop:

1. Read [ADR-013 — Lean on the ecosystem](../docs/adr/013-lean-on-the-ecosystem.md) for where custom code is allowed and where it is not.
2. Read [ADR-014 — Runtime architecture](../docs/adr/014-runtime-architecture.md) for the concrete shape (single `Body` newtype, infallible service contract, named seams).
3. Reference implementation: [`axum/src/serve/mod.rs`](https://github.com/tokio-rs/axum/blob/main/axum/src/serve/mod.rs) — ~150 lines that do what our 1098-line `server.rs` does. If you find yourself reinventing pieces of it, stop.

**Stop signal.** If you spend more than ~2 compile iterations fighting tower / hyper / tower-http type or lifetime errors, you are off-pattern. Stop editing, read the equivalent in axum / hyper-util / tower-http examples, and either restructure the wiring or file an issue against the wiring (not the layer).

## Guidelines

- [Rust](../docs/guidelines/rust.md) — naming, visibility, error handling, async, testing for `iroh-http-core` and `iroh-http-discovery`.
- [JavaScript / TypeScript](../docs/guidelines/javascript.md) — platform types, error classes, streaming, serve/fetch contracts.
- [Tauri](../docs/guidelines/tauri.md) — invoke commands, channels, plugin structure for `iroh-http-tauri`.

## Skills

- [manage-issues](../.agents/skills/manage-issues/SKILL.md) — create, triage, update, and close GitHub issues.
- [fix-issues](../.agents/skills/fix-issues/SKILL.md) — systematically resolve open issues through reviewed branches.
- [regression-first](../.agents/skills/regression-first/SKILL.md) — reproduce bugs that escaped a release before fixing them.
- [analyze-git-diff](../.agents/skills/analyze-git-diff/SKILL.md) — generate an evidence-based HTML breakdown of review churn and footprint.
