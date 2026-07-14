//! Platform-independent DNS-SD engine contracts.
//!
//! This module intentionally contains no network implementation and no iroh
//! policy. Platform transports produce canonical [`RawEvent`] values; higher
//! layers may project those records into application-specific discoveries.

mod transport;
mod types;

#[cfg(feature = "runtime")]
mod session;

#[cfg(feature = "runtime")]
pub use session::{AdvertisementSession, BrowseSession};
pub use transport::{
    AdvertisementHandle, AdvertisementUpdate, BoxFuture, BrowseHandle, TransportError,
};
#[cfg(feature = "mdns")]
pub(crate) use types::service_type;
pub use types::{BrowseConfig, Protocol, RawEvent, ServiceConfig, ServiceRecord};
