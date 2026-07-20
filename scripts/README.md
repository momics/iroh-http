# Scripts

## Daily development

```sh
npm run ci
```

Requires dependencies to be installed with plain `npm ci` first. Do not use
`npm ci --omit=optional` locally — the Tauri guest-JS tests need Vitest's
native rolldown binding.

Runs: `cargo fmt --check` → `cargo clippy` → `cargo test` → feature checks →
Tauri guest-JS tests → TypeScript typecheck → builds → Node/Deno/interop tests.

Matches the GitHub CI coverage across `verify`, `bench-smoke`, and `e2e`.
Run this before pushing to `main`.

---

## Releasing

```sh
npm run release
```

Or pass the version directly to skip the prompt:

```sh
npm run release -- 0.4.0
```

The script is interactive and walks you through each step:

1. Shows unreleased commits since the last tag
2. Runs `npm run ci` — **exits immediately if any check fails**
3. Bumps all manifests (`Cargo.toml`, `package.json`, `deno.jsonc`, `adapter.ts`)
4. Prepends the curated version section to `CHANGELOG.md`
5. Shows the diff and asks you to confirm
6. Commits `chore: release vX.Y.Z` and creates the git tag
7. Asks whether to push

### Pre-tag mobile discovery gate

Before running this script, check the changes since the previous tag. If mobile
discovery implementation, native lifecycle, permissions, service declarations,
or setup instructions changed materially, complete the affected physical-device
matrix in the
[on-device DNS-SD verification runbook](../docs/internals/dns-sd-device-verification.md).
Record the candidate commit, devices and OS/API versions, tested directions,
suite totals, generic DNS-SD result, and required JSON/log excerpts in the
release-tracking issue. CI compile and contract tests are not a substitute for
this pre-tag device evidence.

Pushing the tag triggers three GitHub Actions workflows:

| Workflow | What it does |
|----------|-------------|
| `build.yml` | Creates a draft GitHub release from the curated changelog section and builds native binaries across 5 platforms |
| `extended-tests.yml` | Runs the tag's extended compatibility and platform tests |
| `bench.yml` | Records the tag's Rust, Node.js, and Deno benchmark snapshot |

After all three workflows are green, a maintainer manually runs `publish.yml`
for that exact tag, verifies npm, JSR, and crates.io, and publishes the draft
GitHub release.

---

## Individual commands

```sh
# Bump all manifests without committing or tagging:
npm run version:bump -- 0.4.0

# Run CI checks only:
npm run ci

# Manually republish a package (if publish.yml needs a retry):
npm run publish:shared          # → npm
npm run publish:shared:jsr      # → JSR
npm run publish:node            # → npm (all platform packages)
npm run publish:deno            # → JSR
npm run publish:tauri           # → npm
```

---

## Prerequisites

| Tool | Purpose | Install |
|------|---------|---------|
| Rust (stable) | Core build | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Node.js 22+ | JS packages, tests | [nodejs.org](https://nodejs.org) |
| Deno | Deno package, tests | `curl -fsSL https://deno.land/install.sh \| sh` |
| cargo-deny | License / advisory checks | `cargo install cargo-deny --locked` |
| cargo-audit | Security advisories | `cargo install cargo-audit --locked` |
| git-cliff | Curated changelog generation | `cargo install git-cliff --locked` |

Cross-compilation (5 platforms) is handled entirely by GitHub Actions — no local cross-compile toolchain is needed for releasing.
