// Standalone Gradle harness to compile the iroh-http Tauri plugin's Android
// (Kotlin) sources in CI, without a full Tauri app build (issue #333).
//
// It wires two Android library modules:
//   :tauri-android            — the Tauri Android API, resolved from the `tauri`
//                               crate's `mobile/android` and passed in via
//                               -DtauriAndroidDir (see scripts/ci-android-gradle-build.sh)
//   :tauri-plugin-iroh-http   — this crate's android/ module (the code under test)
//
// Plugin versions are pinned to what `tauri android init` currently generates,
// so the harness compiles the Kotlin the same way a real Tauri app would.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    plugins {
        id("com.android.library") version "8.11.0"
        id("org.jetbrains.kotlin.android") version "1.9.25"
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.PREFER_SETTINGS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "iroh-http-android-ci"

val tauriAndroidDir: String = providers.systemProperty("tauriAndroidDir").orNull
    ?: throw GradleException(
        "Set -DtauriAndroidDir=<path to the tauri crate's mobile/android>. " +
            "Run via scripts/ci-android-gradle-build.sh, which resolves it from cargo metadata."
    )

include(":tauri-android")
project(":tauri-android").projectDir = File(tauriAndroidDir)

include(":tauri-plugin-iroh-http")
project(":tauri-plugin-iroh-http").projectDir = File(rootDir, "../android")
