#!/usr/bin/env bash
set -euo pipefail
#
# Interactive release workflow.
#
# Usage:
#   scripts/release.sh [VERSION] [--skip-ci]
#   npm run release
#   npm run release -- 0.4.0
#
# Steps:
#   1. Determine target version (and check required tools up front)
#   2. Show unreleased commits since last tag
#   3. Run npm run ci — exits immediately on any failure
#   4. Bump all manifests (Cargo.toml, package.json, deno.jsonc, adapter.ts)
#   5. Prepend the release section to CHANGELOG.md via git-cliff
#   6. Show diff and ask to confirm
#   7. Commit and tag vX.Y.Z (version bump + changelog in one commit)
#   8. Ask whether to push (pushing triggers GitHub Actions build + publish)

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'; BLUE='\033[0;34m'
BOLD='\033[1m'; NC='\033[0m'

ok()      { echo -e "  ${GREEN}✓${NC}  $1"; }
fail()    { echo -e "  ${RED}✗${NC}  $1"; }
warn()    { echo -e "  ${YELLOW}!${NC}  $1"; }
section() { echo -e "\n${BOLD}${BLUE}── $1 ──${NC}"; }
ask()     { printf "\n  ${YELLOW}?${NC}  $1"; }

VERSION=""
SKIP_CI=false

for arg in "$@"; do
  case "$arg" in
    --skip-ci) SKIP_CI=true ;;
    -*)        echo "Unknown flag: $arg"; exit 1 ;;
    *)         VERSION="$arg" ;;
  esac
done

# ── 1. Version ────────────────────────────────────────────────────────────────

section "Release"

CURRENT=$(grep '^version = ' "$ROOT/Cargo.toml" | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "  Current version: ${BOLD}v$CURRENT${NC}"

if [[ -z "$VERSION" ]]; then
  ask "New version (e.g. 0.4.0): "
  read -r VERSION
fi

VERSION="${VERSION// /}"  # trim accidental spaces

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
  fail "'$VERSION' is not valid semver (expected X.Y.Z or X.Y.Z-pre)"
  exit 1
fi

if [[ "$VERSION" == "$CURRENT" ]]; then
  fail "Already at v$VERSION — nothing to bump"
  exit 1
fi

TAG="v$VERSION"
echo "  Target:          ${BOLD}$TAG${NC}"

# Fail fast on missing tools before the long CI run, so a release can't get
# halfway (bumped + CI-passed) only to stall on a missing changelog generator.
if ! command -v git-cliff >/dev/null 2>&1; then
  fail "git-cliff not found — required to regenerate CHANGELOG.md"
  echo "     Install: brew install git-cliff   (or: cargo install git-cliff)"
  exit 1
fi

# ── 2. Unreleased commits ─────────────────────────────────────────────────────

section "Unreleased commits"

LAST_TAG=$(git tag --sort=-version:refname | head -1)
if [[ -n "$LAST_TAG" ]]; then
  COMMITS=$(git log "$LAST_TAG"..HEAD --oneline)
  COUNT=$(echo "$COMMITS" | wc -l | tr -d ' ')
  echo "$COMMITS" | head -20
  [[ "$COUNT" -gt 20 ]] && warn "…and $((COUNT - 20)) more"
  echo ""
  echo "  $COUNT commit(s) since $LAST_TAG"
else
  git log --oneline | head -10
fi

# ── 3. CI ─────────────────────────────────────────────────────────────────────

section "CI"

if $SKIP_CI; then
  warn "Skipping CI (--skip-ci)"
else
  if ! npm run ci; then
    echo ""
    fail "CI failed. Fix the issues above and run again."
    exit 1
  fi
  ok "All checks passed"
fi

# ── 4. Version bump ───────────────────────────────────────────────────────────

section "Version bump → $VERSION"
bash "$ROOT/scripts/version.sh" "$VERSION"

# ── 5. Changelog ──────────────────────────────────────────────────────────────
# Prepend the new release section to CHANGELOG.md. --unreleased scopes it to
# commits since the last tag; --prepend leaves prior sections untouched (no
# history rewrite). Uses git-cliff's default config — the same one CI uses for
# the GitHub Release body (build.yml: `git cliff --latest`) — so the in-repo
# changelog and the published release notes stay identical. This runs after
# version.sh (which leaves the tree dirty), so `git add -u` below commits the
# changelog and the version bump together in the single `chore: release` commit.

section "Changelog → $TAG"
git cliff --tag "$TAG" --unreleased --prepend "$ROOT/CHANGELOG.md"
ok "Prepended $TAG section to CHANGELOG.md"

# ── 6. Review ─────────────────────────────────────────────────────────────────

echo ""
git diff --stat
echo ""
ask "Commit and tag $TAG? [y/N] "
read -r CONFIRM

if [[ ! "$CONFIRM" =~ ^[Yy]$ ]]; then
  warn "Aborted — reverting version bump + changelog"
  git checkout -- .
  exit 0
fi

# ── 7. Commit + tag ───────────────────────────────────────────────────────────

section "Commit + tag"

git add -u
git commit -m "chore: release $TAG"
ok "Committed"

git tag "$TAG" -m "Release $TAG"
ok "Tagged $TAG"

# ── 8. Push ───────────────────────────────────────────────────────────────────

echo ""
ask "Push main + $TAG to origin now? [y/N] "
read -r PUSH

if [[ "$PUSH" =~ ^[Yy]$ ]]; then
  git push origin main --tags
  ok "Pushed"
  echo ""
  echo -e "  ${GREEN}${BOLD}✓ Release $TAG is underway${NC}"
  echo "  GitHub Actions will build all targets and publish."
  echo "  https://github.com/Momics/iroh-http/actions"
else
  echo ""
  echo "  When ready:"
  echo "    git push origin main --tags"
fi

echo ""
