//! Pre-shared key (PSK) authentication
//!
//! Implements challenge-response authentication using HMAC-SHA256.

use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const NONCE_LENGTH: usize = 32;
/// Maximum PSK length to prevent performance issues with very long keys
pub const MAX_PSK_LENGTH: usize = 1024;

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

/// Constant-time comparison to prevent timing attacks.
///
/// Padding to the length of the longer input avoids the early-return that
/// would otherwise leak whether the two slices have different lengths. The
/// length difference is folded into the accumulator so that trailing zero
/// bytes do not accidentally produce equality.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len().max(b.len());
    let mut result = 0u8;
    for i in 0..len {
        let x = a.get(i).unwrap_or(&0);
        let y = b.get(i).unwrap_or(&0);
        result |= x ^ y;
    }
    // Fold any length mismatch into the result so `b"x"` and `b"x\0"` are
    // not reported as equal.
    result |= (a.len() != b.len()) as u8;
    result == 0
}

/// Read PSK from file, trimming whitespace.
///
/// On Unix, rejects files that are readable or writable by anyone other than
/// the owner (mode bits for group/other are set), because such permissions
/// would let other users on the host read the pre-shared key.
pub fn read_psk_file(path: &std::path::Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path)?;
        let mode = metadata.mode() & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "PSK file {:?} has overly broad permissions ({:03o}): group/other access is not allowed",
                path,
                mode
            );
        }
    }

    let content = std::fs::read_to_string(path)?;
    let psk = content.trim().to_string();
    validate_psk(&psk)?;
    Ok(psk)
}

/// Validate PSK length
pub fn validate_psk(psk: &str) -> anyhow::Result<()> {
    if psk.len() > MAX_PSK_LENGTH {
        anyhow::bail!(
            "PSK exceeds maximum length of {} bytes (got {} bytes)",
            MAX_PSK_LENGTH,
            psk.len()
        );
    }
    if psk.is_empty() {
        anyhow::bail!("PSK cannot be empty");
    }
    Ok(())
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

        // Same value padded with a zero byte should compare unequal without
        // short-circuiting on length.
        let left = b"secret";
        let mut right = b"secret".to_vec();
        right.push(0);
        assert!(!constant_time_eq(left, &right));

        // Comparing an empty slice to a non-empty slice should return false.
        assert!(!constant_time_eq(b"", b"x"));
    }

    #[test]
    #[cfg(unix)]
    fn test_read_psk_file_rejects_group_or_other_readable() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leaky.psk");
        fs::write(&path, "super-secret-key").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let err = read_psk_file(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("overly broad permissions"),
            "error should flag permissions: {}",
            msg
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_read_psk_file_accepts_owner_only_permissions() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.psk");
        fs::write(&path, "owner-only-key\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        let psk = read_psk_file(&path).unwrap();
        assert_eq!(psk, "owner-only-key");
    }

    #[test]
    fn test_validate_psk_valid() {
        assert!(validate_psk("secret").is_ok());
        assert!(validate_psk("a").is_ok());
        assert!(validate_psk(&"x".repeat(MAX_PSK_LENGTH)).is_ok());
    }

    #[test]
    fn test_validate_psk_empty() {
        assert!(validate_psk("").is_err());
    }

    #[test]
    fn test_validate_psk_too_long() {
        let long_psk = "x".repeat(MAX_PSK_LENGTH + 1);
        assert!(validate_psk(&long_psk).is_err());
    }
}
