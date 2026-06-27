#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // `.with_scheme()` registers `httpi://` as a native webview URI scheme so
        // `<img>`, `<audio>`, and `<video>` can load peer assets directly — see
        // the Audio tab for a live demo.
        .plugin(tauri_plugin_iroh_http::builder().with_scheme().build())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
