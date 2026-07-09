//! Generic DNS-SD (RFC 6763) advertise/browse engine.
//!
//! This is the single mDNS engine used by the crate. It knows nothing about
//! iroh: you hand it a [`ServiceConfig`] (or [`BrowseConfig`]) and it publishes
//! or discovers plain DNS-SD services, surfacing the full [`ServiceRecord`]
//! (instance, host, port, addresses, and *all* TXT properties) without dropping
//! any field.
//!
//! The iroh-http–specific path ([`crate::advertise_peer`] /
//! [`crate::browse_peers`]) is a thin specialization layered on top of this
//! module: it builds a [`ServiceConfig`] from an [`iroh::Endpoint`] and wires
//! browse results into the endpoint's address lookup. See ADR-018.

use std::net::{IpAddr, SocketAddr};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::DiscoveryError;

// ── Protocol ─────────────────────────────────────────────────────────────────

/// Transport protocol component of a DNS-SD service type (`_udp` / `_tcp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// `_udp` — the default, and what iroh-http uses (QUIC is UDP-based).
    #[default]
    Udp,
    /// `_tcp` — for advertising/browsing TCP-based services.
    Tcp,
}

impl Protocol {
    /// The DNS-SD label for this protocol, including the leading underscore.
    fn label(self) -> &'static str {
        match self {
            Protocol::Udp => "_udp",
            Protocol::Tcp => "_tcp",
        }
    }
}

// ── Config / record types ────────────────────────────────────────────────────

/// A service to advertise via DNS-SD.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Bare service name, e.g. `"my-app"` → `_my-app._udp.local.`. Must be a
    /// single DNS label: non-empty, ASCII alphanumeric or `-`.
    pub service_name: String,
    /// DNS-SD instance label (the human/instance-visible name).
    pub instance_name: String,
    /// SRV port peers should connect to.
    pub port: u16,
    /// Direct IP addresses to advertise. Local interface addresses are always
    /// added automatically and kept fresh; entries here are advertised in
    /// addition.
    pub addrs: Vec<IpAddr>,
    /// Arbitrary TXT key/value properties.
    pub txt: Vec<(String, String)>,
    /// Transport protocol (`_udp` default).
    pub protocol: Protocol,
}

/// Parameters for a browse session.
#[derive(Debug, Clone)]
pub struct BrowseConfig {
    /// Bare service name to browse, e.g. `"my-app"` → `_my-app._udp.local.`.
    pub service_name: String,
    /// Transport protocol (`_udp` default).
    pub protocol: Protocol,
}

impl BrowseConfig {
    /// Convenience constructor for a `_udp` browse of `service_name`.
    pub fn udp(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            protocol: Protocol::Udp,
        }
    }
}

/// A fully-resolved DNS-SD service record — nothing dropped.
#[derive(Debug, Clone)]
pub struct ServiceRecord {
    /// `true` = the service appeared or updated; `false` = it expired/left.
    pub is_active: bool,
    /// Service type and domain, e.g. `_my-app._udp.local.`.
    pub service_type: String,
    /// Service instance label.
    pub instance_name: String,
    /// Host name of the service, e.g. `my-host.local.` (absent on expiry).
    pub host: Option<String>,
    /// SRV port (`0` on expiry).
    pub port: u16,
    /// Resolved socket addresses (empty on expiry).
    pub addrs: Vec<SocketAddr>,
    /// All TXT key/value properties (empty on expiry).
    pub txt: Vec<(String, String)>,
}

// ── Service-type helpers ─────────────────────────────────────────────────────

/// Validate a service name and build the fully-qualified DNS-SD service type
/// `_<service_name>.<proto>.local.`.
///
/// The name must be a single DNS label: non-empty and ASCII alphanumeric or
/// `-` (no leading `_`, no dot).
pub(crate) fn service_type(
    service_name: &str,
    protocol: Protocol,
) -> Result<String, DiscoveryError> {
    if service_name.is_empty() {
        return Err(DiscoveryError::InvalidServiceName(
            "service name must not be empty".into(),
        ));
    }
    if !service_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(DiscoveryError::InvalidServiceName(format!(
            "{service_name:?} must contain only ASCII letters, digits, and '-'"
        )));
    }
    Ok(format!("_{service_name}.{}.local.", protocol.label()))
}

