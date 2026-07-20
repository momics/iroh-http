#!/usr/bin/env bash
set -euo pipefail

# Verify that a release tag agrees with every independently versioned package
# and every pinned internal dependency. This is intentionally read-only and is
# run both by ordinary CI and immediately before registry publication.

ROOT="${RELEASE_CHECK_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
TAG="${1:-}"

if ! [[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "Error: expected a release tag such as v0.6.0, got '${TAG:-<empty>}'" >&2
  exit 1
fi

VERSION="${TAG#v}"
MAJOR_MINOR="$(printf '%s' "$VERSION" | sed -E 's/^([0-9]+\.[0-9]+).*/\1/')"
ERRORS=0

fail() {
  echo "  x $*" >&2
  ERRORS=$((ERRORS + 1))
}

check_value() {
  local label="$1" actual="$2" expected="$3"
  if [[ "$actual" != "$expected" ]]; then
    fail "$label is '$actual' (expected '$expected')"
  fi
}

toml_version() {
  sed -n 's/^version = "\([^"]*\)"/\1/p' "$1" | head -1
}

json_version() {
  node -e 'const p=require(process.argv[1]); process.stdout.write(p.version ?? "")' "$1"
}

check_value "Cargo workspace version" "$(toml_version "$ROOT/Cargo.toml")" "$VERSION"
check_value "Tauri Rust package version" "$(toml_version "$ROOT/packages/iroh-http-tauri/Cargo.toml")" "$VERSION"

JSON_MANIFESTS=(
  packages/iroh-http-shared/package.json
  packages/iroh-http-node/package.json
  packages/iroh-http-tauri/package.json
  packages/iroh-http-node/npm/darwin-arm64/package.json
  packages/iroh-http-node/npm/darwin-x64/package.json
  packages/iroh-http-node/npm/linux-arm64-gnu/package.json
  packages/iroh-http-node/npm/linux-x64-gnu/package.json
  packages/iroh-http-node/npm/win32-x64-msvc/package.json
  packages/iroh-http-shared/deno.json
)

for manifest in "${JSON_MANIFESTS[@]}"; do
  check_value "$manifest version" "$(json_version "$ROOT/$manifest")" "$VERSION"
done

DENO_VERSION="$(sed -n 's/^[[:space:]]*"version": "\([^"]*\)".*/\1/p' "$ROOT/packages/iroh-http-deno/deno.jsonc" | head -1)"
check_value "packages/iroh-http-deno/deno.jsonc version" "$DENO_VERSION" "$VERSION"

ADAPTER_VERSION="$(sed -n 's/^const VERSION = "\([^"]*\)";.*/\1/p' "$ROOT/packages/iroh-http-deno/src/adapter.ts" | head -1)"
check_value "Deno adapter VERSION" "$ADAPTER_VERSION" "$VERSION"

PATH_DEP_MANIFESTS=(
  crates/iroh-http-adapter/Cargo.toml
  crates/iroh-http-discovery/Cargo.toml
  packages/iroh-http-deno/Cargo.toml
  packages/iroh-http-node/Cargo.toml
  packages/iroh-http-tauri/Cargo.toml
)

for manifest in "${PATH_DEP_MANIFESTS[@]}"; do
  while IFS= read -r line; do
    if [[ "$line" != *"version = \"$VERSION\""* ]]; then
      fail "$manifest has an internal path dependency not pinned to $VERSION: $line"
    fi
  done < <(grep 'iroh-http.*path *= *"' "$ROOT/$manifest" || true)
done

REQUIRED_PATH_DEPS=(
  'crates/iroh-http-adapter/Cargo.toml|iroh-http-core'
  'crates/iroh-http-discovery/Cargo.toml|iroh-http-core'
  'packages/iroh-http-deno/Cargo.toml|iroh-http-core'
  'packages/iroh-http-deno/Cargo.toml|iroh-http-adapter'
  'packages/iroh-http-deno/Cargo.toml|iroh-http-discovery'
  'packages/iroh-http-node/Cargo.toml|iroh-http-core'
  'packages/iroh-http-node/Cargo.toml|iroh-http-adapter'
  'packages/iroh-http-node/Cargo.toml|iroh-http-discovery'
  'packages/iroh-http-tauri/Cargo.toml|iroh-http-core'
  'packages/iroh-http-tauri/Cargo.toml|iroh-http-adapter'
  'packages/iroh-http-tauri/Cargo.toml|iroh-http-discovery'
)

for requirement in "${REQUIRED_PATH_DEPS[@]}"; do
  manifest="${requirement%%|*}"
  dependency="${requirement#*|}"
  if ! grep -Eq "^${dependency}[[:space:]]*=.*path[[:space:]]*=" "$ROOT/$manifest"; then
    fail "$manifest is missing required internal dependency $dependency"
  fi
done

check_value "Node shared dependency range" \
  "$(node -e 'const p=require(process.argv[1]); process.stdout.write(p.dependencies["@momics/iroh-http-shared"] ?? "")' "$ROOT/packages/iroh-http-node/package.json")" \
  "^$MAJOR_MINOR"
check_value "Tauri shared dependency range" \
  "$(node -e 'const p=require(process.argv[1]); process.stdout.write(p.dependencies["@momics/iroh-http-shared"] ?? "")' "$ROOT/packages/iroh-http-tauri/package.json")" \
  "^$MAJOR_MINOR"

DENO_SHARED_RANGE="$(sed -n 's|.*jsr:@momics/iroh-http-shared@\([^\"]*\).*|\1|p' "$ROOT/packages/iroh-http-deno/deno.jsonc" | head -1)"
check_value "Deno shared dependency range" "$DENO_SHARED_RANGE" "^$MAJOR_MINOR"

NODE_PLATFORM_VERSIONS="$(node -e '
  const p=require(process.argv[1]);
  for (const [name, version] of Object.entries(p.optionalDependencies ?? {})) {
    if (name.startsWith("@momics/iroh-http-node-")) console.log(`${name}\t${version}`);
  }
' "$ROOT/packages/iroh-http-node/package.json")"

EXPECTED_NODE_PLATFORMS="$(printf '%s\n' \
  '@momics/iroh-http-node-darwin-arm64' \
  '@momics/iroh-http-node-darwin-x64' \
  '@momics/iroh-http-node-linux-arm64-gnu' \
  '@momics/iroh-http-node-linux-x64-gnu' \
  '@momics/iroh-http-node-win32-x64-msvc')"
ACTUAL_NODE_PLATFORMS="$(printf '%s\n' "$NODE_PLATFORM_VERSIONS" | cut -f1 | sed '/^$/d' | sort)"
if [[ "$ACTUAL_NODE_PLATFORMS" != "$EXPECTED_NODE_PLATFORMS" ]]; then
  fail "Node optionalDependencies do not contain the exact five supported platform packages"
fi

while IFS=$'\t' read -r name dependency_version; do
  [[ -z "$name" ]] && continue
  check_value "$name optional dependency" "$dependency_version" "$VERSION"
done <<< "$NODE_PLATFORM_VERSIONS"

if [[ "$ERRORS" -ne 0 ]]; then
  echo "Release version check failed with $ERRORS problem(s)." >&2
  exit 1
fi

echo "Release tag $TAG matches all publishable manifests and internal dependency pins."
