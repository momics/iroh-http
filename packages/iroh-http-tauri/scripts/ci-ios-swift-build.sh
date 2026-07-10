#!/usr/bin/env bash
# CI: compile the iOS Swift plugin sources against the Tauri iOS API (#333).
#
# Build-only, no device/simulator boot, no network, no Rust cross-compile. We
# materialize the Tauri Swift API package (`mobile/ios-api`, shipped inside the
# `tauri` crate) into the path `ios/Package.swift` expects (`../.tauri/tauri-api`
# — the same location `tauri ios build` populates) and then `swift build`
# targeting the iOS-simulator SDK. This catches renamed/removed `@objc`
# handlers, changed signatures, and Tauri API misuse.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-mobile-ci.sh
source "$here/lib-mobile-ci.sh"

plugin_dir="$(tauri_plugin_dir)"
cd "$plugin_dir"

echo "==> Fetching crates so the Tauri iOS API is available on disk"
cargo fetch --manifest-path Cargo.toml

tauri_src="$(tauri_crate_src_dir "$plugin_dir/Cargo.toml")"
ios_api="$tauri_src/mobile/ios-api"
echo "==> Tauri iOS API: $ios_api"
[ -d "$ios_api/Sources" ] || {
  echo "error: $ios_api/Sources not found" >&2
  exit 1
}

# Materialize ../.tauri/tauri-api exactly where ios/Package.swift resolves its
# `Tauri` dependency. `.tauri/` is gitignored and normally created by
# `tauri ios build`; we create it directly from the resolved crate source.
api_dest="$plugin_dir/.tauri/tauri-api"
rm -rf "$plugin_dir/.tauri"
mkdir -p "$api_dest"
cp -R "$ios_api/Sources" "$ios_api/Package.swift" "$api_dest/"

sdk="$(xcrun --sdk iphonesimulator --show-sdk-path)"
echo "==> swift build (iphonesimulator SDK: $sdk)"
cd "$plugin_dir/ios"
swift build \
  --sdk "$sdk" \
  -Xswiftc -target -Xswiftc arm64-apple-ios14.0-simulator

echo "==> iOS Swift plugin compiled successfully"