/// Extract the service instance label from a DNS-SD fullname like
/// `<instance>._my-app._udp.local.`, given the known `ty_domain` suffix
/// (`_my-app._udp.local.`) reported alongside it.
///
/// Stripping the exact, known suffix — rather than splitting on the first
/// `"._"` — keeps this correct for the *generic* DNS-SD surface, where an
/// instance label can itself legally contain `._` (`mdns-sd` escapes literal
/// dots in TXT/instance data with a leading `\`, but an instance name built
/// from arbitrary bytes can still contain the two-character sequence `._`
/// without escaping). A naive first-match split would then chop the label
/// short.
pub(crate) fn instance_from_fullname(fullname: &str, ty_domain: &str) -> Option<String> {
    let suffix = format!(".{ty_domain}");
    let instance = fullname.strip_suffix(suffix.as_str())?;
    if instance.is_empty() {
        None
    } else {
        Some(instance.to_string())
    }
}

/// Create a fresh mDNS daemon for a single browse or advertise session.
///
/// Each session owns its own daemon (rather than sharing one process-wide) so
/// that its lifetime is tied to the session: dropping the session shuts the
/// daemon down. It also means an advertise and a browse in the *same* process
/// run on separate daemons and can discover each other over loopback multicast
/// — `mdns-sd` does not deliver a daemon's own registrations back to its own
/// browsers.
pub(crate) fn new_daemon() -> Result<ServiceDaemon, DiscoveryError> {
    ServiceDaemon::new().map_err(|e| DiscoveryError::Setup(e.to_string()))
}

// ── Record conversion ────────────────────────────────────────────────────────

fn record_from_resolved(rs: &mdns_sd::ResolvedService) -> Option<ServiceRecord> {
    let instance_name = instance_from_fullname(&rs.fullname, &rs.ty_domain)?;
    let addrs = rs
        .addresses
        .iter()
        .map(|scoped| SocketAddr::new(scoped.to_ip_addr(), rs.port))
        .collect();
    let txt = rs
        .txt_properties
        .iter()
        .map(|p| (p.key().to_string(), p.val_str().to_string()))
        .collect();
    Some(ServiceRecord {
        is_active: true,
        service_type: rs.ty_domain.clone(),
        instance_name,
        host: Some(rs.host.clone()),
        port: rs.port,
        addrs,
        txt,
    })
}

fn record_from_removed(service_type: String, fullname: &str) -> Option<ServiceRecord> {
    let instance_name = instance_from_fullname(fullname, &service_type)?;
    Some(ServiceRecord {
        is_active: false,
        service_type,
        instance_name,
        host: None,
        port: 0,
        addrs: Vec::new(),
        txt: Vec::new(),
    })
}

// ── Advertise ────────────────────────────────────────────────────────────────

