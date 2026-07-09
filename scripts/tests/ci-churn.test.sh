#!/usr/bin/env bash
# ── ci-churn.test.sh ────────────────────────────────────────────────────────
# Regression tests for the release.sh auto-revert-vs-abort decision (see #326).
# Exercises the pure logic in scripts/lib/ci-churn.sh with canned
# `git status --porcelain` fixtures — no git, no build, no publishing.
#
# Usage: bash scripts/tests/ci-churn.test.sh
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/ci-churn.sh
source "$DIR/../lib/ci-churn.sh"

PASS=0
FAIL=0

# assert_unexpected NAME EXPECTED PORCELAIN
#   Feed PORCELAIN through ci_churn_unexpected_paths and compare the printed
#   unexpected-paths list against EXPECTED (newline-joined, may be empty).
assert_unexpected() {
  local name="$1" expected="$2" porcelain="$3" got
  got="$(printf '%s' "$porcelain" | ci_churn_unexpected_paths)"
  if [[ "$got" == "$expected" ]]; then
    echo "  ✓ $name"
    PASS=$((PASS + 1))
  else
    echo "  ✗ $name"
    echo "      expected: [$expected]"
    echo "      got:      [$got]"
    FAIL=$((FAIL + 1))
  fi
}

# Clean tree → nothing dirty → no unexpected paths (release proceeds, no revert).
assert_unexpected "clean tree" "" ""

# Only known-churn artifacts dirty → subset of allowlist → auto-revert (no abort).
assert_unexpected "deno.lock only" "" " M deno.lock"
assert_unexpected "index.js only" "" " M packages/iroh-http-node/index.js"
assert_unexpected "both churn artifacts" "" \
" M deno.lock
 M packages/iroh-http-node/index.js"

# A single non-allowlisted path → fail-closed abort, even mixed with churn.
assert_unexpected "unrelated file only" "src/main.rs" " M src/main.rs"
assert_unexpected "churn + unrelated → abort" "src/main.rs" \
" M deno.lock
 M src/main.rs"
assert_unexpected "untracked file → abort" "notes.txt" "?? notes.txt"

# A file whose basename matches but path differs must NOT be treated as churn.
assert_unexpected "look-alike path is not allowlisted" "vendor/deno.lock" \
" M vendor/deno.lock"

# Rename destination is what lands on disk; classify by the new path.
assert_unexpected "rename to unrelated path → abort" "src/renamed.rs" \
"R  src/old.rs -> src/renamed.rs"

echo ""
echo "  $PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]]
