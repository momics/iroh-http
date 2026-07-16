//! Transport subsystem — raw QUIC endpoint and stable identity.

use iroh::Endpoint;

/// Raw QUIC transport state.
pub(in crate::endpoint) struct Transport {
    /// The bound iroh endpoint. Cloning is cheap (internally `Arc`).
    pub(in crate::endpoint) ep: Endpoint,
    /// The node's own base32-encoded public key. Stable for the lifetime
    /// of the secret key.
    pub(in crate::endpoint) node_id_str: String,
    /// Private lifetime guard for temporary resolver compatibility helpers.
    pub(in crate::endpoint) scoped_dns_compat: super::scoped_dns_compat::ScopedDnsCompat,
}

impl Transport {
    /// Stop transport helpers and then close the underlying endpoint.
    pub(in crate::endpoint) async fn close(&self) {
        self.scoped_dns_compat.shutdown().await;
        self.ep.close().await;
    }

    #[cfg(test)]
    pub(in crate::endpoint) fn scoped_dns_proxy_count(&self) -> usize {
        self.scoped_dns_compat.proxy_count()
    }

    #[cfg(test)]
    pub(in crate::endpoint) fn running_scoped_dns_proxy_count(&self) -> usize {
        self.scoped_dns_compat.running_proxy_count()
    }
}
