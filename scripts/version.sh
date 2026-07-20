#!/usr/bin/env bash
set -euo pipefail

# Usage: ./scripts/version.sh 0.2.0
#
# Bumps the version across ALL package manifests:
#   - Root Cargo.toml [workspace.package] (covers all 5 workspace members via inheritance)
#   - packages/iroh-http-tauri/Cargo.toml (standalone workspace — updated explicitly)
#   - 3 package.json (node, tauri, shared)
#   - 1 deno.jsonc
#   - 1 deno.json (shared JSR)
#
# Synchronizes lockfile entries for workspace packages without upgrading
# third-party dependencies. Examples are intentionally untouched.

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <new-version>"
  echo "  e.g. $0 0.2.0"
  exit 1
fi

NEW="$1"

# ── Dirty-tree guard ──────────────────────────────────────────────────────────
# version.sh must only be invoked via scripts/release.sh from a clean tree.
# A dirty tree means either (a) uncommitted work would get tangled with the
# release commit, or (b) someone is bumping versions manually mid-feature,
# which is exactly the pattern that breaks CI (see #196).
if [[ -n "$(git -C "$ROOT" status --porcelain)" ]] && [[ -z "${ALLOW_DIRTY:-}" ]]; then
  echo "Error: working tree is dirty. version.sh should only run from a clean tree," >&2
  echo "       typically via scripts/release.sh." >&2
  echo "       Do NOT bump versions manually inside a feature commit — see CONTRIBUTING.md." >&2
  echo "       Set ALLOW_DIRTY=1 to override (testing only)." >&2
  exit 1
fi

