---
id: "013"
title: "Lean on the ecosystem: custom code only where it earns its place"
status: accepted
date: 2026-04-29
area: ecosystem
tags: [hyper, tower, axum, refactor, principles, agents]
---

# [013] Lean on the ecosystem: custom code only where it earns its place

## Context

Principle #3 in [`docs/principles.md`](../principles.md) — *Leverage, don't reinvent* — already states that we prefer well-maintained libraries over custom code. In practice, however, two recent issues escaped the spirit of that principle by composing the ecosystem **incorrectly** rather than by re-implementing it:

- [#147 — server-side compression refactor](https://github.com/Momics/iroh-http/issues/147) ballooned because the existing per-connection wiring in `crates/iroh-http-core/src/server.rs` carried multiple incompatible body and error types through the tower stack.
- [#153 — inbound request body decompression](https://github.com/Momics/iroh-http/issues/153) was estimated at one hour and stalled in a multi-hour Rust type-system fight against the same wiring. The compiler kept rejecting the standard `tower_http::decompression::RequestDecompressionLayer` because it expected a single `Body` newtype and an infallible service contract that we never gave it.

The pattern: a contributor (human or AI agent) reaches for a battle-tested layer, the type system pushes back, and the work drifts into reinventing pieces of axum (a custom error-to-response handler, per-request body juggling) rather than restructuring the wiring to match what the ecosystem expects.

Principle #3 speaks to *whether* to use the ecosystem. ADR-013 speaks to *how* to compose it, where bespoke code is allowed, and what the **stop signal** is when an attempt is going off-pattern. ADR-014 (the runtime architecture) is the concrete shape that satisfies this rule.

## Questions

1. Where is custom code legitimately allowed inside `iroh-http-core`?
2. What heuristic tells a contributor — within ~2 compile iterations — that they are off-pattern and should step back?
3. How is this rule made discoverable to AI agents who do not read the full principles file by default?

## What we know

- The repository's coding philosophy already names the libraries it leans on: hyper v1, tower 0.5, tower-http 0.6, moka, slotmap, tokio. These choices are recorded in [`docs/internals/design-decisions.md`](../internals/design-decisions.md) and the `Leverage, don't reinvent` table in `principles.md`.
- Axum's `serve.rs` runs the equivalent of our 1,098-line `crates/iroh-http-core/src/server.rs` in roughly 150 lines because it commits to a single `Body` newtype and an infallible service contract at the hyper boundary. ADR-014 §D1 mirrors that shape.
- Custom code that is *genuinely* required by iroh-http (and has no ecosystem equivalent):
  - the Iroh transport adapter that bridges an Iroh `Connection` to `AsyncRead` / `AsyncWrite` and extracts peer identity (no other QUIC library exposes the iroh ALPN multiplexing plus peer pubkey);
  - the FFI dispatch service — the single tower `Service` that bridges Rust requests/responses into JS handles and back-channels;
  - the public JS-facing types (`fetch`, `serve`, `Session`) which must mirror WHATWG and WebTransport — see *Belong to the Platform* in `principles.md`.
- Anywhere else, ecosystem code exists.

## Decisions

### D1 — Where custom code is allowed

Custom code in `crates/iroh-http-core` and the FFI adapters is allowed **only** in:

1. The **Iroh transport adapter** — converting Iroh `Connection` and `BiStream` into `AsyncRead` + `AsyncWrite`, surfacing peer `NodeId`.
2. The **FFI dispatch service** — the single tower `Service` that turns Rust requests into JS handle channels and back.
3. The **public platform-shaped types** in the JS adapters (`fetch`, `serve`, error classes, `Session`) — these have to mirror WHATWG / Deno / WebTransport precisely.

Anywhere else, custom code requires a one-paragraph justification comment in the code that:

- references this ADR by number;
- names the ecosystem option that was considered;
- explains why it does not fit (a concrete incompatibility, not a preference).

### D2 — Stop signal

If a contributor spends more than **~2 compile iterations** fighting tower / hyper / tower-http type or lifetime errors when adding a layer or middleware, they are off-pattern. The required action is:

1. Stop editing.
2. Read the equivalent code in [`axum/src/serve/mod.rs`](https://github.com/tokio-rs/axum/blob/main/axum/src/serve/mod.rs), [`hyper-util`](https://docs.rs/hyper-util/latest/hyper_util/), or the relevant `tower-http` example.
3. Either: (a) restructure the wiring so the ecosystem layer fits, or (b) file an issue against the wiring (not the layer) and stop.

The stop signal is intentionally cheap: two iterations is short enough to catch drift before it becomes an hour-long detour, and explicit enough that an AI agent following a skill can recognise it.

### D3 — Discoverability for agents

To make D1 and D2 reachable without reading the full principles file:

- [`.github/copilot-instructions.md`](../../.github/copilot-instructions.md) gains a "Before writing custom HTTP / middleware code" pointer to this ADR and to `axum/src/serve/mod.rs`.
- [`.agents/skills/fix-issues/SKILL.md`](../../.agents/skills/fix-issues/SKILL.md) gains the stop-signal heuristic in its fix-loop guardrails so any agent fixing an issue inherits it automatically.

## Consequences

- New middleware work starts by checking tower-http / hyper-util / axum first; the wiring is shaped to receive standard layers, not to wrap custom ones.
- ADR-014 (runtime architecture) becomes the concrete embodiment of this rule. Future contributors do not have to re-derive the seams.
- The cost of writing a bespoke primitive becomes visible at review time: the justification comment is a permanent record that reviewers can challenge.
- AI agents have an explicit anchor (`copilot-instructions.md` pointer + skill guardrail) and can no longer drift unannounced.

## Next steps

- [x] Land ADR-013 alongside ADR-014.
- [x] Update `docs/principles.md` to cross-reference ADR-013 from principle #3.
- [x] Update `.github/copilot-instructions.md` with the "before writing custom HTTP/middleware code" pointer.
- [x] Update `.agents/skills/fix-issues/SKILL.md` with the stop-signal heuristic.
