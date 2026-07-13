//! Endpoint lifecycle: close, drain, serve-handle wiring, and events.
//!
//! Extracted from `mod.rs` to keep the façade ≤ 200 LoC (ADR-014 D1 AC #4).
//! All methods here operate on [`SessionRuntime`] and [`Transport`] fields
//! that live inside [`EndpointInner`].

use super::IrohEndpoint;

impl IrohEndpoint {
    /// Graceful close: signal the serve loop to stop accepting, wait for
    /// in-flight requests to drain, then close the QUIC endpoint.
    pub async fn close(&self) {
        let handle = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(h) = handle {
            h.drain().await;
        }
        self.clear_local_service();
        self.inner.transport.ep.close().await;
        let _ = self.inner.session.closed_tx.send(true);
    }

    /// Immediate close: abort the serve loop with no drain period.
    pub async fn close_force(&self) {
        let handle = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(h) = handle {
            h.abort();
        }
        self.clear_local_service();
        self.inner.transport.ep.close().await;
        let _ = self.inner.session.closed_tx.send(true);
    }

    /// Wait until this endpoint has been closed. Returns immediately if already closed.
    pub async fn wait_closed(&self) {
        let mut rx = self.inner.session.closed_rx.clone();
        let _ = rx.wait_for(|v| *v).await;
    }

    /// Store a serve handle so that `close()` can drain it.
    ///
    /// If `stop_serve()` was called before this (the JS abort raced ahead of
    /// the napi async setup), the handle is immediately shut down so the
    /// serve loop drains without a second `stop_serve()` call.
    pub fn set_serve_handle(&self, handle: crate::http::server::ServeHandle) {
        // #336: an endpoint has at most one active serve loop. If serve() is
        // restarted without an explicit stop_serve() first — e.g. the iOS
        // foreground recovery path re-serves after the app returns to the
        // foreground — the previously stored ServeHandle would just be dropped.
        // ServeHandle has no Drop, so the old accept loop keeps running and
        // competes with the new one for `ep.accept()`. Incoming requests then
        // land on the stale loop's handler and get its old response (the
        // "half-live serve answers a blanket 200 instead of the routed status"
        // symptom). Shut the previous loop down so only the new one accepts.
        //
        // Take the previous handle, install the done signal, evaluate the stop
        // generation, and store the new handle all under ONE continuous hold of
        // the `serve_handle` lock. Every step is non-blocking (no await), so the
        // critical section stays short.
        //
        // F7: register the new handle under the same `serve_handle` lock that
        // `set_local_service` bumped `serve_started_gen` under and that
        // `stop_serve` records `serve_stopped_gen` under. `my_gen` is this
        // cycle's generation; if a `stop_serve` has been requested for this
        // generation or a later one (`serve_stopped_gen >= my_gen`), shut the
        // new handle down. This subsumes the old early-stop boolean and also
        // closes the restart-with-concurrent-stop race: a stop that targeted
        // the stale previous handle still stops this cycle because it recorded a
        // generation >= `my_gen`.
        let mut guard = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        if let Some(prev) = guard.take() {
            prev.shutdown_and_close();
        }

        *self
            .inner
            .session
            .serve_done_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle.subscribe_done());

        let my_gen = self
            .inner
            .session
            .serve_started_gen
            .load(std::sync::atomic::Ordering::SeqCst);
        let stopped_gen = self
            .inner
            .session
            .serve_stopped_gen
            .load(std::sync::atomic::Ordering::SeqCst);
        if stopped_gen > 0 && stopped_gen >= my_gen {
            handle.shutdown();
        }

        *guard = Some(handle);
    }

    /// Signal the serve loop to stop accepting new connections.
    pub fn stop_serve(&self) {
        // F7: clear the local service, record the stop generation, and shut the
        // registered handle (if any) under ONE hold of the `serve_handle` lock —
        // the same lock `set_local_service` and `set_serve_handle` take. This
        // serializes the stop with a concurrent restart so its side effects
        // (service clear + generation record) cannot straddle the new cycle's
        // start. `serve_stopped_gen` is raised to the current `serve_started_gen`
        // so a not-yet-registered new handle is shut down when it arrives.
        let guard = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.clear_local_service();
        let g = self
            .inner
            .session
            .serve_started_gen
            .load(std::sync::atomic::Ordering::SeqCst);
        self.inner
            .session
            .serve_stopped_gen
            .fetch_max(g, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = guard.as_ref() {
            h.shutdown();
        }
    }

    /// Wait until the serve loop has fully exited.
    pub async fn wait_serve_stop(&self) {
        let rx = self
            .inner
            .session
            .serve_done_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(mut rx) = rx {
            let _ = rx.wait_for(|v| *v).await;
        }
    }

    /// Take the transport event receiver, handing it off to a platform drain task.
    /// May only be called once per endpoint.
    pub fn subscribe_events(
        &self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::http::events::TransportEvent>> {
        self.inner
            .session
            .event_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}
