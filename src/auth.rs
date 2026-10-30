//! Pre-shared key (PSK) authentication
//!
//! Implements challenge-response authentication using HMAC-SHA256.

use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use sha2::Sha256;
use std::io::Read;

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
    compute_response_with_key(nonce, psk.as_bytes())
}

/// Compute a response from binary key material.
///
/// QUIC uses this with a session PSK that is bound to the connection's TLS
/// exporter. The public string-based API remains the stable path for TCP.
pub(crate) fn compute_response_with_key(nonce: &str, key: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(nonce.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

/// Verify an authentication response
pub fn verify_response(nonce: &str, psk: &str, response: &str) -> bool {
    verify_response_with_key(nonce, psk.as_bytes(), response)
}

/// Verify a response made from binary key material.
pub(crate) fn verify_response_with_key(nonce: &str, key: &[u8], response: &str) -> bool {
    let expected = compute_response_with_key(nonce, key);
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

/// Reject secret-bearing files that grant any access to group or other users.
#[cfg(unix)]
pub(crate) fn require_owner_only_permissions(
    path: &std::path::Path,
    metadata: &std::fs::Metadata,
    description: &str,
) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        anyhow::bail!(
            "{} {:?} has overly broad permissions ({:03o}): group/other access is not allowed",
            description,
            path,
            mode
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn service_credential_permissions(
    owner: u32,
    mode: u32,
    regular: bool,
    acl: &[u8],
    uid: u32,
) -> bool {
    let mut expected = [
        2, 0, 0, 0, // POSIX ACL version 2
        1, 0, 4, 0, 255, 255, 255, 255, // owner: read
        2, 0, 4, 0, 0, 0, 0, 0, // named service user: read
        4, 0, 0, 0, 255, 255, 255, 255, // owning group: none
        16, 0, 4, 0, 255, 255, 255, 255, // mask: read
        32, 0, 0, 0, 255, 255, 255, 255, // other: none
    ];
    expected[16..20].copy_from_slice(&uid.to_le_bytes());
    owner == 0 && mode & 0o7777 == 0o440 && regular && acl == expected
}

#[cfg(target_os = "linux")]
fn is_service_credential(file: &std::fs::File, metadata: &std::fs::Metadata) -> bool {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let mut acl = [0; 44];
    // The ACL mask makes systemd's named-user read grant appear as group-readable.
    let size = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c"system.posix_acl_access".as_ptr(),
            acl.as_mut_ptr().cast(),
            acl.len(),
        )
    };
    size == acl.len() as isize
        && service_credential_permissions(
            metadata.uid(),
            metadata.mode(),
            metadata.is_file(),
            &acl,
            unsafe { libc::geteuid() },
        )
}

/// Read PSK from file, trimming whitespace.
///
/// On Unix, rejects files that are readable or writable by anyone other than
/// the owner. Linux also accepts the root-owned read-only ACL pattern used by
/// systemd credentials, granting read access only to the effective service UID.
pub fn read_psk_file(path: &std::path::Path) -> anyhow::Result<String> {
    use anyhow::Context;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to open PSK file {}", path.display()))?;

    #[cfg(unix)]
    {
        // Inspect the open handle, not the path, so the permissions and bytes
        // always belong to the same inode even if a symlink or path is swapped.
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect PSK file {}", path.display()))?;
        if let Err(error) = require_owner_only_permissions(path, &metadata, "PSK file") {
            #[cfg(target_os = "linux")]
            let credential = is_service_credential(&file, &metadata);
            #[cfg(not(target_os = "linux"))]
            let credential = false;
            if !credential {
                return Err(error);
            }
        }
    }

    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("failed to read PSK file {}", path.display()))?;
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

    /// Validate authentication settings before any network resources are used.
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(psk) = self.psk.as_deref() {
            validate_psk(psk)?;
        }
        Ok(())
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
        for mode in [0o440, 0o644, 0o660] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            let err = read_psk_file(&path).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("overly broad permissions"), "{mode:o}: {msg}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_read_psk_file_accepts_owner_only_permissions() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.psk");
        fs::write(&path, "owner-only-key\n").unwrap();
        for mode in [0o600, 0o400] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            let psk = read_psk_file(&path).unwrap();
            assert_eq!(psk, "owner-only-key");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn service_credential_accepts_only_the_read_only_service_acl() {
        let acl = [
            2, 0, 0, 0, 1, 0, 4, 0, 255, 255, 255, 255, 2, 0, 4, 0, 232, 3, 0, 0, 4, 0, 0, 0, 255,
            255, 255, 255, 16, 0, 4, 0, 255, 255, 255, 255, 32, 0, 0, 0, 255, 255, 255, 255,
        ];
        assert!(service_credential_permissions(
            0, 0o100440, true, &acl, 1000
        ));
        for (owner, mode, regular, uid) in [
            (1000, 0o440, true, 1000),
            (0, 0o640, true, 1000),
            (0, 0o444, true, 1000),
            (0, 0o450, true, 1000),
            (0, 0o2440, true, 1000),
            (0, 0o440, false, 1000),
            (0, 0o440, true, 1001),
        ] {
            assert!(!service_credential_permissions(
                owner, mode, regular, &acl, uid
            ));
        }
        for index in 0..acl.len() {
            let mut changed = acl;
            changed[index] ^= 1;
            assert!(
                !service_credential_permissions(0, 0o440, true, &changed, 1000),
                "changed ACL byte {index} must fail"
            );
        }
        for invalid in [acl[..43].to_vec(), [acl.as_slice(), &[0; 8]].concat()] {
            assert!(!service_credential_permissions(
                0, 0o440, true, &invalid, 1000
            ));
        }
    }

    #[test]
    #[cfg(unix)]
    fn psk_symlink_permissions_are_checked_on_the_target() {
        use std::fs;
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("key");
        let link = dir.path().join("link");
        fs::write(&target, "symlink-test-key").unwrap();
        symlink(&target, &link).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_psk_file(&link).is_err());
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_psk_file(&link).unwrap(), "symlink-test-key");
    }

    #[test]
    fn test_read_psk_file_errors_include_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.psk");

        let error = read_psk_file(&path).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("failed to open PSK file"));
        assert!(message.contains(path.to_str().unwrap()));
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

    #[test]
    fn test_auth_config_validates_optional_psk() {
        assert!(AuthConfig::default().validate().is_ok());
        assert!(
            AuthConfig {
                psk: Some("secret".to_string()),
            }
            .validate()
            .is_ok()
        );
        assert!(
            AuthConfig {
                psk: Some(String::new()),
            }
            .validate()
            .is_err()
        );
        assert!(
            AuthConfig {
                psk: Some("x".repeat(MAX_PSK_LENGTH + 1)),
            }
            .validate()
            .is_err()
        );
    }
}
