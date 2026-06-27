/// Terminate the host process with the given exit code.
///
/// Invoked by the shared runner (`tests/runners/tauri.ts`) in `?ci=true` mode.
/// The WebdriverIO spec drives the app without that flag and reads the result
/// summary off `window` instead, but the command is registered so the runner's
/// documented CI contract stays functional.
#[tauri::command]
fn exit_with_code(code: i32) {
    std::process::exit(code);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_iroh_http::init())
        .invoke_handler(tauri::generate_handler![exit_with_code])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
