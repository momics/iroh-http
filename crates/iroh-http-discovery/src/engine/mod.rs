//! Platform-independent DNS-SD engine contracts.
//!
//! This module intentionally contains no network implementation and no iroh
//! policy. Platform transports produce canonical [`RawEvent`] values; higher
//! layers may project those records into application-specific discoveries.

mod advertise;
mod browse;
mod transport;
mod types;

pub use advertise::{
    AdvertiseCommand, AdvertiseEffect, AdvertiseInput, AdvertiseLifecycle, AdvertiseStatus,
};
pub use browse::{BrowseInput, BrowseLifecycle, BrowseStatus};
pub use transport::{
    BoxFuture, DnsSdTransport, NextEvent, TransportAdvertisement, TransportBrowse, TransportError,
    TransportErrorKind, TransportResult,
};
pub use types::{BrowseConfig, Protocol, RawEvent, ServiceConfig, ServiceRecord};
