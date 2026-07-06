//! Ed25519 key operations: signing, verification, and key generation.
//!
//! Each helper is panic-guarded so a fault in the underlying crypto library
//! surfaces as a [`CoreError`] (or `false`) rather than unwinding across the
//! FFI boundary.

use crate::CoreError;

/// Sign arbitrary bytes with a 32-byte Ed25519 secret key.
/// Returns a 64-byte signature, or `Err` if the underlying crypto panics.
pub fn secret_key_sign(secret_key_bytes: &[u8; 32], data: &[u8]) -> Result<[u8; 64], CoreError> {
    std::panic::catch_unwind(|| {
        let key = iroh::SecretKey::from_bytes(secret_key_bytes);
        key.sign(data).to_bytes()
    })
    .map_err(|_| CoreError::internal("secret_key_sign panicked"))
}

/// Verify a 64-byte Ed25519 signature against a 32-byte public key.
/// Returns `true` on success, `false` on any failure (including panics).
pub fn public_key_verify(public_key_bytes: &[u8; 32], data: &[u8], sig_bytes: &[u8; 64]) -> bool {
    std::panic::catch_unwind(|| {
        let Ok(key) = iroh::PublicKey::from_bytes(public_key_bytes) else {
            return false;
        };
        let sig = iroh::Signature::from_bytes(sig_bytes);
        key.verify(data, &sig).is_ok()
    })
    .unwrap_or(false)
}

/// Generate a fresh Ed25519 secret key. Returns 32 raw bytes, or `Err` if the RNG panics.
pub fn generate_secret_key() -> Result<[u8; 32], CoreError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        iroh::SecretKey::generate().to_bytes()
    }))
    .map_err(|_| CoreError::internal("generate_secret_key panicked"))
}