# Validate semver-ish format
if ! [[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
  echo "Error: '$NEW' doesn't look like a valid version (expected X.Y.Z or X.Y.Z-pre)"
  exit 1
fi

# Detect current version from the source of truth ([workspace.package] in root)
OLD=$(grep '^version = ' "$ROOT/Cargo.toml" | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Bumping $OLD → $NEW"

# ── Root Cargo.toml [workspace.package] ──────────────────────────────────────
# All 5 workspace members inherit from here; one edit covers them all.
sed -i '' "s/^version = \"$OLD\"/version = \"$NEW\"/" "$ROOT/Cargo.toml"
echo "  ✓ Cargo.toml (workspace.package)"

# ── Internal path-dep version constraints ─────────────────────────────────────
# Every path dep with a version field must be updated. Replace ANY version
# (not just $OLD) to catch deps that drifted in prior releases.
for cargo_file in \
  crates/iroh-http-adapter/Cargo.toml \
  crates/iroh-http-discovery/Cargo.toml \
  packages/iroh-http-deno/Cargo.toml \
  packages/iroh-http-node/Cargo.toml; do
  filepath="$ROOT/$cargo_file"
  if [[ -f "$filepath" ]]; then
    sed -i '' "s/\(iroh-http-core.*version = \"\)[^\"]*\"/\1$NEW\"/" "$filepath"
    sed -i '' "s/\(iroh-http-adapter.*version = \"\)[^\"]*\"/\1$NEW\"/" "$filepath"
    sed -i '' "s/\(iroh-http-discovery.*version = \"\)[^\"]*\"/\1$NEW\"/" "$filepath"
    echo "  ✓ $cargo_file"
  fi
done

# ── iroh-http-tauri (standalone [workspace] — must be updated explicitly) ────
TAURI_CARGO="$ROOT/packages/iroh-http-tauri/Cargo.toml"
if [[ -f "$TAURI_CARGO" ]]; then
  # Package version
  sed -i '' "s/^version = \"$OLD\"/version = \"$NEW\"/" "$TAURI_CARGO"
  # Internal path-dep version constraints (iroh-http-core, iroh-http-adapter, iroh-http-discovery).
  # Replace ANY version in these deps, not just $OLD, to catch deps that were
  # out of sync from prior releases (e.g. iroh-http-adapter stuck at 0.1.0).
  sed -i '' "s/\(iroh-http-core.*version = \"\)[^\"]*\"/\1$NEW\"/" "$TAURI_CARGO"
  sed -i '' "s/\(iroh-http-adapter.*version = \"\)[^\"]*\"/\1$NEW\"/" "$TAURI_CARGO"
  sed -i '' "s/\(iroh-http-discovery.*version = \"\)[^\"]*\"/\1$NEW\"/" "$TAURI_CARGO"
  echo "  ✓ packages/iroh-http-tauri/Cargo.toml"
fi

# ── package.json files ────────────────────────────────────────────────────────
JSON_FILES=(
  packages/iroh-http-shared/package.json
  packages/iroh-http-node/package.json
  packages/iroh-http-tauri/package.json
)

for f in "${JSON_FILES[@]}"; do
  filepath="$ROOT/$f"
  if [[ ! -f "$filepath" ]]; then
    echo "  SKIP (not found): $f"
    continue
  fi
  # Only replace the top-level "version" field (line must start with  "version")
  sed -i '' "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" "$filepath"
  echo "  ✓ $f"
done

# ── platform package.json files (iroh-http-node npm/ split) ──────────────────
PLATFORM_DIRS=(
  packages/iroh-http-node/npm/darwin-arm64
  packages/iroh-http-node/npm/darwin-x64
  packages/iroh-http-node/npm/linux-x64-gnu
  packages/iroh-http-node/npm/linux-arm64-gnu
  packages/iroh-http-node/npm/win32-x64-msvc
)

for dir in "${PLATFORM_DIRS[@]}"; do
  filepath="$ROOT/$dir/package.json"
  if [[ ! -f "$filepath" ]]; then
    echo "  SKIP (not found): $dir/package.json"
    continue
  fi
  sed -i '' "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" "$filepath"
  # Update pinned optionalDependency version in the main node package.json
  pkg=$(basename "$dir")
  sed -i '' "s/\"@momics\/iroh-http-node-$pkg\": \"$OLD\"/\"@momics\/iroh-http-node-$pkg\": \"$NEW\"/" "$ROOT/packages/iroh-http-node/package.json"
  echo "  ✓ $dir/package.json"
done

# ── deno.jsonc ────────────────────────────────────────────────────────────────
DENO_JSON="$ROOT/packages/iroh-http-deno/deno.jsonc"
if [[ -f "$DENO_JSON" ]]; then
  sed -i '' "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" "$DENO_JSON"
  # Update the shared import version range (^0.1 → ^0.2, etc.)
  MAJOR_MINOR=$(echo "$NEW" | sed 's/\([0-9]*\.[0-9]*\).*/\1/')
  sed -i '' "s|@momics/iroh-http-shared@\^[0-9.]*|@momics/iroh-http-shared@^$MAJOR_MINOR|" "$DENO_JSON"
  echo "  ✓ packages/iroh-http-deno/deno.jsonc"
fi

# ── shared deno.json (for JSR publish) ────────────────────────────────────────
SHARED_DENO="$ROOT/packages/iroh-http-shared/deno.json"
if [[ -f "$SHARED_DENO" ]]; then
  sed -i '' "s/\"version\": \"$OLD\"/\"version\": \"$NEW\"/" "$SHARED_DENO"
  echo "  ✓ packages/iroh-http-shared/deno.json"
fi

# ── Deno adapter VERSION constant ────────────────────────────────────────────
ADAPTER_TS="$ROOT/packages/iroh-http-deno/src/adapter.ts"
if [[ -f "$ADAPTER_TS" ]]; then
  sed -i '' "s/^const VERSION = \"$OLD\";/const VERSION = \"$NEW\";/" "$ADAPTER_TS"
  echo "  ✓ packages/iroh-http-deno/src/adapter.ts (VERSION)"
fi

# ── npm consumer dependency ranges ──────────────────────────────────────────
# iroh-http-node and iroh-http-tauri declare @momics/iroh-http-shared as a
# semver range (^MAJOR.MINOR).  When the minor version crosses a boundary the
# old range can no longer resolve the new version, so we update it here.
MAJOR_MINOR=$(echo "$NEW" | sed 's/\([0-9]*\.[0-9]*\).*/\1/')

NODE_PKG="$ROOT/packages/iroh-http-node/package.json"
if [[ -f "$NODE_PKG" ]]; then
  sed -i '' "s|\"@momics/iroh-http-shared\": \"\^[0-9.]*\"|\"@momics/iroh-http-shared\": \"^$MAJOR_MINOR\"|" "$NODE_PKG"
  echo "  ✓ packages/iroh-http-node/package.json (@momics/iroh-http-shared dep range → ^$MAJOR_MINOR)"
fi

TAURI_PKG="$ROOT/packages/iroh-http-tauri/package.json"
if [[ -f "$TAURI_PKG" ]]; then
  sed -i '' "s|\"@momics/iroh-http-shared\": \"\^[0-9.]*\"|\"@momics/iroh-http-shared\": \"^$MAJOR_MINOR\"|" "$TAURI_PKG"
  echo "  ✓ packages/iroh-http-tauri/package.json (@momics/iroh-http-shared dep range → ^$MAJOR_MINOR)"
fi

# ── Synchronize lock files ────────────────────────────────────────────────────
echo ""
echo "Synchronizing Cargo.lock …"
# `cargo update --workspace` updates the renamed local workspace packages while
# retaining every registry dependency already selected in Cargo.lock. Using
# `cargo generate-lockfile` here would silently upgrade compatible third-party
# dependencies after the release candidate had already passed CI.
cargo update --workspace --manifest-path "$ROOT/Cargo.toml"
echo "  ✓ Cargo.lock"

# Tauri has its own workspace with a separate Cargo.lock
if [[ -f "$ROOT/packages/iroh-http-tauri/Cargo.lock" ]]; then
  cargo update --workspace --manifest-path "$ROOT/packages/iroh-http-tauri/Cargo.toml"
  echo "  ✓ packages/iroh-http-tauri/Cargo.lock"
fi

echo "Synchronizing package-lock.json …"
# Only workspace metadata changed. Keep the reviewed third-party graph intact;
# a full npm install here would resolve newer compatible dependencies and make
# the release commit differ from the candidate checked immediately beforehand.
node "$ROOT/scripts/sync-package-lock-workspaces.mjs" "$ROOT"
node "$ROOT/scripts/check-lockfile.mjs"
echo "  ✓ package-lock.json"

echo "Regenerating deno.lock …"
(cd "$ROOT" && deno install --frozen=false --quiet 2>/dev/null || true)
echo "  ✓ deno.lock"

echo ""
echo "Done. All manifests and lock files updated to $NEW."

# ── Self-check: catch version drift before it reaches CI ──────────────────────
echo ""
echo "Verifying …"
ERRORS=0

# Every publishable Cargo.toml with a version field must say $NEW
CARGO_FILES=(
  "$ROOT/Cargo.toml"
  "$ROOT/packages/iroh-http-tauri/Cargo.toml"
)
for file in "${CARGO_FILES[@]}"; do
  ver=$(grep '^version = ' "$file" | head -1 | sed 's/version = "\(.*\)"/\1/')
  if [[ "$ver" != "$NEW" ]]; then
    echo "  ✗ $file has version $ver (expected $NEW)"
    ERRORS=$((ERRORS + 1))
  fi
done

# Every path dep must carry a version = "$NEW"
for cargo_file in \
  "$ROOT/crates/iroh-http-adapter/Cargo.toml" \
  "$ROOT/crates/iroh-http-discovery/Cargo.toml" \
  "$ROOT/packages/iroh-http-deno/Cargo.toml" \
  "$ROOT/packages/iroh-http-node/Cargo.toml" \
  "$ROOT/packages/iroh-http-tauri/Cargo.toml"; do
  while IFS= read -r line; do
    if ! echo "$line" | grep -q "version = \"$NEW\""; then
      echo "  ✗ $cargo_file: path dep without version $NEW: $line"
      ERRORS=$((ERRORS + 1))
    fi
  done < <(grep 'iroh-http.*path *= *"' "$cargo_file" 2>/dev/null || true)
done

# Every package.json (except root) must say $NEW
for f in packages/iroh-http-shared/package.json \
         packages/iroh-http-node/package.json \
         packages/iroh-http-tauri/package.json; do
  pv=$(grep '"version"' "$ROOT/$f" | head -1 | sed 's/.*"\([0-9][^"]*\)".*/\1/')
  if [[ "$pv" != "$NEW" ]]; then
    echo "  ✗ $f has version $pv (expected $NEW)"
    ERRORS=$((ERRORS + 1))
  fi
done

# npm lockfile must parse cleanly
if ! (cd "$ROOT" && npm ci --omit=optional --dry-run --ignore-scripts) >/dev/null 2>&1; then
  echo "  ✗ package-lock.json fails npm ci --dry-run"
  ERRORS=$((ERRORS + 1))
fi

if [[ "$ERRORS" -gt 0 ]]; then
  echo ""
  echo "  ✗ $ERRORS problem(s) found. Fix before committing."
  exit 1
fi

echo "  ✓ All versions consistent at $NEW"
echo "  ✓ All lockfiles valid"

echo ""
echo "Review:  git diff --stat"
