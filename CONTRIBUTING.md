# Contributing to iroh-http

Thank you for your interest in contributing!

## Development setup

### Prerequisites

- Rust 1.77+ (`rustup update stable`)
- Node.js 18+ (for Node.js adapter)
- Deno 2+ (for Deno adapter)
- Tauri CLI v2 (for Tauri plugin)

### Git hooks

Run once after cloning to enable the pre-commit hook (checks `cargo fmt`):

```sh
git config core.hooksPath .githooks
```

### Build

```sh
# Check all Rust crates
cargo check --workspace

# Check Tauri plugin (separate workspace)
cd packages/iroh-http-tauri && cargo check

# TypeScript
npm install
npm run typecheck
```

## Code style

- Rust: `cargo fmt` + `cargo clippy`
- TypeScript: standard formatting

## Benchmarks

Run benchmarks in release mode and on a dedicated machine when possible.

```sh
# Node.js (mitata)
npm run bench:node

# Deno (Deno.bench)
npm run bench:deno

# Rust core / Tauri baseline (Criterion)
npm run bench:rust
```

For normalized benchmark reports used by CI regression checks:

```sh
npm run bench:node:report
npm run bench:deno:report
```

## Submitting changes

1. Fork the repository
2. Create a feature branch: `git checkout -b feature/my-change`
3. Make your changes with tests
4. Run `cargo check --workspace` to verify Rust compiles
5. Submit a pull request

## Versioning and releases

**Do not bump version fields manually.** This includes `Cargo.toml`, `package.json`, `deno.json`, and `deno.jsonc`.

Version fields are managed exclusively by `scripts/release.sh`, which bumps all manifests atomically, regenerates lock files, prepends the release section to `CHANGELOG.md` (via [git-cliff](https://git-cliff.org)), and produces a `chore: release X.Y.Z` commit immediately before tagging and publishing. Bumping versions inside a feature or fix commit breaks CI: the new version's platform binaries don't exist on npm until the tag is published, so `npm ci` crashes on every platform for the entire window between the bump and the release.

CI enforces this via the `version-bump-policy` job, which rejects any commit that touches a version-bearing file unless the commit subject starts with `chore: release`.

The release script uses git-cliff to prepend a curated version section to
`CHANGELOG.md`. The tag workflow then extracts that reviewed section verbatim
for the draft GitHub Release body; it does not regenerate a second set of raw
notes. This is why commit messages must follow the Conventional Commit rules in
[`AGENTS.md`](AGENTS.md). `git-cliff` must be installed locally to cut a release
(`brew install git-cliff` or `cargo install git-cliff`); the release script
checks for it up front.

**To cut a release:** `bash scripts/release.sh X.Y.Z`

Pushing the release tag creates a **draft** GitHub release and starts three
independent workflows. It does not publish packages. Complete the release in
this order:

1. Wait for the tag's **Build**, **Extended tests**, and **Benchmarks** workflows
   to pass (benchmarks record a snapshot; they do not enforce a performance gate).
2. Verify that the draft release assets and the successful Build run use the
   tag's exact commit SHA.
3. Smoke-test the native artifacts produced by that Build run.
4. Manually run the **Publish** workflow with the release tag. It resolves the
   tag to the successful Build run and promotes those exact artifacts to npm,
   crates.io, and JSR.
5. Verify the published versions and perform a clean consumer install.
6. Publish the draft GitHub release.

The release script must be run from a clean local `main` at the exact
`origin/main` tip. It pushes only the release commit and its single tag,
atomically; unrelated local tags are never pushed.

Do not publish the GitHub draft or dispatch **Publish** before all tag
workflows are green. If a release is abandoned before any registry package is
published, leave the GitHub release as a draft while deciding whether to retry
or retire the tag; never reuse its artifacts for a different commit.

## License

By contributing, you agree that your contributions will be licensed under
MIT OR Apache-2.0.
