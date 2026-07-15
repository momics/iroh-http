use std::collections::HashMap;

use serde::Deserialize;

use iroh_http_discovery::engine::{RawEvent, ServiceRecord, TransportError};

/// One generic DNS-SD record crossing the native mobile bridge.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileServiceRecord {
    pub is_active: bool,
    pub service_type: String,
    pub instance_name: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub txt: HashMap<String, String>,
}

pub(crate) fn raw_event_from_mobile(
    record: MobileServiceRecord,
) -> Result<RawEvent, TransportError> {
    if !record.is_active {
        return Ok(RawEvent::Remove {
            service_type: record.service_type,
            instance_name: record.instance_name,
        });
    }
    let addrs = record
        .addrs
        .into_iter()
        .map(|address| {
            address.parse().map_err(|_| {
                TransportError::new(format!(
                    "native DNS-SD returned an invalid socket address: {address}"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut txt: Vec<_> = record.txt.into_iter().collect();
    txt.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(RawEvent::Upsert(ServiceRecord {
        is_active: true,
        service_type: record.service_type,
        instance_name: record.instance_name,
        host: record.host,
        port: record.port,
        addrs,
        txt,
    }))
}

#[cfg(test)]
mod tests {
    use iroh_http_discovery::engine::RawEvent;

    use super::*;

    fn record(is_active: bool) -> MobileServiceRecord {
        MobileServiceRecord {
            is_active,
            service_type: "_demo._udp.local.".to_string(),
            instance_name: "printer".to_string(),
            host: Some("printer.local.".to_string()),
            port: 9100,
            addrs: vec![
                "192.168.1.20:9100".to_string(),
                "[fd00::20]:9100".to_string(),
            ],
            txt: HashMap::from([("note".to_string(), "office".to_string())]),
        }
    }

    #[test]
    fn active_native_record_becomes_a_canonical_upsert() {
        let RawEvent::Upsert(record) = raw_event_from_mobile(record(true)).unwrap() else {
            panic!("expected an upsert");
        };

        assert_eq!(record.service_type, "_demo._udp.local.");
        assert_eq!(record.instance_name, "printer");
        assert_eq!(record.port, 9100);
        assert_eq!(record.addrs.len(), 2);
        assert_eq!(record.txt, vec![("note".to_string(), "office".to_string())]);
    }

    #[test]
    fn inactive_native_record_preserves_only_removal_identity() {
        let RawEvent::Remove {
            service_type,
            instance_name,
        } = raw_event_from_mobile(record(false)).unwrap()
        else {
            panic!("expected a removal");
        };

        assert_eq!(service_type, "_demo._udp.local.");
        assert_eq!(instance_name, "printer");
    }

    #[test]
    fn malformed_active_native_address_is_rejected_at_the_adapter_seam() {
        let mut record = record(true);
        record.addrs.push("not-a-socket".to_string());

        assert!(raw_event_from_mobile(record).is_err());
    }
}
