//! Transport subsystem — raw QUIC endpoint and stable identity.

use iroh::Endpoint;

/// Raw QUIC transport state.
pub(in crate::endpoint) struct Transport {
    /// The bound iroh endpoint. Cloning is cheap (internally `Arc`).
    pub(in crate::endpoint) ep: Endpoint,
    /// The node's own base32-encoded public key. Stable for the lifetime
    /// of the secret key.
    pub(in crate::endpoint) node_id_str: String,
    /// Scoped IPv6 DNS forwarders. Their handles keep the tasks alive until
    /// explicit endpoint close, and abort them on drop during failed setup.
    pub(in crate::endpoint) scoped_dns_proxies: Vec<super::bind::ScopedDnsProxy>,
}

impl Transport {
    pub(in crate::endpoint) fn shutdown_scoped_dns_proxies(&self) {
        for proxy in &self.scoped_dns_proxies {
            proxy.shutdown();
        }
    }

    #[cfg(test)]
    pub(in crate::endpoint) fn scoped_dns_proxy_count(&self) -> usize {
        self.scoped_dns_proxies.len()
    }

    #[cfg(test)]
    pub(in crate::endpoint) fn running_scoped_dns_proxy_count(&self) -> usize {
        self.scoped_dns_proxies
            .iter()
            .filter(|proxy| proxy.is_running())
            .count()
    }
}
