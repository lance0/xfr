//! Pre-shared key (PSK) authentication
//!
//! Implements challenge-response authentication using HMAC-SHA256.

use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const NONCE_LENGTH: usize = 32;

/// Generate a random nonce for authentication challenge
pub fn generate_nonce() -> String {
    let mut rng = rand::rng();
    let nonce: Vec<u8> = (0..NONCE_LENGTH).map(|_| rng.random()).collect();
    hex::encode(nonce)
}

/// Compute HMAC-SHA256 response for a challenge
pub fn compute_response(nonce: &str, psk: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(psk.as_bytes()).expect("HMAC can take key of any size");
    mac.update(nonce.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

/// Verify an authentication response
pub fn verify_response(nonce: &str, psk: &str, response: &str) -> bool {
    let expected = compute_response(nonce, psk);
    // Constant-time comparison to prevent timing attacks
    constant_time_eq(expected.as_bytes(), response.as_bytes())
}

/// Constant-time comparison to prevent timing attacks
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Read PSK from file, trimming whitespace
pub fn read_psk_file(path: &std::path::Path) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)?;
    Ok(content.trim().to_string())
}

/// Authentication configuration
#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    /// Pre-shared key for authentication
    pub psk: Option<String>,
}

impl AuthConfig {
    /// Check if authentication is required
    pub fn is_required(&self) -> bool {
        self.psk.is_some()
    }
}

// Need hex encoding
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nonce_generation() {
        let nonce1 = generate_nonce();
        let nonce2 = generate_nonce();
        assert_ne!(nonce1, nonce2);
        assert_eq!(nonce1.len(), NONCE_LENGTH * 2); // hex encoded
    }

    #[test]
    fn test_compute_response() {
        let nonce = "abc123";
        let psk = "secret";
        let response = compute_response(nonce, psk);
        assert!(!response.is_empty());
        // Same inputs should produce same output
        assert_eq!(response, compute_response(nonce, psk));
    }

    #[test]
    fn test_verify_response() {
        let nonce = "test_nonce";
        let psk = "my_secret_key";
        let response = compute_response(nonce, psk);

        assert!(verify_response(nonce, psk, &response));
        assert!(!verify_response(nonce, "wrong_key", &response));
        assert!(!verify_response("wrong_nonce", psk, &response));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
    }
}
