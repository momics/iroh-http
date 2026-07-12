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
        // F2: take the previous handle, install the done signal, observe/clear
        // the early-stop flag, and store the new handle all under ONE
        // continuous hold of the `serve_handle` lock. Every step is
        // non-blocking (no await), so the critical section stays short.
        //
        // Releasing the lock between `take()` and the final store previously
        // left `serve_handle == None` for a window in which a concurrent
        // `stop_serve()` (an independent Tauri command fired by JS abort/close
        // at any time) would take its None branch and set
        // `serve_stopped_early = true`. But this method had already read
        // `was_stopped = false`, so the new loop was never shut down — the stop
        // was lost and the loop ran forever; worse, the now-stuck flag would
        // spuriously shut down the NEXT serve. `stop_serve()` sets the flag
        // only while holding `serve_handle`, so holding it here forces a racing
        // `stop_serve()` to either (a) run first and be observed via the flag
        // swap below, or (b) block until the new handle is stored and then shut
        // *it* down. Either way the stop is honoured exactly once.
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

        // Clear the early-stop flag and check if it was set. Serialized with
        // `stop_serve()`'s flag-set via the `serve_handle` lock we hold, so this
        // read-and-clear can never miss a concurrent stop.
        let was_stopped = self
            .inner
            .session
            .serve_stopped_early
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        if was_stopped {
            handle.shutdown();
        }

        *guard = Some(handle);
    }

    /// Signal the serve loop to stop accepting new connections.
    pub fn stop_serve(&self) {
        self.clear_local_service();
        let guard = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(h) = guard.as_ref() {
            h.shutdown();
        } else {
            // Handle not registered yet — set a flag so that
            // `set_serve_handle` will shut down as soon as it arrives.
            self.inner
                .session
                .serve_stopped_early
                .store(true, std::sync::atomic::Ordering::Release);
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
