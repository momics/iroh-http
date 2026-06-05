#![deny(unsafe_code)]

mod commands;
mod state;

#[cfg(test)]
mod tests;

#[cfg(mobile)]
pub mod mobile_mdns;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the plugin with default settings (no `httpi://` scheme handler).
///
/// Equivalent to `builder().build()`.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    builder().build()
}

/// Returns a [`PluginBuilder`] for configuring the plugin before calling
/// [`.build()`](PluginBuilder::build).
///
/// # Example
/// ```rust,no_run
/// tauri::Builder::default()
///     .plugin(tauri_plugin_iroh_http::builder().with_scheme().build());
/// ```
pub fn builder<R: Runtime>() -> PluginBuilder<R> {
    PluginBuilder::new()
}

/// Fluent builder for the iroh-http Tauri plugin.
pub struct PluginBuilder<R: Runtime> {
    scheme: bool,
    _runtime: std::marker::PhantomData<R>,
}

impl<R: Runtime> PluginBuilder<R> {
    fn new() -> Self {
        Self { scheme: false, _runtime: std::marker::PhantomData }
    }

    /// Register `httpi://` as a native webview URI scheme protocol.
    ///
    /// When enabled, the webview engine resolves `httpi://` URLs directly
    /// through iroh-http-core — `<img>`, `<audio>`, `<video>`, CSS `url()`,
    /// and `fetch("httpi://…")` all work without any JavaScript bridging.
    ///
    /// The handler auto-binds to the first endpoint created via
    /// `createEndpoint()`. There is nothing else to configure.
    pub fn with_scheme(mut self) -> Self {
        self.scheme = true;
        self
    }

    /// Finalise and produce the [`TauriPlugin`].
    pub fn build(self) -> TauriPlugin<R> {
        let mut plugin_builder = Builder::new("iroh-http").invoke_handler(tauri::generate_handler![
            commands::create_endpoint,
            commands::close_endpoint,
            commands::ping,
            commands::node_addr,
            commands::node_ticket,
            commands::home_relay,
            commands::peer_info,
            commands::peer_stats,
            commands::endpoint_stats,
            commands::next_chunk,
            commands::try_next_chunk,
            commands::send_chunk,
            commands::finish_body,
            commands::cancel_request,
            commands::create_body_writer,
            commands::create_fetch_token,
            commands::cancel_in_flight,
            commands::fetch,
            commands::serve,
            commands::stop_serve,
            commands::wait_serve_stop,
            commands::wait_endpoint_closed,
            commands::respond_to_request,
            commands::secret_key_sign,
            commands::public_key_verify,
            commands::generate_secret_key,
            commands::mdns_browse,
            commands::mdns_next_event,
            commands::mdns_browse_close,
            commands::mdns_advertise,
            commands::mdns_advertise_close,
            commands::session_connect,
            commands::session_accept,
            commands::session_create_bidi_stream,
            commands::session_next_bidi_stream,
            commands::session_close,
            commands::session_closed,
            commands::session_create_uni_stream,
            commands::session_next_uni_stream,
            commands::session_send_datagram,
            commands::session_recv_datagram,
            commands::session_max_datagram_size,
            commands::start_transport_events,
        ]);

        // Register the `httpi://` scheme protocol before `.build()` — Tauri
        // requires schemes to be declared at builder time.
        if self.scheme {
            plugin_builder = plugin_builder
                .register_asynchronous_uri_scheme_protocol("httpi", |ctx, request, responder| {
                    let app = ctx.app_handle().clone();
                    // Hand off to the existing tokio runtime; never block the
                    // protocol callback thread.
                    tauri::async_runtime::spawn(async move {
                        let response = scheme::handle(&app, request).await;
                        responder.respond(response);
                    });
                });
        }

        plugin_builder
            .setup(move |app, _api| {
                if self.scheme {
                    // Manage the state slot so `create_endpoint` can auto-bind.
                    app.manage(state::SchemeState::new());
                }
                #[cfg(mobile)]
                {
                    // ISS-009: return recoverable error instead of panicking on init failure.
                    let mdns = mobile_mdns::init(app, _api).map_err(|e| e.into())?;
                    app.manage(mdns);
                }
                Ok(())
            })
            // ISS-079: close all registered endpoints when the *last* webview is
            // destroyed to prevent QUIC socket leaks on window close without an
            // explicit JS `closeEndpoint` call.
            //
            // We count the remaining windows *after* the destroyed event fires.
            // When that count reaches zero, no webview is left running, so it is
            // safe to tear down all endpoints.  In multi-window apps this means
            // closing window A does not affect window B's networking.
            .on_event(|app, event| {
                if let tauri::RunEvent::WindowEvent {
                    event: tauri::WindowEvent::Destroyed,
                    ..
                } = event
                {
                    if app.webview_windows().is_empty() {
                        iroh_http_core::registry::close_all_endpoints();
                    }
                }
            })
            .build()
    }
}

// ── Scheme handler ────────────────────────────────────────────────────────────

mod scheme {
    use tauri::http::{Response, StatusCode};
    use tauri::{Manager, Runtime};

