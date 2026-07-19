---
name: fix-issues
description: Systematically resolve a set of open iroh-http issues through evidence-based triage, compatible vertical slices, focused verification, reviewed commits, and a branch pull request. Use when working through the backlog or resolving multiple related issues. Do not use for creating issues or bypassing review with a direct main-branch push.
---

# Fix iroh-http issues

Resolve open GitHub issues systematically: triage → plan → fix → verify →
review → merge → close.

## Phase 1 — Discover

Fetch all open issues with an available GitHub integration or `gh`.

Read the full body of any issue that lacks sufficient detail before planning.

## Phase 2 — Triage

Sort by priority label: **P1 first**, then P2, then P3. Within a priority tier, prefer issues that:
- Touch fewer files (lower risk)
- Have clear acceptance criteria
- Are not blocked on another open issue

**Skip (defer to a future session):**
- Issues with no labels — triage them first using the `manage-issues` skill
- Issues that require significant architectural decisions without prior analysis
- Issues where the fix would touch the same files as another planned fix and conflict

Record the final ordered work plan before proceeding. Use session memory if the list is long.

## Phase 3 — Group

Decide which issues to combine into a single commit vs. keep separate.

**Combine when:**
- Changes touch the same file(s) and the diff would be reviewed as one unit
- Fixes are logically inseparable (fixing one makes no sense without the other)
- Same crate/package, same type of change (e.g., two CI config corrections, two clippy lints)

**Keep separate when:**
- Different concerns that would produce a muddled commit message
- One fix might cause the other's CI to fail (fix separately to keep bisectability)
- Different scopes — keep `git blame` clean for future diagnostics
- One is a `fix`, the other is a `refactor` or `ci`

Label each group in your plan before writing any code.

## Phase 4 — Fix loop

For each group, in plan order:

### 4a. Read before touching
Read all relevant files in the issue's Evidence section. Understand the existing code before changing anything.

### 4b. Implement
Make the minimal change that satisfies the acceptance criteria. Do not refactor adjacent code, add unrelated docs, or widen scope. If the fix reveals a deeper problem, file a new issue rather than expanding this one.

### 4c. Verify locally

```
npm run ci
```

This runs: full release build → `scripts/check.sh` (fmt + clippy strict + cargo test workspace + cargo test tauri + bench smoke + feature checks + typecheck) → Node e2e → Deno tests → interop suite.

**If CI fails:**
- Fix the failure before moving on — never commit broken code
- If the failure is pre-existing and unrelated to this issue, note it and decide: fix it in the same commit (if trivial), open a new issue, or skip this group if it blocks verification

### 4d. Commit

Follow the repository conventions in `AGENTS.md`. Use closing keywords only
when the pull request will actually resolve the referenced issue.

```
fix(scope): short description

Body explaining what and why.

Closes #42
Closes #43
```

After committing, record the commit hash.

### 4e. Record completion evidence

For each issue addressed by the commit, record:

1. The acceptance criteria satisfied.
2. The focused and full validation commands run.
3. The commit hash that will appear in the pull request.

Do not close the issue yet. A local commit or unmerged branch is not completion.
Then move to the next group.

## Phase 5 — Review and merge

Only after all planned groups are committed and verified:

1. Push the working branch, never `main` directly.
2. Open one focused pull request whose body maps commits and tests to issues.
3. Wait for required CI and review. Address failures on the branch.
4. After merge, verify each issue's acceptance criteria on the default branch.
5. Let closing keywords close resolved issues, or use `manage-issues` to post
   the merged change and close them manually.

## Guardrails

- Never push with failing CI
- Never push directly to `main`
- Never close an issue before the resolving change is merged
- Never combine issues whose fixes conflict — this produces a commit that is hard to revert
- If a fix turns out to be larger than expected mid-implementation, stop, file a more detailed issue, and skip that group for this session
- Amend the commit (not a new commit) if CI catches something in the immediately preceding fix before moving on
- **Stop signal — ecosystem composition.** If a fix involves adding or modifying a tower / hyper / tower-http layer in `iroh-http-core` and you spend more than ~2 compile iterations fighting type or lifetime errors, you are off-pattern. Stop editing. Read [ADR-013](../../../docs/adr/013-lean-on-the-ecosystem.md), [ADR-014](../../../docs/adr/014-runtime-architecture.md), and the equivalent code in [`axum/src/serve/mod.rs`](https://github.com/tokio-rs/axum/blob/main/axum/src/serve/mod.rs). Either restructure the wiring to fit the standard layer, or file a separate issue against the wiring and skip this group.

## Related skills

- [manage-issues](./../manage-issues/SKILL.md) — create, label, and structure issues
- [regression-first](./../regression-first/SKILL.md) — reproduce a released bug before fixing it
