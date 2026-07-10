#!/usr/bin/env bash
# CI: compile the Android Kotlin plugin sources against the Tauri Android API
# (#333).
#
# Build-only, no device/emulator, no network beyond Gradle dependency fetch, no
# NDK, no Rust cross-compile. The Tauri Android library (`mobile/android`,
# shipped inside the `tauri` crate) plus this crate's `android/` module are
# wired into the standalone Gradle harness under `android-ci/`, then we run
# `:tauri-plugin-iroh-http:assembleDebug`. This catches renamed/removed
# `@Command` handlers, changed signatures, and Tauri API misuse.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-mobile-ci.sh
source "$here/lib-mobile-ci.sh"

plugin_dir="$(tauri_plugin_dir)"
cd "$plugin_dir"

: "${ANDROID_HOME:?ANDROID_HOME must point at the Android SDK}"

echo "==> Fetching crates so the Tauri Android library is available on disk"
cargo fetch --manifest-path Cargo.toml

tauri_src="$(tauri_crate_src_dir "$plugin_dir/Cargo.toml")"
android_lib_src="$tauri_src/mobile/android"
echo "==> Tauri Android library: $android_lib_src"
[ -f "$android_lib_src/build.gradle.kts" ] || {
  echo "error: $android_lib_src/build.gradle.kts not found" >&2
  exit 1
}

# The crate registry is read-only, but Gradle writes a build/ dir into every
# included project. Copy the Tauri Android library to a writable location.
work="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/iroh-http-tauri-android-ci"
android_lib="$work/tauri-android"
rm -rf "$android_lib"
mkdir -p "$android_lib"
cp -R "$android_lib_src/." "$android_lib/"
chmod -R u+w "$android_lib"
rm -rf "$android_lib/build"

harness="$plugin_dir/android-ci"
echo "==> gradle :tauri-plugin-iroh-http:assembleDebug"
gradle \
  --project-dir "$harness" \
  --no-daemon \
  --console=plain \
  "-DtauriAndroidDir=$android_lib" \
  :tauri-plugin-iroh-http:assembleDebug

echo "==> Android Kotlin plugin compiled successfully"
