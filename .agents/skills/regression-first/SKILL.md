---
name: regression-first
description: 'Write a failing regression test before fixing any bug that escaped a release. USE FOR: any bug reported against a published version (e.g. "this broke in v0.2.1"), any issue filed after a release, any case where CI passed but the bug reached users. DO NOT USE FOR: bugs caught before release by existing tests, new features, refactors.'
---

# Regression-First — Momics/iroh-http

If a bug escaped into a released version, a test could have caught it. Write
the test before writing the fix. No exceptions.

## Principle

A bug in production means two things failed:
1. The code was wrong.
2. The test suite didn't cover that behaviour.

Fixing the code alone leaves the second failure unaddressed. A regression test
closes both gaps and makes the fix meaningful for future changes.

## Workflow

### Step 1 — Reproduce in a test

Before touching any source code, write a test that:

- Exercises the exact behaviour the issue describes.
- **Fails** on the current `main` (i.e. it would have caught this release).
- Passes after the fix and only after the fix.

If the test cannot be made to fail reliably (e.g. the bug is timing-dependent
or process-exit behaviour), mark it `ignore` with a clear comment and a `TODO:
remove ignore when #N is fixed` annotation. Do **not** skip writing it.

### Step 2 — Commit the test separately

```
test(scope): add regression for <one-line description> (#N)

Reproduces the bug described in #N. This test fails on the current code
and will pass once the fix is applied.
```

Committing the test before the fix lets `git bisect` find the exact commit
that broke the behaviour.

### Step 3 — Fix the bug

Make the minimal change that makes the test pass. Do not expand scope.

### Step 4 — Verify the full suite

```
npm run ci
```

The regression test must pass. No pre-existing tests must newly fail.

### Step 5 — Commit the fix

```
fix(scope): <description> (#N)

<What changed and why.>

Closes #N
```

### Step 6 — Close the issue

After the resolving pull request is merged, post a full link to the merged pull
request or commit and a one-sentence outcome. Then close the issue via
`manage-issues` if GitHub did not close it automatically.

---

## Where regression tests live

| Layer | Location |
|---|---|
| Rust core logic | `crates/iroh-http-core/tests/integration.rs` |
| Deno adapter / JS behaviour | `packages/iroh-http-deno/test/adapter.test.ts` |
| Node.js adapter | `packages/iroh-http-node/test/` |
| Shared TypeScript | `packages/iroh-http-shared/src/*.test.ts` (if exists) |

## Anatomy of a good regression test

```typescript
// Regression: #N — <one-line description of what broke>
//
// Root cause: <what was wrong and why it was hard to catch>
//
// Fix: <what changed to make it pass>
Deno.test({
  name: "<component> — <expected behaviour> (regression #N)",
  // sanitizeOps, ignore, etc. if needed — document why
}, async () => {
  // minimal reproduction
});
```

## On `ignore: true`

If the test correctly describes the expected behaviour but the fix isn't ready
yet, use `ignore: true` + a comment. **Do not delete the test.** An ignored
test with a clear issue reference is better than a comment saying "we should
add a test for this one day."

```typescript
Deno.test({
  name: "serve — no pending ops after shutdown (regression #115)",
  ignore: true, // Remove when #115 is fixed.
  sanitizeOps: true,
}, async () => { ... });
```

## Relation to other skills

- Use `manage-issues` to confirm the issue exists and to close it after fixing.
- Use `fix-issues` to sequence the regression-first workflow within a larger
  backlog sweep.
- Follow `AGENTS.md` for commit and branch conventions.
