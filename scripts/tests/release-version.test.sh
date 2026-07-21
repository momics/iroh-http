#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PASS=0
FAIL=0

WORKSPACE_VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
RELEASE_TAG="v$WORKSPACE_VERSION"

ok() { PASS=$((PASS + 1)); echo "  ✓ $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  ✗ $1"; }

expect_pass() {
  local name="$1" fixture="$2" tag="$3"
  if RELEASE_CHECK_ROOT="$fixture" bash "$ROOT/scripts/check-release-version.sh" "$tag" >/dev/null 2>&1; then
    ok "$name"
  else
    bad "$name"
  fi
}

expect_fail() {
  local name="$1" fixture="$2" tag="$3"
  if RELEASE_CHECK_ROOT="$fixture" bash "$ROOT/scripts/check-release-version.sh" "$tag" >/dev/null 2>&1; then
    bad "$name"
  else
    ok "$name"
  fi
}

new_fixture() {
  local fixture
  fixture="$(mktemp -d)"
  mkdir -p "$fixture/crates/iroh-http-adapter" \
    "$fixture/crates/iroh-http-discovery" \
    "$fixture/packages/iroh-http-deno/src" \
    "$fixture/packages/iroh-http-node/npm" \
    "$fixture/packages/iroh-http-shared" \
    "$fixture/packages/iroh-http-tauri"

  cp "$ROOT/Cargo.toml" "$fixture/Cargo.toml"
  cp "$ROOT/crates/iroh-http-adapter/Cargo.toml" "$fixture/crates/iroh-http-adapter/Cargo.toml"
  cp "$ROOT/crates/iroh-http-discovery/Cargo.toml" "$fixture/crates/iroh-http-discovery/Cargo.toml"
  cp "$ROOT/packages/iroh-http-deno/Cargo.toml" "$fixture/packages/iroh-http-deno/Cargo.toml"
  cp "$ROOT/packages/iroh-http-deno/deno.jsonc" "$fixture/packages/iroh-http-deno/deno.jsonc"
  cp "$ROOT/packages/iroh-http-deno/src/adapter.ts" "$fixture/packages/iroh-http-deno/src/adapter.ts"
  cp "$ROOT/packages/iroh-http-node/Cargo.toml" "$fixture/packages/iroh-http-node/Cargo.toml"
  cp "$ROOT/packages/iroh-http-node/package.json" "$fixture/packages/iroh-http-node/package.json"
  cp -R "$ROOT/packages/iroh-http-node/npm/." "$fixture/packages/iroh-http-node/npm/"
  cp "$ROOT/packages/iroh-http-shared/package.json" "$fixture/packages/iroh-http-shared/package.json"
  cp "$ROOT/packages/iroh-http-shared/deno.json" "$fixture/packages/iroh-http-shared/deno.json"
  cp "$ROOT/packages/iroh-http-tauri/Cargo.toml" "$fixture/packages/iroh-http-tauri/Cargo.toml"
  cp "$ROOT/packages/iroh-http-tauri/package.json" "$fixture/packages/iroh-http-tauri/package.json"
  printf '%s' "$fixture"
}

FIXTURE="$(new_fixture)"
trap 'rm -rf "$FIXTURE" "${FIXTURE_2:-}" "${FIXTURE_3:-}" "${FIXTURE_4:-}"' EXIT

expect_pass "consistent release" "$FIXTURE" "$RELEASE_TAG"
expect_fail "malformed tag" "$FIXTURE" main

FIXTURE_2="$(new_fixture)"
sed -i.bak "s/version = \"$WORKSPACE_VERSION\"/version = \"0.0.0\"/" "$FIXTURE_2/packages/iroh-http-tauri/Cargo.toml"
expect_fail "standalone manifest drift" "$FIXTURE_2" "$RELEASE_TAG"

FIXTURE_3="$(new_fixture)"
sed -i.bak '/iroh-http-core =/d' "$FIXTURE_3/crates/iroh-http-discovery/Cargo.toml"
expect_fail "missing internal dependency" "$FIXTURE_3" "$RELEASE_TAG"

FIXTURE_4="$(new_fixture)"
node -e '
  const fs=require("fs"); const file=process.argv[1]; const p=require(file);
  delete p.optionalDependencies["@momics/iroh-http-node-win32-x64-msvc"];
  fs.writeFileSync(file, JSON.stringify(p, null, 2) + "\n");
' "$FIXTURE_4/packages/iroh-http-node/package.json"
expect_fail "missing Node platform package" "$FIXTURE_4" "$RELEASE_TAG"

echo ""
echo "  $PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]]
