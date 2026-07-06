//! Shared adapter utilities for iroh-http.
//!
//! Adapters (Deno, Node, Tauri) share a common JSON error convention at the
//! FFI boundary: `{"code":"...","message":"..."}`.  This crate owns that
//! convention so it is defined exactly once.
//!
//! This is intentionally **not** part of `iroh-http-core` — the JSON shape is
//! an adapter-layer concern, not HTTP transport semantics.
#![deny(unsafe_code)]

use iroh_http_core::{respond, CoreError, ErrorCode, HandleStore, ResponseHeadEntry};

/// Maximum number of header rows accepted at an adapter boundary.
pub const MAX_HEADER_COUNT: usize = 100;
/// Maximum header name length in bytes accepted at an adapter boundary.
pub const MAX_HEADER_NAME_LEN: usize = 256;
/// Maximum header value length in bytes accepted at an adapter boundary.
pub const MAX_HEADER_VALUE_LEN: usize = 8_192;
/// Maximum adapter-level timeout in milliseconds.
pub const MAX_TIMEOUT_MS: u64 = 300_000;
/// Maximum adapter-level body cap in bytes.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Maximum node-id / ticket string length in bytes accepted at an adapter boundary.
pub const MAX_NODE_ID_LEN: usize = 128;
/// Maximum request URL length in bytes accepted at an adapter boundary.
pub const MAX_URL_LEN: usize = 8_192;
/// Maximum HTTP method length in bytes accepted at an adapter boundary.
pub const MAX_METHOD_LEN: usize = 32;
/// Maximum number of direct addresses accepted at an adapter boundary.
pub const MAX_DIRECT_ADDRS: usize = 32;
/// Maximum length of a single direct address string in bytes.
pub const MAX_DIRECT_ADDR_LEN: usize = 256;

const UNDELIVERABLE_STATUS: u16 = 503;
const UNDELIVERABLE_HEADERS: [(&str, &str); 1] = [("content-length", "0")];

/// Build the fail-closed response head for a request that cannot reach JS.
///
/// `reason` is accepted at call sites so logs and future diagnostics can pass
/// stable context without redefining the response status or headers.
pub fn reject_undeliverable(_reason: impl std::fmt::Display) -> ResponseHeadEntry {
    ResponseHeadEntry {
        status: UNDELIVERABLE_STATUS,
        headers: UNDELIVERABLE_HEADERS
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect(),
    }
}

/// Send the standard undeliverable rejection to a pending core request.
pub fn send_undeliverable_rejection(
    handles: &HandleStore,
    req_handle: u64,
    reason: impl std::fmt::Display,
) -> Result<(), CoreError> {
    let head = reject_undeliverable(reason);
    respond(handles, req_handle, head.status, head.headers)
}

/// Adapter-boundary input validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterInputError {
    /// Node/public-key input failed coarse boundary validation.
    InvalidNodeId,
    /// URL input failed coarse boundary validation.
    InvalidUrl,
    /// Caller-provided input exceeds a hard boundary limit.
    InputTooLarge {
        field: &'static str,
        max: usize,
        got: usize,
    },
    /// Caller-provided bytes contain invalid encoding for the field.
    InvalidEncoding { field: &'static str },
    /// Generic argument validation failure for a named field.
    InvalidArgument { field: &'static str, reason: String },
}

impl AdapterInputError {
    /// Stable JSON error code used by JS/TS adapters.
    pub fn code(&self) -> &'static str {
        match self {
            AdapterInputError::InvalidNodeId => "INVALID_NODE_ID",
            AdapterInputError::InvalidUrl => "INVALID_URL",
            AdapterInputError::InputTooLarge { .. } => "INPUT_TOO_LARGE",
            AdapterInputError::InvalidEncoding { .. } => "INVALID_ENCODING",
            AdapterInputError::InvalidArgument { .. } => "INVALID_ARGUMENT",
        }
    }

    /// Human-readable validation message.
    pub fn message(&self) -> String {
        match self {
            AdapterInputError::InvalidNodeId => "invalid node id".to_string(),
            AdapterInputError::InvalidUrl => "invalid url".to_string(),
            AdapterInputError::InputTooLarge { field, max, got } => {
                format!("{field} exceeds max size {max} (got {got})")
            }
            AdapterInputError::InvalidEncoding { field } => {
                format!("{field} contains invalid encoding")
            }
            AdapterInputError::InvalidArgument { field, reason } => {
                format!("{field}: {reason}")
            }
        }
    }

    /// Serialize this error to the adapter JSON error envelope.
    pub fn to_json(&self) -> String {
        format_error_json(self.code(), self.message())
    }
}

