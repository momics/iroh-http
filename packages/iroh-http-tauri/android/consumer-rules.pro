# Consumer ProGuard rules for the iroh-http Tauri plugin.
#
# Applied to apps that depend on this library. The plugin's command handlers
# are invoked reflectively by name across the Rust ↔ Kotlin FFI boundary
# (see `@Command` handlers in IrohHttpPlugin.kt), so keep them and the plugin
# entry point from being stripped or renamed by R8/ProGuard.
-keep class com.iroh.http.** { *; }
