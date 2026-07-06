//! Structured error types shared across the crate and the FFI boundary.
//!
//! [`CoreError`] pairs a machine-readable [`ErrorCode`] with human-readable
//! detail. Platform adapters match on the code directly — no string parsing.

/// Machine-readable error codes for the FFI boundary.
///
/// Platform adapters match on this directly — no string parsing needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    InvalidInput,
    ConnectionFailed,
    Timeout,
    BodyTooLarge,
    HeaderTooLarge,
    PeerRejected,
    Cancelled,
    Internal,
}

/// Structured error returned by core functions.
///
/// `code` is machine-readable. `message` carries human-readable detail.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: String,
}

impl CoreError {
    pub fn invalid_input(detail: impl std::fmt::Display) -> Self {
        CoreError {
            code: ErrorCode::InvalidInput,
            message: detail.to_string(),
        }
    }
    pub fn connection_failed(detail: impl std::fmt::Display) -> Self {
        CoreError {
            code: ErrorCode::ConnectionFailed,
            message: detail.to_string(),
        }
    }
    pub fn timeout(detail: impl std::fmt::Display) -> Self {
        CoreError {
            code: ErrorCode::Timeout,
            message: detail.to_string(),
        }
    }
    pub fn body_too_large(detail: impl std::fmt::Display) -> Self {
        CoreError {
            code: ErrorCode::BodyTooLarge,
            message: detail.to_string(),
        }
    }
    pub fn header_too_large(detail: impl std::fmt::Display) -> Self {
        CoreError {
            code: ErrorCode::HeaderTooLarge,
            message: detail.to_string(),
        }
    }
    pub fn peer_rejected(detail: impl std::fmt::Display) -> Self {
        CoreError {
            code: ErrorCode::PeerRejected,
            message: detail.to_string(),
        }
    }
    pub fn internal(detail: impl std::fmt::Display) -> Self {
        CoreError {
            code: ErrorCode::Internal,
            message: detail.to_string(),
        }
    }
    pub fn invalid_handle(handle: u64) -> Self {
        CoreError {
            code: ErrorCode::InvalidInput,
            message: format!("unknown handle: {handle}"),
        }
    }
    pub fn cancelled() -> Self {
        CoreError {
            code: ErrorCode::Cancelled,
            message: "aborted".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_error_display() {
        let e = CoreError::timeout("30s elapsed");
        assert!(e.to_string().contains("Timeout"));
        assert!(e.to_string().contains("30s elapsed"));
    }
}