/// Validate and convert FFI header rows into core header pairs.
pub fn validate_header_rows(
    headers: Vec<Vec<String>>,
) -> Result<Vec<(String, String)>, AdapterInputError> {
    if headers.len() > MAX_HEADER_COUNT {
        return Err(AdapterInputError::InputTooLarge {
            field: "headers",
            max: MAX_HEADER_COUNT,
            got: headers.len(),
        });
    }
    let mut pairs = Vec::with_capacity(headers.len());
    for pair in headers {
        if pair.len() != 2 {
            return Err(AdapterInputError::InvalidArgument {
                field: "headers",
                reason: "each header must be [name, value]".to_string(),
            });
        }
        let name = &pair[0];
        let value = &pair[1];
        validate_bounded_string("headerName", name, MAX_HEADER_NAME_LEN)?;
        validate_bounded_string("headerValue", value, MAX_HEADER_VALUE_LEN)?;
        pairs.push((name.clone(), value.clone()));
    }
    Ok(pairs)
}

/// Validate a caller-supplied node id (base32 public key or JSON ticket).
///
/// A payload that begins with `{` (after leading whitespace) is treated as a
/// JSON ticket / `NodeAddrInfo` blob and parsed by core; only boundary-level
/// size and encoding constraints are enforced here.  Otherwise the string must
/// be lowercase base32 (`a-z`, `2-7`).
pub fn validate_node_id(node_id: &str) -> Result<(), AdapterInputError> {
    validate_bounded_string("nodeId", node_id, MAX_NODE_ID_LEN)?;
    if node_id.trim_start().starts_with('{') {
        return Ok(());
    }
    let valid = node_id
        .bytes()
        .all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'));
    if !valid {
        return Err(AdapterInputError::InvalidNodeId);
    }
    Ok(())
}

/// Validate a caller-supplied request URL against boundary size/encoding limits.
pub fn validate_url(url: &str) -> Result<(), AdapterInputError> {
    validate_bounded_string("url", url, MAX_URL_LEN)
}

/// Validate a caller-supplied HTTP method against boundary size/encoding limits.
pub fn validate_method(method: &str) -> Result<(), AdapterInputError> {
    validate_bounded_string("method", method, MAX_METHOD_LEN)
}

/// Validate the optional list of caller-supplied direct addresses.
pub fn validate_direct_addrs(direct_addrs: &Option<Vec<String>>) -> Result<(), AdapterInputError> {
    if let Some(addrs) = direct_addrs {
        if addrs.len() > MAX_DIRECT_ADDRS {
            return Err(AdapterInputError::InputTooLarge {
                field: "directAddrs",
                max: MAX_DIRECT_ADDRS,
                got: addrs.len(),
            });
        }
        for addr in addrs {
            validate_bounded_string("directAddr", addr, MAX_DIRECT_ADDR_LEN)?;
        }
    }
    Ok(())
}

/// Validate and convert an f64 option to a non-negative u64.
pub fn safe_f64_to_u64(
    value: f64,
    field: &'static str,
    max: u64,
) -> Result<u64, AdapterInputError> {
    if value.is_nan() || value.is_infinite() || value < 0.0 {
        return Err(AdapterInputError::InvalidArgument {
            field,
            reason: format!("expected a non-negative finite number, got {value}"),
        });
    }
    if value.fract() != 0.0 {
        return Err(AdapterInputError::InvalidArgument {
            field,
            reason: "must be an integer".to_string(),
        });
    }
    if value > max as f64 {
        return Err(AdapterInputError::InvalidArgument {
            field,
            reason: format!("must be <= {max}"),
        });
    }
    Ok(value as u64)
}

/// Validate and convert an f64 option to a non-negative usize.
pub fn safe_f64_to_usize(
    value: f64,
    field: &'static str,
    max: usize,
) -> Result<usize, AdapterInputError> {
    if value.is_nan() || value.is_infinite() || value < 0.0 {
        return Err(AdapterInputError::InvalidArgument {
            field,
            reason: format!("expected a non-negative finite number, got {value}"),
        });
    }
    if value.fract() != 0.0 {
        return Err(AdapterInputError::InvalidArgument {
            field,
            reason: "must be an integer".to_string(),
        });
    }
    if value > max as f64 {
        return Err(AdapterInputError::InvalidArgument {
            field,
            reason: format!("must be <= {max}"),
        });
    }
    Ok(value as usize)
}

fn validate_bounded_string(
    field: &'static str,
    value: &str,
    max_len: usize,
) -> Result<(), AdapterInputError> {
    if value.is_empty() {
        return Err(AdapterInputError::InvalidArgument {
            field,
            reason: "must not be empty".to_string(),
        });
    }
    if value.len() > max_len {
        return Err(AdapterInputError::InputTooLarge {
            field,
            max: max_len,
            got: value.len(),
        });
    }
    if value.as_bytes().contains(&0) {
        return Err(AdapterInputError::InvalidEncoding { field });
    }
    Ok(())
}

