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
        // Clear the early-stop flag and check if it was set.
        let was_stopped = self
            .inner
            .session
            .serve_stopped_early
            .swap(false, std::sync::atomic::Ordering::AcqRel);

        *self
            .inner
            .session
            .serve_done_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle.subscribe_done());

        if was_stopped {
            handle.shutdown();
        }

        *self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle);
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
