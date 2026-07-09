//! Global state managed by the Tauri plugin.

use std::sync::atomic::{AtomicU64, Ordering};

use iroh_http_core::{endpoint::IrohEndpoint, registry};

// ── Endpoint slab (delegates to core registry) ───────────────────────────────

pub fn insert_endpoint(ep: IrohEndpoint) -> u64 {
    registry::insert_endpoint(ep)
}

pub fn get_endpoint(handle: u64) -> Option<IrohEndpoint> {
    registry::get_endpoint(handle)
}

pub fn remove_endpoint(handle: u64) -> Option<IrohEndpoint> {
    registry::remove_endpoint(handle)
}

// ── Scheme handler state ─────────────────────────────────────────────────────

/// Managed state tracking which endpoint the `httpi://` scheme handler
/// resolves requests through. Stored via `app.manage()` only when the
/// plugin is built with [`crate::PluginBuilder::with_scheme`].
///
/// Auto-binds to the first endpoint created; call
/// [`SchemeState::bind`] explicitly (e.g. from `create_endpoint`) to
/// rebind to a different one.
pub struct SchemeState {
    /// 0 = unbound (no endpoint created yet).
    active: AtomicU64,
}

impl SchemeState {
    pub fn new() -> Self {
        Self {
            active: AtomicU64::new(0),
        }
    }

    /// Bind to `handle`. Only stores if nothing is bound yet (first-wins).
    /// Returns `true` if this call performed the bind.
    ///
    /// If the previously stored handle is no longer live in the registry (e.g.
    /// the endpoint was closed), the stale handle is cleared first so the
    /// incoming handle can claim the slot. This means close → recreate works
    /// without any explicit unbind step.
    pub fn bind_if_unbound(&self, handle: u64) -> bool {
        // Lazy stale-handle clearing: if the current active handle is dead,
        // reset to 0 so the new handle can bind (CORR-002).
        let current = self.active.load(Ordering::Acquire);
        if current != 0 && get_endpoint(current).is_none() {
            // compare_exchange: if another thread already cleared or rebound,
            // that outcome is also correct — just proceed to the binding CAS.
            let _ = self
                .active
                .compare_exchange(current, 0, Ordering::AcqRel, Ordering::Relaxed);
        }
        self.active
            .compare_exchange(0, handle, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    /// Explicitly rebind to `handle`, replacing any previous binding.
    /// Useful in multi-endpoint apps that want to swap which endpoint the
    /// `httpi://` scheme handler resolves through.
    #[allow(dead_code)]
    pub fn bind(&self, handle: u64) {
        self.active.store(handle, Ordering::Release);
    }

    /// Returns the active endpoint handle, or `None` if unbound.
    pub fn active_handle(&self) -> Option<u64> {
        match self.active.load(Ordering::Acquire) {
            0 => None,
            h => Some(h),
        }
    }
}