/// Serialize a [`CoreError`] to the FFI error envelope.
///
/// Produces `{"code":"REFUSED","message":"..."}` (and similar) for each
/// [`ErrorCode`] variant.  Unknown future variants map to `"UNKNOWN"`.
pub fn core_error_to_json(e: &CoreError) -> String {
    let code = match e.code {
        ErrorCode::InvalidInput => "INVALID_INPUT",
        ErrorCode::ConnectionFailed => "REFUSED",
        ErrorCode::Timeout => "TIMEOUT",
        ErrorCode::BodyTooLarge => "BODY_TOO_LARGE",
        ErrorCode::HeaderTooLarge => "HEADER_TOO_LARGE",
        ErrorCode::PeerRejected => "PEER_REJECTED",
        ErrorCode::Cancelled => "CANCELLED",
        ErrorCode::Internal => "INTERNAL",
        _ => "UNKNOWN",
    };
    let json_msg = serde_json::Value::String(e.message.clone());
    format!("{{\"code\":\"{code}\",\"message\":{json_msg}}}")
}

/// Serialize an arbitrary error message to the FFI error envelope with an
/// explicit error code string.
pub fn format_error_json(code: &str, msg: impl std::fmt::Display) -> String {
    let json_msg = serde_json::Value::String(msg.to_string());
    format!("{{\"code\":\"{code}\",\"message\":{json_msg}}}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_http_core::CoreError;

    #[test]
    fn core_error_to_json_timeout() {
        let e = CoreError::timeout("timed out");
        let json = core_error_to_json(&e);
        assert!(json.contains("\"code\":\"TIMEOUT\""));
        assert!(json.contains("timed out"));
    }

    #[test]
    fn internal_error_maps_to_internal_code() {
        let e = CoreError::internal("something broke");
        let json = core_error_to_json(&e);
        // Internal errors must surface as "INTERNAL", not "UNKNOWN", so that
        // callers can distinguish deliberate internal errors from unrecognised
        // future error codes.
        assert!(json.contains("\"code\":\"INTERNAL\""));
        assert!(json.contains("something broke"));
    }

    #[test]
    fn format_error_json_escapes_message() {
        let json = format_error_json("INVALID_INPUT", "bad \"chars\"");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["code"].as_str().unwrap(), "INVALID_INPUT");
        assert_eq!(v["message"].as_str().unwrap(), "bad \"chars\"");
    }

    #[test]
    fn validate_header_rows_accepts_valid_rows() {
        let rows = vec![vec!["content-type".to_string(), "text/plain".to_string()]];
        assert_eq!(
            validate_header_rows(rows),
            Ok(vec![("content-type".to_string(), "text/plain".to_string())])
        );
    }

    #[test]
    fn reject_undeliverable_uses_single_503_head() {
        let head = reject_undeliverable("queue full");
        assert_eq!(head.status, 503);
        assert_eq!(head.headers, vec![("content-length".into(), "0".into())]);
    }

    #[test]
    fn validate_header_rows_rejects_malformed_pair() {
        let result = validate_header_rows(vec![vec!["x".to_string()]]);
        assert!(matches!(
            result,
            Err(AdapterInputError::InvalidArgument {
                field: "headers",
                ..
            })
        ));
    }

    #[test]
    fn validate_header_rows_rejects_too_many_rows() {
        let rows = vec![vec!["x".to_string(), "y".to_string()]; MAX_HEADER_COUNT + 1];
        assert!(matches!(
            validate_header_rows(rows),
            Err(AdapterInputError::InputTooLarge {
                field: "headers",
                max: MAX_HEADER_COUNT,
                got
            }) if got == MAX_HEADER_COUNT + 1
        ));
    }

    #[test]
    fn validate_header_rows_rejects_overlong_name_and_value() {
        let long_name = "x".repeat(MAX_HEADER_NAME_LEN + 1);
        let name_result = validate_header_rows(vec![vec![long_name, "y".to_string()]]);
        assert!(matches!(
            name_result,
            Err(AdapterInputError::InputTooLarge {
                field: "headerName",
                max: MAX_HEADER_NAME_LEN,
                ..
            })
        ));

        let long_value = "y".repeat(MAX_HEADER_VALUE_LEN + 1);
        let value_result = validate_header_rows(vec![vec!["x".to_string(), long_value]]);
        assert!(matches!(
            value_result,
            Err(AdapterInputError::InputTooLarge {
                field: "headerValue",
                max: MAX_HEADER_VALUE_LEN,
                ..
            })
        ));
    }

    #[test]
    fn validate_header_rows_rejects_empty_and_null_bytes() {
        let empty_result = validate_header_rows(vec![vec!["".to_string(), "y".to_string()]]);
        assert!(matches!(
            empty_result,
            Err(AdapterInputError::InvalidArgument {
                field: "headerName",
                ..
            })
        ));

        let null_result = validate_header_rows(vec![vec!["x\0".to_string(), "y".to_string()]]);
        assert!(matches!(
            null_result,
            Err(AdapterInputError::InvalidEncoding {
                field: "headerName"
            })
        ));
    }

    #[test]
    fn safe_f64_to_u64_accepts_integer_within_cap() {
        assert_eq!(safe_f64_to_u64(0.0, "timeoutMs", MAX_TIMEOUT_MS), Ok(0));
        assert_eq!(
            safe_f64_to_u64(MAX_TIMEOUT_MS as f64, "timeoutMs", MAX_TIMEOUT_MS),
            Ok(MAX_TIMEOUT_MS)
        );
    }

    #[test]
    fn safe_f64_to_u64_rejects_invalid_values() {
        for value in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(matches!(
                safe_f64_to_u64(value, "timeoutMs", MAX_TIMEOUT_MS),
                Err(AdapterInputError::InvalidArgument {
                    field: "timeoutMs",
                    ..
                })
            ));
        }
        assert!(safe_f64_to_u64(1.5, "timeoutMs", MAX_TIMEOUT_MS).is_err());
        assert!(safe_f64_to_u64((MAX_TIMEOUT_MS + 1) as f64, "timeoutMs", MAX_TIMEOUT_MS).is_err());
    }

    #[test]
    fn safe_f64_to_usize_rejects_invalid_values() {
        for value in [
            f64::NAN,
            f64::INFINITY,
            -1.0,
            1.5,
            (MAX_BODY_BYTES + 1) as f64,
        ] {
            assert!(matches!(
                safe_f64_to_usize(value, "maxResponseBodyBytes", MAX_BODY_BYTES),
                Err(AdapterInputError::InvalidArgument {
                    field: "maxResponseBodyBytes",
                    ..
                })
            ));
        }
    }

    #[test]
    fn validate_node_id_accepts_base32_and_ticket() {
        assert_eq!(validate_node_id("abcdefghijklmnopqrstuvwxyz234567"), Ok(()));
        // JSON ticket / NodeAddrInfo blob bypasses the base32 charset check.
        assert_eq!(validate_node_id("  {\"nodeId\":\"x\"}"), Ok(()));
    }

    #[test]
    fn validate_node_id_rejects_bad_charset_and_bounds() {
        assert_eq!(
            validate_node_id("bad-node-id!"),
            Err(AdapterInputError::InvalidNodeId)
        );
        assert!(matches!(
            validate_node_id(""),
            Err(AdapterInputError::InvalidArgument {
                field: "nodeId",
                ..
            })
        ));
        let long = "a".repeat(MAX_NODE_ID_LEN + 1);
        assert!(matches!(
            validate_node_id(&long),
            Err(AdapterInputError::InputTooLarge {
                field: "nodeId",
                ..
            })
        ));
    }

    #[test]
    fn validate_url_enforces_bounds_and_encoding() {
        assert_eq!(validate_url("httpi://peer/path"), Ok(()));
        assert!(matches!(
            validate_url("httpi://peer/\0x"),
            Err(AdapterInputError::InvalidEncoding { field: "url" })
        ));
        let long = "u".repeat(MAX_URL_LEN + 1);
        assert!(matches!(
            validate_url(&long),
            Err(AdapterInputError::InputTooLarge { field: "url", .. })
        ));
    }

    #[test]
    fn validate_method_enforces_bounds() {
        assert_eq!(validate_method("POST"), Ok(()));
        let long = "M".repeat(MAX_METHOD_LEN + 1);
        assert!(matches!(
            validate_method(&long),
            Err(AdapterInputError::InputTooLarge {
                field: "method",
                ..
            })
        ));
    }

    #[test]
    fn validate_direct_addrs_enforces_count_and_element_bounds() {
        assert_eq!(validate_direct_addrs(&None), Ok(()));
        assert_eq!(
            validate_direct_addrs(&Some(vec!["127.0.0.1:1234".to_string()])),
            Ok(())
        );
        let too_many = vec!["1.2.3.4:5".to_string(); MAX_DIRECT_ADDRS + 1];
        assert!(matches!(
            validate_direct_addrs(&Some(too_many)),
            Err(AdapterInputError::InputTooLarge {
                field: "directAddrs",
                max: MAX_DIRECT_ADDRS,
                ..
            })
        ));
        let long = "a".repeat(MAX_DIRECT_ADDR_LEN + 1);
        assert!(matches!(
            validate_direct_addrs(&Some(vec![long])),
            Err(AdapterInputError::InputTooLarge {
                field: "directAddr",
                ..
            })
        ));
    }
}