/// An active advertise session.
///
/// Drop to stop advertising; this unregisters the DNS-SD service and shuts the
/// session's mDNS daemon down.
pub struct AdvertiseSession {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for AdvertiseSession {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Advertise a service on the local network via DNS-SD.
///
/// Publishes `PTR` + `SRV` + `TXT` + `A`/`AAAA` records under
/// `_<service_name>.<proto>.local.`. Local interface addresses are added and
/// kept fresh automatically ([`ServiceInfo::enable_addr_auto`]); any
/// [`ServiceConfig::addrs`] are advertised in addition. The service stays
/// advertised until the returned [`AdvertiseSession`] is dropped.
pub fn advertise(config: ServiceConfig) -> Result<AdvertiseSession, DiscoveryError> {
    let service_type = service_type(&config.service_name, config.protocol)?;
    let daemon = new_daemon()?;
    let host_name = format!("{}.local.", config.instance_name);

    let info = ServiceInfo::new(
        &service_type,
        &config.instance_name,
        &host_name,
        &config.addrs[..],
        config.port,
        &config.txt[..],
    )
    .map_err(|e| DiscoveryError::Setup(e.to_string()))?
    .enable_addr_auto();

    let fullname = info.get_fullname().to_string();
    daemon
        .register(info)
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?;

    Ok(AdvertiseSession { daemon, fullname })
}

// ── Browse ───────────────────────────────────────────────────────────────────

/// An active browse session that yields [`ServiceRecord`]s.
///
/// Drop to stop receiving records; this stops the underlying DNS-SD browse,
/// shuts the session's mDNS daemon down, and stops the record pump.
pub struct BrowseSession {
    rx: tokio::sync::mpsc::Receiver<ServiceRecord>,
    daemon: ServiceDaemon,
    service_type: String,
    pump: tokio::task::JoinHandle<()>,
}

impl BrowseSession {
    /// Returns the next record, or `None` when the session is closed.
    pub async fn next_record(&mut self) -> Option<ServiceRecord> {
        self.rx.recv().await
    }
}

impl Drop for BrowseSession {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(&self.service_type);
        let _ = self.daemon.shutdown();
        self.pump.abort();
    }
}

/// Browse for services on the local network via DNS-SD.
///
/// Browses `_<service_name>.<proto>.local.` and yields a [`ServiceRecord`] for
/// each resolved or removed service.
pub fn browse(config: BrowseConfig) -> Result<BrowseSession, DiscoveryError> {
    let service_type = service_type(&config.service_name, config.protocol)?;
    let daemon = new_daemon()?;
    let receiver = daemon
        .browse(&service_type)
        .map_err(|e| DiscoveryError::Setup(e.to_string()))?;

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let pump = tokio::spawn(async move {
        while let Ok(event) = receiver.recv_async().await {
            let record = match event {
                ServiceEvent::ServiceResolved(rs) => record_from_resolved(&rs),
                ServiceEvent::ServiceRemoved(ty, fullname) => record_from_removed(ty, &fullname),
                _ => None,
            };
            if let Some(record) = record {
                if tx.send(record).await.is_err() {
                    break;
                }
            }
        }
    });

    Ok(BrowseSession {
        rx,
        daemon,
        service_type,
        pump,
    })
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_type_builds_udp_local_domain() {
        assert_eq!(
            service_type("iroh-http", Protocol::Udp).unwrap(),
            "_iroh-http._udp.local."
        );
    }

    #[test]
    fn service_type_builds_tcp_local_domain() {
        assert_eq!(
            service_type("my-app", Protocol::Tcp).unwrap(),
            "_my-app._tcp.local."
        );
    }

    #[test]
    fn service_type_rejects_empty_and_illegal_names() {
        assert!(matches!(
            service_type("", Protocol::Udp),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
        assert!(matches!(
            service_type("has space", Protocol::Udp),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
        assert!(matches!(
            service_type("has.dot", Protocol::Udp),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
    }

    #[test]
    fn instance_from_fullname_extracts_label() {
        let ty_domain = "_iroh-http._udp.local.";
        let full = "abcdef234567._iroh-http._udp.local.";
        assert_eq!(
            instance_from_fullname(full, ty_domain).as_deref(),
            Some("abcdef234567")
        );
        assert_eq!(
            instance_from_fullname("._iroh-http._udp.local.", ty_domain),
            None
        );
    }

    #[test]
    fn instance_from_fullname_handles_dotted_instance_label() {
        // Regression for PR #330 review finding #5: an instance label that
        // itself contains the two-character sequence `._` must not be
        // truncated by a naive first-match split — only the known
        // `ty_domain` suffix should be stripped.
        let ty_domain = "_my-app._udp.local.";
        let full = "foo._bar._my-app._udp.local.";
        assert_eq!(
            instance_from_fullname(full, ty_domain).as_deref(),
            Some("foo._bar")
        );
    }

    #[test]
    fn removed_record_has_no_addresses_or_txt() {
        let rec = record_from_removed(
            "_my-app._udp.local.".to_string(),
            "inst._my-app._udp.local.",
        )
        .expect("valid instance");
        assert!(!rec.is_active);
        assert_eq!(rec.instance_name, "inst");
        assert!(rec.addrs.is_empty());
        assert!(rec.txt.is_empty());
        assert_eq!(rec.port, 0);
        assert!(rec.host.is_none());
    }
}
