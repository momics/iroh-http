#!/usr/bin/env bash
# ── ci-churn ────────────────────────────────────────────────────────────────
# Shared logic for classifying working-tree dirt left by `npm run ci`.
#
# `npm run ci` regenerates tracked build artifacts. On machines whose toolchain
# differs from CI's (napi-rs CLI version, deno lock ordering), the regenerated
# files differ from the committed baseline and leave the tree dirty — which
# would trip version.sh's dirty-tree guard *after* the slow CI pass (see #326).
#
# This library is intentionally free of git/publish side effects so the
# revert-vs-abort decision can be unit-tested in isolation
# (scripts/tests/ci-churn.test.sh).

# The exact allowlist of artifacts CI is known to regenerate. The auto-revert in
# release.sh triggers ONLY when the dirty set is a subset of this list; a single
# path outside it forces a fail-closed abort (nothing is reverted).
CI_CHURN_ARTIFACTS=(
  "deno.lock"
  "packages/iroh-http-node/index.js"
)

# ci_churn_is_allowlisted PATH
#   Returns 0 if PATH is an exact match for a known-churn artifact, else 1.
ci_churn_is_allowlisted() {
  local candidate="$1" a
  for a in "${CI_CHURN_ARTIFACTS[@]}"; do
    [[ "$candidate" == "$a" ]] && return 0
  done
  return 1
}

# ci_churn_path_from_porcelain LINE
#   Extract the affected path from a single `git status --porcelain` line.
#   Handles the "XY " status prefix, rename arrows ("old -> new" → new), and
#   the surrounding quotes git adds for paths with special characters.
ci_churn_path_from_porcelain() {
  local line="$1" path
  path="${line:3}"                 # strip the 2-char status + separating space
  if [[ "$path" == *" -> "* ]]; then
    path="${path##* -> }"          # rename: the destination is what's on disk
  fi
  path="${path%\"}"; path="${path#\"}"
  printf '%s' "$path"
}

# ci_churn_unexpected_paths
#   Read `git status --porcelain` output on stdin; print (one per line) every
#   dirty path that is NOT in the allowlist. Empty output means every dirty path
#   is a known-churn artifact (safe to auto-revert). Any output means abort.
ci_churn_unexpected_paths() {
  local line path
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ -z "$line" ]] && continue
    path="$(ci_churn_path_from_porcelain "$line")"
    ci_churn_is_allowlisted "$path" || printf '%s\n' "$path"
  done
}
