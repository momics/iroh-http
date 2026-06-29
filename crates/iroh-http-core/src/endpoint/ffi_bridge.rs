//! FfiBridge subsystem — per-endpoint FFI handle store.
//!
//! Kept as a thin wrapper today (one field) so future additions —
//! fetch-cancel slabs, request-head slabs, etc. — have a named home.
//!
//! This module is the definition site of the FFI handle-store reference.
//! The `disallowed_types` lint is explicitly suppressed here; the architecture
//! test in `tests/architecture.rs` enforces that `mod http` never imports
//! `HandleStore`.
#![allow(clippy::disallowed_types)]

use std::sync::Mutex;

use crate::ffi::dispatcher::IrohHttpService;
use crate::ffi::handles::HandleStore;

/// FFI handle store and any future JS-facing token registries.
pub(in crate::endpoint) struct FfiBridge {
    /// Per-endpoint handle store — owns all body readers, writers,
    /// sessions, request-head channels, and fetch-cancel tokens.
    pub(in crate::endpoint) handles: HandleStore,
    /// The locally-registered serve service, stored while a `serve()` is
    /// active so that a self-request (`fetch()` to this node's own id) can be
    /// dispatched in-process instead of dialing over QUIC, which iroh forbids.
    /// Set when serving starts and cleared on serve stop / endpoint close.
    /// See ADR-015 and [`crate::ffi::fetch`].
    pub(in crate::endpoint) local_service: Mutex<Option<IrohHttpService>>,
}
