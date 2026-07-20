//! Global state managed by the Tauri plugin.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use iroh_http_core::{endpoint::IrohEndpoint, registry};

// ── Endpoint slab (delegates to core registry) ───────────────────────────────

pub fn get_endpoint(handle: u64) -> Option<IrohEndpoint> {
    registry::get_endpoint(handle)
}

pub fn remove_endpoint(handle: u64) -> Option<IrohEndpoint> {
    remove_endpoint_owner(handle);
    registry::remove_endpoint(handle)
}

fn endpoint_owners() -> &'static Mutex<HashMap<String, u64>> {
    static S: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Insert an endpoint and make it the current owner for its stable node id.
///
/// A Tauri webview can be reloaded by the OS without destroying the Rust
/// process. When JS boots again with the same persisted key, the previous
/// native endpoint may still be present but no longer controlled by the new JS
/// object. In that case the new endpoint replaces the old one.
pub fn replace_endpoint_for_node_id(
    node_id: String,
    ep: IrohEndpoint,
) -> (u64, Option<(u64, IrohEndpoint)>) {
    let handle = registry::insert_endpoint(ep);
    let old_handle = endpoint_owners()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(node_id, handle);

    let old = old_handle
        .filter(|old| *old != handle)
        .and_then(|old| registry::remove_endpoint(old).map(|ep| (old, ep)));

    (handle, old)
}

fn remove_endpoint_owner(handle: u64) {
    endpoint_owners()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|_, owned_handle| *owned_handle != handle);
}

/// Clear stable-node ownership after process-wide endpoint teardown.
pub(crate) fn clear_endpoint_owners() {
    endpoint_owners()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
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
