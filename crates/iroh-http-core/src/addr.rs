//! Node address parsing: tickets, bare node IDs, and JSON address info.

use crate::encoding::parse_node_id;
use crate::{CoreError, IrohEndpoint, NodeAddrInfo};

/// Generate a ticket string for the given endpoint.
///
/// ISS-025: returns `Result` so serialization failures are surfaced to callers
/// instead of being masked as empty strings.
pub fn node_ticket(ep: &IrohEndpoint) -> Result<String, CoreError> {
    let info = ep.node_addr();
    serde_json::to_string(&info)
        .map_err(|e| CoreError::internal(format!("failed to serialize node ticket: {e}")))
}

/// Parsed node address from a ticket string, bare node ID, or JSON address info.
pub struct ParsedNodeAddr {
    pub node_id: iroh::PublicKey,
    pub direct_addrs: Vec<std::net::SocketAddr>,
}

/// Parse a string that may be a bare node ID, a ticket string (JSON-encoded
/// `NodeAddrInfo`), or a JSON object with `id` and `addrs` fields.
///
/// ISS-023: malformed entries that look like socket addresses but fail to parse
/// cause a deterministic error. Entries that are clearly not socket addresses
/// (e.g. relay URLs containing `://`) are silently skipped and handled
/// elsewhere in the protocol stack.
pub fn parse_node_addr(s: &str) -> Result<ParsedNodeAddr, CoreError> {
    if let Ok(info) = serde_json::from_str::<NodeAddrInfo>(s) {
        let node_id = parse_node_id(&info.id)?;
        let mut direct_addrs = Vec::new();
        for addr_str in &info.addrs {
            // Skip relay URLs — they are handled by the relay subsystem.
            if addr_str.contains("://") {
                continue;
            }
            let addr = addr_str
                .parse::<std::net::SocketAddr>()
                .map_err(|_| CoreError::invalid_input(format!("malformed address: {addr_str}")))?;
            direct_addrs.push(addr);
        }
        return Ok(ParsedNodeAddr {
            node_id,
            direct_addrs,
        });
    }
    let node_id = parse_node_id(s)?;
    Ok(ParsedNodeAddr {
        node_id,
        direct_addrs: Vec::new(),
    })
}
