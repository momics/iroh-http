#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
plugin="${ANDROID_PLUGIN_SOURCE:-$here/../android/src/main/java/com/iroh/http/IrohHttpPlugin.kt}"
out="${TMPDIR:-/tmp}/iroh-http-android-contract-tests.jar"

kotlinc \
  "$here/stubs/android/content/Context.kt" \
  "$here/stubs/android/app/Activity.kt" \
  "$here/stubs/android/net/Network.kt" \
  "$here/stubs/android/net/nsd/NsdManager.kt" \
  "$here/stubs/android/os/Build.kt" \
  "$here/stubs/android/os/Handler.kt" \
  "$here/stubs/android/util/Log.kt" \
  "$here/stubs/app/tauri/annotation/Annotations.kt" \
  "$here/stubs/app/tauri/plugin/Plugin.kt" \
  "$here/stubs/org/json/Json.kt" \
  "$plugin" \
  "$here/IrohHttpPluginContractHarness.kt" \
  -nowarn \
  -include-runtime \
  -d "$out"

kotlin -classpath "$out" com.iroh.http.IrohHttpPluginContractHarnessKt
