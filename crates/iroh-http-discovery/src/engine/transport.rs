use std::{error::Error, fmt, future::Future, pin::Pin};

use super::{BrowseConfig, RawEvent, ServiceConfig};

/// Boxed future used by transport traits without requiring an async-trait
/// dependency.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Transport operation result.
pub type TransportResult<T> = Result<T, TransportError>;

/// Result of polling a browse transport.
///
/// `Ok(Some(event))` is data, `Ok(None)` is clean and permanent closure, and
/// `Err(error)` is failed and permanent closure.
pub type NextEvent = TransportResult<Option<RawEvent>>;

/// Broad operation class for a platform transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
    Setup,
    Start,
    Browse,
    Update,
    Stop,
}

/// Error crossing the platform transport boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
}

impl TransportError {
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for TransportError {}

/// A live platform advertisement.
pub trait TransportAdvertisement: Send + Sync {
    /// Replace the complete advertised configuration.
    fn update(&self, config: ServiceConfig) -> BoxFuture<'_, TransportResult<()>>;

    /// Stop advertising. Successful return is the cleanup acknowledgement.
    fn close(&self) -> BoxFuture<'_, TransportResult<()>>;
}

/// A live platform browse.
pub trait TransportBrowse: Send + Sync {
    /// Return the next event or a permanent terminal outcome.
    ///
    /// Implementations must serialize platform polling if their native API
    /// permits only one in-flight poll. Once this returns `Ok(None)` or `Err`,
    /// all later calls must return `Ok(None)`.
    fn next(&self) -> BoxFuture<'_, NextEvent>;

    /// Close browsing. Successful return is the cleanup acknowledgement.
    fn close(&self) -> BoxFuture<'_, TransportResult<()>>;
}

/// Factory for platform DNS-SD operations.
pub trait DnsSdTransport: Send + Sync {
    type Advertisement: TransportAdvertisement;
    type Browse: TransportBrowse;

    fn advertise(
        &self,
        config: ServiceConfig,
    ) -> BoxFuture<'_, TransportResult<Self::Advertisement>>;

    fn browse(&self, config: BrowseConfig) -> BoxFuture<'_, TransportResult<Self::Browse>>;
}
