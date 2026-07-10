#!/usr/bin/env bash
# Shared helpers for the mobile native-compile CI jobs (issue #333).
#
# These jobs compile the Swift (iOS) and Kotlin (Android) plugin sources
# against the *matching* Tauri mobile API — the version this crate's Cargo.lock
# already resolves — so a signature/API drift on the native side of the
# Rust ↔ Swift/Kotlin FFI boundary fails CI instead of only surfacing on-device.
#
# Neither job needs a device, a network, the NDK, or a full Rust mobile
# cross-compile: the Tauri mobile API sources ship inside the `tauri` crate
# (`mobile/ios-api`, `mobile/android`), so we `cargo fetch` the crate and read
# its on-disk location out of `cargo metadata`.
set -euo pipefail

# Directory of this crate (packages/iroh-http-tauri), regardless of caller cwd.
tauri_plugin_dir() {
  cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd
}

# Absolute path to the resolved `tauri` crate source directory.
#
# Requires the registry to be populated first (`cargo fetch`). Uses python3,
# which is preinstalled on GitHub's macOS and Ubuntu runners.
tauri_crate_src_dir() {
  local manifest="$1"
  cargo metadata --format-version 1 --manifest-path "$manifest" \
    | python3 -c '
import json, os, sys
meta = json.load(sys.stdin)
pkgs = [p for p in meta["packages"] if p["name"] == "tauri"]
if not pkgs:
    sys.exit("could not find the `tauri` package in cargo metadata")
# Prefer the version actually resolved for this crate (there may be several in
# the graph); cargo metadata lists the resolved set, so pick the highest.
pkgs.sort(key=lambda p: p["version"])
print(os.path.dirname(pkgs[-1]["manifest_path"]))
'
}
