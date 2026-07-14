//! Platform-independent DNS-SD engine contracts.
//!
//! This module intentionally contains no network implementation and no iroh
//! policy. Platform transports produce canonical [`RawEvent`] values; higher
//! layers may project those records into application-specific discoveries.

mod transport;
mod types;

pub use transport::{
    BoxFuture, DnsSdTransport, NextEvent, TransportAdvertisement, TransportBrowse, TransportError,
    TransportErrorKind, TransportResult,
};
#[cfg(feature = "mdns")]
pub(crate) use types::service_type;
pub use types::{BrowseConfig, Protocol, RawEvent, ServiceConfig, ServiceRecord};
