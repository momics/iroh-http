# Agent instructions

These instructions apply to the entire repository.

## Repository discipline

- Preserve existing work. Use a clean branch or worktree when the active
  checkout contains unrelated changes.
- Read the relevant architecture, specification, and guideline documents
  before changing behavior.
- Keep public interfaces aligned across the Rust core, FFI adapters, shared
  TypeScript package, documentation, and tests.
- Verify changes proportionately. Use `npm run ci` for release-facing work and
  the focused commands in `docs/build-and-test.md` while iterating.

## Git conventions

- Use Conventional Commits: `<type>(<optional-scope>): <imperative summary>`.
- Use `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `ci`, `build`, or
  `chore`; add `!` for breaking changes.
- Prefer the package or subsystem as scope: `core`, `node`, `deno`, `tauri`,
  `shared`, or `discovery`.
- Keep the subject lowercase, without a period, and at most 72 characters.
- Explain what and why in the body. Reference issues with `Fixes #N` or
  `Closes #N` only when the change actually resolves them.
- Use a host-required branch prefix followed by a short kebab-case purpose.
- Format pull-request titles like Conventional Commit subjects.

## Project skills

- `.agents/skills/` is the single canonical location for tracked project
  skills. Do not duplicate a skill under another agent-specific directory.
- `.agents/` remains blanket-ignored so local skills cannot enter a commit via
  bulk staging. Repository skill files are intentionally force-added and
  reviewed explicitly; never force-add the whole `.agents/` tree.
- Track a skill only when it captures iroh-http-specific knowledge or bundles
  deterministic tooling needed by this repository.
- Keep general-purpose or user-specific skills outside the repository.
- Keep each `SKILL.md` concise and place deterministic helpers or detailed
  references in that skill's `scripts/` or `references/` directory.
