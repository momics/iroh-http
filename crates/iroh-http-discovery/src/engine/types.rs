use std::net::{IpAddr, SocketAddr};

#[cfg(any(feature = "mdns", test))]
use crate::DiscoveryError;

/// Transport protocol component of a DNS-SD service type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// UDP-based service (`_udp`).
    #[default]
    Udp,
    /// TCP-based service (`_tcp`).
    Tcp,
}

/// Canonical description of a DNS-SD advertisement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    /// Bare service label, without the leading underscore or protocol/domain.
    pub service_name: String,
    /// Human-visible DNS-SD instance label.
    pub instance_name: String,
    /// SRV port.
    pub port: u16,
    /// Explicit addresses in addition to addresses supplied by the transport.
    pub addrs: Vec<IpAddr>,
    /// TXT properties. Order is retained across the transport boundary.
    pub txt: Vec<(String, String)>,
    /// DNS-SD transport protocol.
    pub protocol: Protocol,
}

/// Canonical description of a DNS-SD browse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseConfig {
    /// Bare service label, without the leading underscore or protocol/domain.
    pub service_name: String,
    /// DNS-SD transport protocol.
    pub protocol: Protocol,
}

impl BrowseConfig {
    /// Create a UDP browse configuration.
    pub fn udp(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            protocol: Protocol::Udp,
        }
    }
}

impl Protocol {
    #[cfg(any(feature = "mdns", test))]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Udp => "_udp",
            Self::Tcp => "_tcp",
        }
    }
}

/// Validate a bare service name and return its fully qualified local type.
#[cfg(any(feature = "mdns", test))]
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
        .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(DiscoveryError::InvalidServiceName(format!(
            "{service_name:?} must contain only ASCII letters, digits, and '-'"
        )));
    }
    Ok(format!("_{service_name}.{}.local.", protocol.label()))
}

/// A fully resolved, active DNS-SD service.
///
/// Removal is represented separately by [`RawEvent::Remove`]. This keeps a
/// tombstone from pretending to be a resolved record with zero/empty fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRecord {
    /// `true` for an appeared/updated service and `false` for a tombstone.
    pub is_active: bool,
    /// Fully qualified service type and domain, for example
    /// `_my-app._udp.local.`.
    pub service_type: String,
    /// DNS-SD instance label.
    pub instance_name: String,
    /// Resolved host name, when supplied by the platform.
    pub host: Option<String>,
    /// Resolved SRV port.
    pub port: u16,
    /// Resolved socket addresses.
    pub addrs: Vec<SocketAddr>,
    /// All TXT key/value properties in transport order.
    pub txt: Vec<(String, String)>,
}

/// Lossless event emitted by a platform DNS-SD transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawEvent {
    /// A service appeared or its resolved data changed.
    Upsert(ServiceRecord),
    /// A service disappeared.
    Remove {
        /// Fully qualified service type and domain.
        service_type: String,
        /// DNS-SD instance label.
        instance_name: String,
    },
}

impl RawEvent {
    /// Identity used to correlate an upsert and remove event.
    pub fn identity(&self) -> (&str, &str) {
        match self {
            Self::Upsert(record) => (&record.service_type, &record.instance_name),
            Self::Remove {
                service_type,
                instance_name,
            } => (service_type, instance_name),
        }
    }
}

impl From<RawEvent> for ServiceRecord {
    fn from(event: RawEvent) -> Self {
        match event {
            RawEvent::Upsert(mut record) => {
                record.is_active = true;
                record
            }
            RawEvent::Remove {
                service_type,
                instance_name,
            } => Self {
                is_active: false,
                service_type,
                instance_name,
                host: None,
                port: 0,
                addrs: Vec::new(),
                txt: Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_and_remove_have_the_same_explicit_identity() {
        let record = ServiceRecord {
            is_active: true,
            service_type: "_web._tcp.local.".into(),
            instance_name: "docs".into(),
            host: Some("docs.local.".into()),
            port: 443,
            addrs: vec![],
            txt: vec![("path".into(), "/manual".into())],
        };
        let upsert = RawEvent::Upsert(record);
        let remove = RawEvent::Remove {
            service_type: "_web._tcp.local.".into(),
            instance_name: "docs".into(),
        };

        assert_eq!(upsert.identity(), remove.identity());
    }

    #[test]
    fn remove_converts_to_the_public_compatibility_tombstone() {
        let record = ServiceRecord::from(RawEvent::Remove {
            service_type: "_web._tcp.local.".into(),
            instance_name: "docs".into(),
        });

        assert!(!record.is_active);
        assert_eq!(record.service_type, "_web._tcp.local.");
        assert_eq!(record.instance_name, "docs");
        assert_eq!(record.host, None);
        assert_eq!(record.port, 0);
        assert!(record.addrs.is_empty());
        assert!(record.txt.is_empty());
    }

    #[test]
    fn service_type_preserves_existing_validation_and_protocol_mapping() {
        assert!(matches!(
            service_type("iroh-http", Protocol::Udp),
            Ok(value) if value == "_iroh-http._udp.local."
        ));
        assert!(matches!(
            service_type("web", Protocol::Tcp),
            Ok(value) if value == "_web._tcp.local."
        ));
        assert!(matches!(
            service_type("bad.name", Protocol::Udp),
            Err(DiscoveryError::InvalidServiceName(_))
        ));
    }
}
