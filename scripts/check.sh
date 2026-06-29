#!/usr/bin/env bash
# ── check ──────────────────────────────────────────────────────────────────────
# Pre-push development check. Mirrors the CI `verify` job exactly.
# Run this before pushing to main.
#
# Each step delegates to an npm script so the same atomic commands work both
# here and when called directly by a developer (e.g. `npm run lint`).
#
# Usage:
#   scripts/check.sh        # full check
#   npm run ci              # same thing
#
# Exit code is non-zero if any check fails.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RED='\033[0;31m'; GREEN='\033[0;32m'; BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'

ok()      { echo -e "  ${GREEN}✓${NC}  $1"; }
section() { echo -e "\n${BOLD}${BLUE}── $1 ──${NC}"; }
die()     { echo -e "  ${RED}✗${NC}  $1"; exit 1; }

section "Format"

echo "  → fmt:check"
npm run fmt:check --silent || die "deno fmt failed — run: npm run fmt"
ok "deno fmt"

section "Rust"

echo "  → lint"
npm run lint --silent || die "lint failed — run: npm run lint"
ok "lint"

echo "  → test:rust"
npm run test:rust --silent
ok "tests"

echo "  → test:tauri"
npm run test:tauri --silent
ok "tests (tauri)"

echo "  → test:tauri:guest"
npm run test:tauri:guest --silent
ok "tests (tauri guest-js)"

echo "  → deny"
if ! command -v cargo-deny &>/dev/null; then
  die "cargo-deny not found — install it: cargo install cargo-deny --locked"
fi
cargo-deny check
ok "deny"

echo "  → audit:rust"
if ! command -v cargo-audit &>/dev/null; then
  die "cargo-audit not found — install it: cargo install cargo-audit --locked"
fi
cargo audit
ok "audit:rust"

echo "  → audit:npm"
npm audit --audit-level=high
ok "audit:npm"

echo "  → bench:smoke"
npm run bench:smoke --silent
ok "bench smoke"

echo "  → check:features"
npm run check:features --silent
ok "feature checks"

section "Build"

echo "  → build:shared"
npm run build:shared --silent
ok "shared"

echo "  → build:node"
npm run build:node --silent
ok "node"

echo "  → build:deno"
npm run build:deno --silent
ok "deno"

section "TypeScript"

echo "  → typecheck"
npm run typecheck --silent
ok "typecheck"

echo "  → lockfile"
node "$ROOT/scripts/check-lockfile.mjs" >/dev/null
ok "lockfile (no empty-version optional stubs — #154)"

section "Tests"

echo "  → test:node"
npm run test:node --silent
ok "node"

echo "  → test:deno"
npm run test:deno --silent
ok "deno"

echo "  → test:interop"
npm run test:interop --silent
ok "interop"

echo ""
echo -e "${GREEN}${BOLD}All checks passed.${NC} Ready to push."
