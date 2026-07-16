use std::{error::Error, fmt, future::Future, net::IpAddr, pin::Pin};

use super::RawEvent;

/// Boxed future used by transport handles without an async-trait dependency.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Error crossing the platform transport boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(String);

impl TransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for TransportError {}

/// Mutable advertisement data. Service and instance identity cannot change,
/// while the SRV port, address set, and TXT data may follow a live service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisementUpdate {
    pub port: u16,
    pub addrs: Vec<IpAddr>,
    pub txt: Vec<(String, String)>,
}

/// An already-started platform advertisement.
pub trait AdvertisementHandle: Send + Sync + 'static {
    /// Apply mutable advertisement data. The engine never cancels this future.
    /// The handle must atomically reject native work that loses a race with
    /// [`Self::request_close`].
    fn update(&self, update: AdvertisementUpdate) -> BoxFuture<'_, Result<(), TransportError>>;
    /// Mark the handle closed immediately. This must be non-blocking,
    /// runtime-independent, and idempotent. If an update is in flight, native
    /// stop must wait until its callback has been observed.
    fn request_close(&self);
    /// Observe cleanup already initiated by [`Self::request_close`]. Repeated
    /// calls must return the same cached outcome.
    fn closed(&self) -> BoxFuture<'_, Result<(), TransportError>>;
}

/// An already-started platform browse.
pub trait BrowseHandle: Send + Sync + 'static {
    /// Await the next event. The handle must atomically reject native work that
    /// loses a race with [`Self::request_close`].
    fn next(&self) -> BoxFuture<'_, Result<Option<RawEvent>, TransportError>>;
    /// Mark the handle closed and cause an in-flight [`Self::next`] to finish
    /// after any non-cancellation-safe native callback has been observed. This
    /// must be non-blocking, runtime-independent, and idempotent.
    fn request_close(&self);
    /// Observe cleanup already initiated by [`Self::request_close`]. Repeated
    /// calls must return the same cached outcome.
    fn closed(&self) -> BoxFuture<'_, Result<(), TransportError>>;
}
