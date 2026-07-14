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
        // Preserve the graceful-drain contract: an in-flight handler may still
        // need DNS. Stop resolver helpers only after every handler has drained,
        // but before closing the transport itself.
        self.inner.transport.shutdown_scoped_dns_proxies();
        self.clear_local_service();
        self.inner.transport.ep.close().await;
        let _ = self.inner.session.closed_tx.send(true);
    }

    /// Immediate close: abort the serve loop with no drain period.
    pub async fn close_force(&self) {
        self.inner.transport.shutdown_scoped_dns_proxies();
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

    /// Install a newly-created serve cycle in the endpoint's identity-scoped
    /// slot. Called by the common pure-Rust server path before it spawns the
    /// accept task, so lifecycle control never depends on an adapter handing
    /// the handle back later.
    pub(crate) fn register_serve_handle(&self, handle: crate::http::server::ServeHandle) {
        let mut guard = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        if guard
            .as_ref()
            .is_some_and(|current| current.is_same_cycle(&handle))
        {
            return;
        }

        if let Some(prev) = guard.take() {
            prev.shutdown_and_close();
        }

        // A pure-Rust service has no FFI self-dispatch target, while an FFI
        // service installs its new weak target only after this token is active.
        // Clearing here prevents a replacement cycle from briefly routing
        // self-requests through the previous handler.
        self.clear_local_service();

        *self
            .inner
            .session
            .serve_done_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle.subscribe_done());
        *guard = Some(handle);
    }

    /// Confirm the serve handle returned to an adapter.
    ///
    /// The common server path already registered this token before returning.
    /// A matching hand-off is therefore a no-op; a different token is a late
    /// adapter hand-off for an already-replaced cycle and is shut down without
    /// disturbing the current cycle.
    pub fn set_serve_handle(&self, handle: crate::http::server::ServeHandle) {
        let guard = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard
            .as_ref()
            .is_some_and(|current| current.is_same_cycle(&handle))
        {
            return;
        }
        handle.shutdown_and_close();
    }

    /// Signal the serve loop to stop accepting new connections.
    pub fn stop_serve(&self) {
        // Identity-scoped: the lock selects one concrete cycle, and both the
        // local service clear and shutdown apply to that same token. A later
        // serve call creates a new token and is not poisoned by this stop.
        let guard = self
            .inner
            .session
            .serve_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.clear_local_service();
        if let Some(h) = guard.as_ref() {
            self.inner.session.record_stopped_token(h.token());
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
