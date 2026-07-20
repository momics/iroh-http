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
    pub relay_urls: Vec<iroh::RelayUrl>,
    pub direct_addrs: Vec<std::net::SocketAddr>,
}

/// Parse a string that may be a bare node ID, a ticket string (JSON-encoded
/// `NodeAddrInfo`), or a JSON object with `id` and `addrs` fields.
///
/// ISS-023: malformed entries cause a deterministic error rather than being
/// silently discarded before they reach iroh's dialer.
pub fn parse_node_addr(s: &str) -> Result<ParsedNodeAddr, CoreError> {
    if let Ok(info) = serde_json::from_str::<NodeAddrInfo>(s) {
        let node_id = parse_node_id(&info.id)?;
        let mut relay_urls = Vec::new();
        let mut direct_addrs = Vec::new();
        for addr_str in &info.addrs {
            if addr_str.contains("://") {
                relay_urls.push(addr_str.parse::<iroh::RelayUrl>().map_err(|_| {
                    CoreError::invalid_input(format!("malformed relay URL: {addr_str}"))
                })?);
                continue;
            }
            let addr = addr_str
                .parse::<std::net::SocketAddr>()
                .map_err(|_| CoreError::invalid_input(format!("malformed address: {addr_str}")))?;
            // #346: a port-0 direct address is undialable; reject it here rather
            // than letting a useless `:0` reach the dialer.
            if addr.port() == 0 {
                return Err(CoreError::invalid_input(format!(
                    "direct address {addr_str} has no port (port 0)"
                )));
            }
            direct_addrs.push(addr);
        }
        return Ok(ParsedNodeAddr {
            node_id,
            relay_urls,
            direct_addrs,
        });
    }
    let node_id = parse_node_id(s)?;
    Ok(ParsedNodeAddr {
        node_id,
        relay_urls: Vec::new(),
        direct_addrs: Vec::new(),
    })
}
