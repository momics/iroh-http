//! Node ticket integration tests.

use iroh_http_core::{parse_node_addr, NodeAddrInfo};

#[test]
fn parse_node_addr_accepts_bare_node_id() {
    let key = iroh::SecretKey::generate();
    let b32 = iroh_http_core::base32_encode(key.public().as_bytes());
    let parsed = parse_node_addr(&b32).unwrap();
    assert_eq!(parsed.node_id, key.public());
    assert!(parsed.direct_addrs.is_empty());
}

#[test]
fn parse_node_addr_accepts_ticket_json() {
    let key = iroh::SecretKey::generate();
    let b32 = iroh_http_core::base32_encode(key.public().as_bytes());
    let info = NodeAddrInfo {
        id: b32.clone(),
        addrs: vec![
            "https://relay.example.com".to_string(),
            "127.0.0.1:12345".to_string(),
        ],
    };
    let ticket = serde_json::to_string(&info).unwrap();
    let parsed = parse_node_addr(&ticket).unwrap();
    assert_eq!(parsed.node_id, key.public());
    assert_eq!(parsed.direct_addrs.len(), 1);
    assert_eq!(parsed.direct_addrs[0].to_string(), "127.0.0.1:12345");
}

#[test]
fn parse_node_addr_rejects_garbage() {
    assert!(parse_node_addr("not-a-valid-thing!!!").is_err());
}

// Regression: #346 — a direct address with port 0 in a ticket/JSON address is
// undialable and must be rejected before it reaches the dialer.
#[test]
fn parse_node_addr_rejects_port_zero_direct_addr() {
    let key = iroh::SecretKey::generate();
    let b32 = iroh_http_core::base32_encode(key.public().as_bytes());
    let info = NodeAddrInfo {
        id: b32,
        addrs: vec!["192.168.50.227:0".to_string()],
    };
    let ticket = serde_json::to_string(&info).unwrap();
    let err = match parse_node_addr(&ticket) {
        Ok(_) => panic!("port-0 direct address must be rejected"),
        Err(e) => e,
    };
    assert!(
        format!("{err:?}").contains("192.168.50.227"),
        "error should name the offending address, got: {err:?}"
    );
}
