#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();
    tauri::Builder::default()
        // `.with_scheme()` registers `httpi://` as a native webview URI scheme so
        // `<img>`, `<audio>`, and `<video>` can load peer assets directly — see
        // the Stream files tab for a live demo.
        .plugin(tauri_plugin_iroh_http::builder().with_scheme().build())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Install a `tracing` subscriber so iroh's connectivity, relay, and discovery
/// logs are visible while diagnosing fetch failures. On Android the events go to
/// logcat (`adb logcat`), on desktop to stdout. Override the default verbosity
/// with the `RUST_LOG` env var (desktop) — e.g. `RUST_LOG=iroh=trace`.
fn init_tracing() {
    use tracing_subscriber::prelude::*;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "info,iroh=debug,iroh_http_core=debug,tauri_plugin_iroh_http=debug",
        )
    });

    let registry = tracing_subscriber::registry().with(filter);

    #[cfg(target_os = "android")]
    let registry = registry.with(paranoid_android::layer(env!("CARGO_PKG_NAME")));
    #[cfg(not(target_os = "android"))]
    let registry = registry.with(tracing_subscriber::fmt::layer());

    // `try_init` so a second call (e.g. mobile re-entry) is a harmless no-op.
    let _ = registry.try_init();
}