    /// Handle a single `httpi://` request from the webview engine.
    ///
    /// Resolves the active endpoint from [`crate::state::SchemeState`], parses
    /// the peer node ID from the URI host, forwards any `Range` header, and
    /// drains the iroh-http-core body into the response bytes.
    pub async fn handle<R: Runtime>(
        app: &tauri::AppHandle<R>,
        request: tauri::http::Request<Vec<u8>>,
    ) -> tauri::http::Response<Vec<u8>> {
        match fetch(app, request).await {
            Ok(r) => r,
            Err(msg) => error_response(StatusCode::BAD_GATEWAY, &msg),
        }
    }

    async fn fetch<R: Runtime>(
        app: &tauri::AppHandle<R>,
        request: tauri::http::Request<Vec<u8>>,
    ) -> Result<Response<Vec<u8>>, String> {
        // ── 0. Gate on GET only ──────────────────────────────────────────────
        // <img>, <audio>, <video> are always GET. Non-GET callers should use
        // the node.fetch() IPC command instead of the scheme handler.
        if request.method() != tauri::http::Method::GET {
            return Ok(Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header("allow", "GET")
                .header("content-type", "text/plain")
                .body(
                    b"httpi:// scheme handler is GET-only; use node.fetch() for other methods"
                        .to_vec(),
                )
                .map_err(|e| e.to_string())?);
        }

        // ── 1. Resolve the active endpoint ───────────────────────────────────
        let scheme_state = app
            .try_state::<crate::state::SchemeState>()
            .ok_or_else(|| "httpi:// scheme handler is not configured".to_string())?;

        let ep_handle = scheme_state
            .active_handle()
            .ok_or_else(|| "httpi:// scheme handler: no endpoint bound yet".to_string())?;

        let ep = crate::state::get_endpoint(ep_handle)
            .ok_or_else(|| "httpi:// scheme handler: active endpoint was closed".to_string())?;

        // ── 2. Parse peer node ID from the URI host ──────────────────────────
        // Platform origin differences (ISS-217):
        //   macOS / Linux / iOS  → httpi://<nodeid>/path
        //   Windows / Android    → http://httpi.localhost/path  (Tauri rewrites)
        // In both cases the node ID comes from the host component.
        let uri = request.uri();
        let node_id = uri
            .host()
            .ok_or_else(|| format!("httpi:// scheme handler: missing host in URI: {uri}"))?;

        // Strip the ".localhost" suffix added by Tauri on Windows/Android.
        let node_id = node_id.trim_end_matches(".localhost");

        // ── 3. Reconstruct the httpi:// URL for iroh-http-core ───────────────
        let path_and_query = uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let core_url = format!("httpi://{node_id}{path_and_query}");

        // ── 4. Collect headers — forward Range for seeking support ───────────
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                let name = k.as_str().to_lowercase();
                // Only forward safe/useful headers; drop host (we set it above).
                if matches!(
                    name.as_str(),
                    "range" | "accept" | "accept-encoding" | "if-none-match" | "if-modified-since"
                ) {
                    v.to_str().ok().map(|val| (name, val.to_string()))
                } else {
                    None
                }
            })
            .collect();

        // ── 5. Fetch via iroh-http-core (reuses the same path as commands::fetch) ──
        let res = iroh_http_core::fetch(
            &ep,
            node_id,
            &core_url,
            "GET",
            &headers,
            None,  // no request body for scheme handler requests
            None,  // no fetch cancellation token
            None,  // no extra direct addrs
            None,  // use endpoint default timeout
            true,  // decompress
            None,  // no response body size cap
        )
        .await
        .map_err(|e| e.to_string())?;

        // ── 6. Drain the streaming body handle into bytes ────────────────────
        let mut body_bytes: Vec<u8> = Vec::new();
        if res.body_handle != 0 {
            loop {
                match ep.handles().next_chunk(res.body_handle).await {
                    Ok(Some(chunk)) => body_bytes.extend_from_slice(&chunk),
                    Ok(None) => break, // EOF — handle cleaned up automatically
                    Err(e) => {
                        // Cancel the reader to release the slot immediately
                        // rather than waiting for the TTL sweep.
                        ep.handles().cancel_reader(res.body_handle);
                        return Err(e.to_string());
                    }
                }
            }
        }

        // ── 7. Build the http::Response ──────────────────────────────────────
        let status = StatusCode::from_u16(res.status)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        let mut builder = Response::builder().status(status);

        for (k, v) in &res.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }

        // Ensure Accept-Ranges is set so <audio>/<video> seeking works,
        // but only if the upstream didn't already send it (CORR-005).
        let has_accept_ranges = res
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept-ranges"));
        if !has_accept_ranges && status != StatusCode::PARTIAL_CONTENT {
            builder = builder.header("accept-ranges", "bytes");
        }

        builder.body(body_bytes).map_err(|e| e.to_string())
    }

    fn error_response(status: StatusCode, msg: &str) -> Response<Vec<u8>> {
        Response::builder()
            .status(status)
            .header("content-type", "text/plain")
            .body(msg.as_bytes().to_vec())
            .unwrap_or_else(|_| Response::new(Vec::new()))
    }
}
