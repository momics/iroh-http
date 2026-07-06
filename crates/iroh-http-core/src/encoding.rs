//! Base32 encoding utilities and node-id parsing.
//!
//! All base32 uses lowercase RFC 4648 without padding, the canonical form for
//! node identifiers throughout the crate.

use crate::CoreError;

/// Encode bytes as lowercase RFC 4648 base32 (no padding).
pub fn base32_encode(bytes: &[u8]) -> String {
    base32::encode(base32::Alphabet::Rfc4648Lower { padding: false }, bytes)
}

/// Decode an RFC 4648 base32 string (no padding, case-insensitive) to bytes.
pub(crate) fn base32_decode(s: &str) -> Result<Vec<u8>, String> {
    base32::decode(base32::Alphabet::Rfc4648Lower { padding: false }, s)
        .ok_or_else(|| format!("invalid base32 string: {s}"))
}

/// Parse a base32 node-id string into an `iroh::PublicKey`.
pub(crate) fn parse_node_id(s: &str) -> Result<iroh::PublicKey, CoreError> {
    let bytes = base32_decode(s).map_err(CoreError::invalid_input)?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| CoreError::invalid_input("node-id must be 32 bytes"))?;
    iroh::PublicKey::from_bytes(&arr).map_err(|e| CoreError::invalid_input(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_round_trip() {
        let original: Vec<u8> = (0..32).collect();
        let encoded = base32_encode(&original);
        let decoded = base32_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn base32_empty() {
        let encoded = base32_encode(&[]);
        assert_eq!(encoded, "");
        let decoded = base32_decode("").unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn base32_decode_invalid_char() {
        let result = base32_decode("!!!invalid!!!");
        assert!(result.is_err());
    }

    #[test]
    fn parse_node_id_invalid_base32() {
        let result = parse_node_id("!!!not-base32!!!");
        assert!(result.is_err());
    }

    #[test]
    fn parse_node_id_wrong_length() {
        let result = parse_node_id("aa");
        assert!(result.is_err());
    }
}
