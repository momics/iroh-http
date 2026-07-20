//! Explicit ordinary-DNS nameserver configuration for the endpoint resolver.
//!
//! This stable module validates adapter-provided nameserver addresses and
//! constructs `iroh-dns`. Scoped IPv6 destinations temporarily pass through
//! [`super::scoped_dns_compat`]; resolving issue #368 removes only that child
//! implementation, not ordinary resolver configuration or input policy.

use std::net::SocketAddr;

use super::scoped_dns_compat::ScopedDnsCompat;

/// Resolver configuration plus any private helper lifetime required by the
/// released resolver implementation.
pub(super) struct ConfiguredDns {
    resolver: Option<iroh::dns::DnsResolver>,
    compat: ScopedDnsCompat,
}

impl ConfiguredDns {
    pub(super) async fn configure(nameservers: &[String]) -> Result<Self, crate::CoreError> {
        if nameservers.is_empty() {
            return Ok(Self {
                resolver: None,
                compat: ScopedDnsCompat::default(),
            });
        }

        let mut builder = iroh::dns::DnsResolver::builder();
        let mut compat = ScopedDnsCompat::default();
        let mut added = 0usize;
        let mut rejected = Vec::new();

        for nameserver in nameservers {
            match parse_dns_nameserver(nameserver) {
                Ok(addr) => {
                    let resolver_addr = compat.resolver_addr(nameserver, addr).await?;
                    builder = builder.with_nameserver(resolver_addr, iroh::dns::DnsProtocol::Udp);
                    added = added.saturating_add(1);
                }
                Err(reason) => rejected.push(reason),
            }
        }

        if !rejected.is_empty() {
            tracing::warn!(
                "iroh-http: ignoring {} unparseable DNS nameserver(s): {}",
                rejected.len(),
                rejected.join("; "),
            );
        }

        // Explicit servers are supplied because the system resolver is not
        // reliable on all supported mobile hosts. Falling back silently would
        // recreate the failure this configuration is intended to prevent.
        if added == 0 {
            return Err(crate::CoreError::invalid_input(format!(
                "all {} supplied dns_nameservers were invalid (expected IP addresses): {}",
                rejected.len(),
                rejected.join("; "),
            )));
        }

        Ok(Self {
            resolver: Some(builder.build()),
            compat,
        })
    }

    pub(super) fn into_parts(self) -> (Option<iroh::dns::DnsResolver>, ScopedDnsCompat) {
        (self.resolver, self.compat)
    }
}

/// Parse a configured DNS nameserver without losing IPv6 routing scope.
///
/// Named scopes are rejected because resolving interface names is
/// platform-specific. Mobile adapters supply the numeric interface index from
/// their networking interface.
fn parse_dns_nameserver(value: &str) -> Result<SocketAddr, String> {
    if let Some((host, scope)) = value.split_once('%') {
        let ip = host
            .parse::<std::net::Ipv6Addr>()
            .map_err(|reason| format!("invalid scoped IPv6 DNS nameserver {value:?}: {reason}"))?;
        let scope_id = scope.parse::<u32>().map_err(|_| {
            format!("IPv6 DNS nameserver {value:?} must use a numeric interface scope")
        })?;
        if scope_id == 0 {
            return Err(format!(
                "IPv6 DNS nameserver {value:?} must use a non-zero interface scope"
            ));
        }
        return Ok(SocketAddr::V6(std::net::SocketAddrV6::new(
            ip, 53, 0, scope_id,
        )));
    }

    let ip = value
        .parse::<std::net::IpAddr>()
        .map_err(|reason| format!("invalid DNS nameserver {value:?}: {reason}"))?;
    if let std::net::IpAddr::V6(v6) = ip {
        let is_link_local = (v6.segments()[0] & 0xffc0) == 0xfe80;
        if is_link_local {
            return Err(format!(
                "link-local IPv6 DNS nameserver {value:?} requires a numeric interface scope"
            ));
        }
    }
    Ok(SocketAddr::new(ip, 53))
}

#[cfg(test)]
mod tests {
    use super::{parse_dns_nameserver, ConfiguredDns, SocketAddr};
    use crate::endpoint::{
        config::{DiscoveryOptions, NodeOptions},
        IrohEndpoint,
    };

    #[tokio::test]
    async fn bind_rejects_all_invalid_dns_nameservers() {
        let discovery = {
            let mut discovery = DiscoveryOptions::new(None, true);
            discovery.dns_nameservers = vec!["not-an-ip".to_string(), "999.1.1.1".to_string()];
            discovery
        };
        let opts = NodeOptions {
            discovery,
            ..Default::default()
        };

        let error = match IrohEndpoint::bind(opts).await {
            Ok(_) => panic!("bind must fail when no supplied dns_nameserver parses"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("dns_nameservers") && message.contains("not-an-ip"),
            "error should name the rejected nameservers, got: {message}"
        );
    }

    #[tokio::test]
    async fn bind_accepts_when_at_least_one_dns_nameserver_parses() {
        let discovery = {
            let mut discovery = DiscoveryOptions::new(None, true);
            discovery.dns_nameservers = vec!["bogus".to_string(), "8.8.8.8".to_string()];
            discovery
        };
        let opts = NodeOptions {
            discovery,
            ..Default::default()
        };

        let endpoint = IrohEndpoint::bind(opts)
            .await
            .expect("one valid nameserver must allow bind");
        endpoint.close().await;
    }

    #[tokio::test]
    async fn bind_rejects_named_ipv6_dns_scope_instead_of_discarding_it() {
        let discovery = {
            let mut discovery = DiscoveryOptions::new(None, true);
            discovery.dns_nameservers = vec!["fe80::1%wlan0".to_string()];
            discovery
        };
        let opts = NodeOptions {
            discovery,
            ..Default::default()
        };

        let error = match IrohEndpoint::bind(opts).await {
            Ok(endpoint) => {
                endpoint.close().await;
                panic!("bind must not silently discard a required IPv6 DNS scope")
            }
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("wlan0"),
            "error should identify the unsupported scope, got: {error}"
        );
    }

    #[test]
    fn numeric_ipv6_dns_scope_is_preserved_in_resolver_address() {
        let addr = parse_dns_nameserver("fe80::1%17").expect("numeric scope must parse");
        let SocketAddr::V6(addr) = addr else {
            panic!("scoped IPv6 input must produce SocketAddrV6");
        };

        assert_eq!(
            addr.ip(),
            &"fe80::1".parse::<std::net::Ipv6Addr>().expect("valid IPv6")
        );
        assert_eq!(addr.port(), 53);
        assert_eq!(addr.scope_id(), 17, "routing scope must not be discarded");
    }

    #[test]
    fn unscoped_link_local_ipv6_dns_is_rejected() {
        let error = parse_dns_nameserver("fe80::1")
            .expect_err("a link-local DNS server without an interface cannot be routed");

        assert!(
            error.contains("interface scope"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn empty_nameserver_list_needs_no_compatibility_guard() {
        let (_, guard) = ConfiguredDns::configure(&[])
            .await
            .expect("empty configuration")
            .into_parts();
        assert_eq!(guard.proxy_count(), 0);
    }
}
