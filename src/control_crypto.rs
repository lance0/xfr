//! Protected control channel: AEAD-encrypted + authenticated messaging.
//!
//! When both peers advertise the `protected_control_v1` capability and PSK
//! authentication is active, the post-auth control channel switches from
//! plaintext newline-delimited JSON to length-prefixed AEAD frames.
//!
//! # Key derivation
//!
//! Keys are derived from the PSK and both handshake nonces using HKDF-SHA256:
//!
//! - Extract: `PRK = HKDF-Extract(salt = client_nonce || server_nonce, IKM = PSK)`
//! - Expand `K_c2s = HKDF-Expand(PRK, "xfr-protected-control-c2s", 32)`
//! - Expand `K_s2c = HKDF-Expand(PRK, "xfr-protected-control-s2c", 32)`
//!
//! Each direction gets its own 32-byte ChaCha20-Poly1305 key.
//!
//! # Framing
//!
//! Each protected message on the wire is:
//!
//! ```text
//!   [4 bytes: big-endian u32 payload length] [payload]
//! ```
//!
//! where payload is:
//!
//! ```text
//!   [8 bytes: u64 sequence number, little-endian]
//!   [ciphertext + 16-byte Poly1305 tag]
//! ```
//!
//! The sequence number is the AEAD nonce (zero-padded to 12 bytes) and also
//! part of the AAD (`seq || direction byte`), so reordering and replay are
//! detected.  Sequence numbers are per-direction, monotonically increasing
//! starting at 0.
//!
//! # Transcript binding
//!
//! The server proves possession of the PSK by sending a `server_proof` in
//! `AuthSuccess`: `HMAC-SHA256(K_s2s_auth, exact_client_hello_bytes ||
//! exact_server_hello_bytes)`.  This binds the full capability negotiation —
//! a MITM cannot strip `protected_control_v1` to force a plaintext downgrade
//! because the capability bytes are part of the MACed transcript.

use anyhow::{Result, anyhow};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use hmac_012::{Hmac, Mac};
use sha2_010::Sha256;

/// Capability string for the protected control channel.
pub const PROTECTED_CONTROL_CAPABILITY: &str = "protected_control_v1";

type HmacSha256 = Hmac<Sha256>;

/// Direction tag used as AAD and for key selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client → server.
    C2S,
    /// Server → client.
    S2C,
}

impl Direction {
    fn info(self) -> &'static [u8] {
        match self {
            Direction::C2S => b"xfr-protected-control-c2s",
            Direction::S2C => b"xfr-protected-control-s2c",
        }
    }

    fn tag(self) -> u8 {
        match self {
            Direction::C2S => 0x00,
            Direction::S2C => 0x01,
        }
    }
}

/// Nonce length for ChaCha20-Poly1305 (96 bits).
const NONCE_LEN: usize = 12;

/// Derive the two AEAD keys and the server-to-client transcript HMAC key.
///
/// Returns `(c2s_key, s2c_key, s2c_auth_key)` where the first two are 32-byte
/// ChaCha20-Poly1305 keys and `s2c_auth_key` is used for the `AuthSuccess`
/// server-proof HMAC.
fn derive_keys(
    client_nonce: &str,
    server_nonce: &str,
    psk: &str,
) -> Result<([u8; 32], [u8; 32], [u8; 32])> {
    let mut salt = Vec::with_capacity(client_nonce.len() + server_nonce.len());
    salt.extend_from_slice(client_nonce.as_bytes());
    salt.extend_from_slice(server_nonce.as_bytes());

    let hk = Hkdf::<Sha256>::new(Some(&salt), psk.as_bytes());

    let mut c2s_key = [0u8; 32];
    hk.expand(Direction::C2S.info(), &mut c2s_key)
        .map_err(|_| anyhow!("HKDF-Expand failed for c2s key"))?;

    let mut s2c_key = [0u8; 32];
    hk.expand(Direction::S2C.info(), &mut s2c_key)
        .map_err(|_| anyhow!("HKDF-Expand failed for s2c key"))?;

    let mut s2c_auth_key = [0u8; 32];
    hk.expand(b"xfr-protected-control-s2c-auth", &mut s2c_auth_key)
        .map_err(|_| anyhow!("HKDF-Expand failed for s2c auth key"))?;

    Ok((c2s_key, s2c_key, s2c_auth_key))
}

/// Compute the server's proof-of-PSK for `AuthSuccess`.
///
/// Binds the exact wire bytes of the client and server Hello messages so a
/// MITM cannot strip the `protected_control_v1` capability to force a
/// plaintext downgrade.
pub fn compute_server_proof(
    client_hello_bytes: &[u8],
    server_hello_bytes: &[u8],
    client_nonce: &str,
    server_nonce: &str,
    psk: &str,
) -> Result<String> {
    let (_, _, s2c_auth_key) = derive_keys(client_nonce, server_nonce, psk)?;
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(&s2c_auth_key).expect("HMAC can take key of any size");
    mac.update(client_hello_bytes);
    mac.update(server_hello_bytes);
    Ok(hex::encode(mac.finalize().into_bytes().as_slice()))
}

/// Verify a server proof (constant-time).
pub fn verify_server_proof(
    client_hello_bytes: &[u8],
    server_hello_bytes: &[u8],
    client_nonce: &str,
    server_nonce: &str,
    psk: &str,
    expected: &str,
) -> Result<bool> {
    let actual = compute_server_proof(
        client_hello_bytes,
        server_hello_bytes,
        client_nonce,
        server_nonce,
        psk,
    )?;
    Ok(constant_time_eq(actual.as_bytes(), expected.as_bytes()))
}

/// Stateful AEAD codec for one direction of the protected control channel.
pub struct ControlCodec {
    cipher: ChaCha20Poly1305,
    direction: Direction,
    /// Monotonically increasing sequence number; also used as the AEAD nonce.
    seq: u64,
}

impl ControlCodec {
    /// Create a codec for the given direction and key.
    pub fn new(key: &[u8; 32], direction: Direction) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new_from_slice(key)
                .expect("ChaCha20Poly1305 can take key of any size"),
            direction,
            seq: 0,
        }
    }

    /// Derive a pair of codecs from the PSK and nonces.
    ///
    /// Returns `(client_codec, server_codec)` — the client uses `C2S` for
    /// sends and `S2C` for receives; the server uses the opposite.
    pub fn derive_pair(
        client_nonce: &str,
        server_nonce: &str,
        psk: &str,
    ) -> Result<(ControlCodec, ControlCodec)> {
        let (c2s, s2c, _) = derive_keys(client_nonce, server_nonce, psk)?;
        Ok((
            ControlCodec::new(&c2s, Direction::C2S),
            ControlCodec::new(&s2c, Direction::S2C),
        ))
    }

    fn nonce(&self) -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        n[..8].copy_from_slice(&self.seq.to_le_bytes());
        n
    }

    fn aad(&self) -> [u8; 9] {
        let mut aad = [0u8; 9];
        aad[..8].copy_from_slice(&self.seq.to_le_bytes());
        aad[8] = self.direction.tag();
        aad
    }

    /// Encrypt a plaintext message, returning the on-wire payload (without
    /// the 4-byte length prefix — the caller adds that).
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce = self.nonce();
        let aad = self.aad();
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce.into(),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("AEAD encryption failed"))?;

        let mut frame = Vec::with_capacity(8 + ciphertext.len());
        frame.extend_from_slice(&self.seq.to_le_bytes());
        frame.extend_from_slice(&ciphertext);
        self.seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| anyhow!("sequence number overflow"))?;
        Ok(frame)
    }

    /// Decrypt an on-wire payload (the bytes after the 4-byte length prefix).
    pub fn open(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        if payload.len() < 8 + 16 {
            return Err(anyhow!("protected frame too short"));
        }
        let seq = u64::from_le_bytes(payload[..8].try_into().unwrap());
        if seq != self.seq {
            return Err(anyhow!(
                "sequence mismatch: expected {}, got {}",
                self.seq,
                seq
            ));
        }

        let nonce = self.nonce();
        let aad = self.aad();
        let plaintext = self
            .cipher
            .decrypt(
                &nonce.into(),
                Payload {
                    msg: &payload[8..],
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("AEAD decryption failed"))?;

        self.seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| anyhow!("sequence number overflow"))?;
        Ok(plaintext)
    }
}

/// Read/write abstraction that transparently handles plaintext vs AEAD-framed
/// control-channel I/O. After the handshake, all post-auth control messages
/// flow through this. Plaintext mode uses newline-delimited JSON; protected
/// mode uses length-prefixed AEAD frames.
pub enum ProtectedControl {
    /// Plaintext fallback (no PSK, or peer lacks `protected_control_v1`).
    Plaintext,
    /// AEAD-protected control channel.
    Protected {
        /// Codec for messages we *send* on this side.
        send: ControlCodec,
        /// Codec for messages we *receive* on this side.
        recv: ControlCodec,
    },
}

impl ProtectedControl {
    /// Create a plaintext transport.
    pub fn plaintext() -> Self {
        Self::Plaintext
    }

    /// Create a protected transport for the **client** side.
    /// Client sends on C2S, receives on S2C.
    pub fn protected_client(c2s: ControlCodec, s2c: ControlCodec) -> Self {
        debug_assert_eq!(c2s.direction, Direction::C2S);
        debug_assert_eq!(s2c.direction, Direction::S2C);
        Self::Protected {
            send: c2s,
            recv: s2c,
        }
    }

    /// Create a protected transport for the **server** side.
    /// Server sends on S2C, receives on C2S.
    pub fn protected_server(c2s: ControlCodec, s2c: ControlCodec) -> Self {
        debug_assert_eq!(c2s.direction, Direction::C2S);
        debug_assert_eq!(s2c.direction, Direction::S2C);
        Self::Protected {
            send: s2c,
            recv: c2s,
        }
    }

    /// Whether this transport is AEAD-protected.
    pub fn is_protected(&self) -> bool {
        matches!(self, Self::Protected { .. })
    }

    /// Split the transport into owned reader and writer halves.
    ///
    /// The reader half (`ControlReader`) owns the recv codec and can be
    /// moved into a dedicated control-reader task. The writer half
    /// (`ControlWriter`) owns the send codec and stays with the stats loop.
    pub fn split(self) -> (ControlReader, ControlWriter) {
        match self {
            Self::Plaintext => (ControlReader::Plaintext, ControlWriter::Plaintext),
            Self::Protected { send, recv } => (
                ControlReader::Protected { recv },
                ControlWriter::Protected { send },
            ),
        }
    }

    /// Read one control message from the transport.
    ///
    /// In plaintext mode, reads a newline-delimited line via `read_bounded_line`.
    /// In protected mode, reads an AEAD frame and decrypts it.
    ///
    /// Uses the *same* buffered reader the handshake used — no stranding of
    /// buffered bytes. In protected mode, the first 4 bytes are the frame
    /// length, then the AEAD payload; `BufReader` feeds both transparently.
    pub async fn read_message<R>(
        &mut self,
        reader: &mut R,
        line_buf: &mut String,
    ) -> anyhow::Result<()>
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        match self {
            Self::Plaintext => {
                use tokio::io::AsyncBufReadExt;
                line_buf.clear();
                let mut total = 0;
                loop {
                    const MAX_LINE_LENGTH: usize = 1024 * 1024;
                    let bytes = reader.fill_buf().await?;
                    if bytes.is_empty() {
                        return Ok(());
                    }
                    if let Some(pos) = bytes.iter().position(|&b| b == b'\n') {
                        let to_read = pos + 1;
                        if total + to_read > MAX_LINE_LENGTH {
                            return Err(anyhow!("Line exceeds maximum length"));
                        }
                        line_buf.push_str(&String::from_utf8_lossy(&bytes[..to_read]));
                        reader.consume(to_read);
                        return Ok(());
                    }
                    let len = bytes.len();
                    if total + len > MAX_LINE_LENGTH {
                        return Err(anyhow!("Line exceeds maximum length"));
                    }
                    line_buf.push_str(&String::from_utf8_lossy(bytes));
                    reader.consume(len);
                    total += len;
                }
            }
            Self::Protected { recv, .. } => {
                use tokio::io::AsyncReadExt;
                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf).await?;
                let len = u32::from_be_bytes(len_buf) as usize;
                const MAX_FRAME: usize = 1024 * 1024;
                if len > MAX_FRAME {
                    return Err(anyhow!("protected frame too large: {len} bytes"));
                }
                let mut payload = vec![0u8; len];
                reader.read_exact(&mut payload).await?;
                let plaintext = recv.open(&payload)?;
                // Store the decrypted message in `line_buf` so callers can
                // deserialize it the same way as plaintext.
                line_buf.clear();
                line_buf.push_str(&String::from_utf8_lossy(&plaintext));
                Ok(())
            }
        }
    }

    /// Write a serialized control message to the transport.
    ///
    /// In plaintext mode, appends `\n` and writes raw. In protected mode,
    /// seals the message in an AEAD frame and writes it with a 4-byte
    /// length prefix.
    pub async fn write_message<W>(
        &mut self,
        writer: &mut W,
        serialized: &str,
    ) -> std::io::Result<()>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::AsyncWriteExt;
        match self {
            Self::Plaintext => writer.write_all(format!("{serialized}\n").as_bytes()).await,
            Self::Protected { send, .. } => {
                let payload = send
                    .seal(serialized.as_bytes())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                let len = payload.len() as u32;
                writer.write_all(&len.to_be_bytes()).await?;
                writer.write_all(&payload).await
            }
        }
    }
}

/// Owned reader half of a [`ProtectedControl`] transport.
///
/// Split off via [`ProtectedControl::split`] so a dedicated control-reader
/// task can own the recv codec without borrowing the send half.
pub enum ControlReader {
    /// Plaintext fallback.
    Plaintext,
    /// AEAD-protected reader.
    Protected { recv: ControlCodec },
}

/// Owned writer half of a [`ProtectedControl`] transport.
///
/// Split off via [`ProtectedControl::split`] so the stats loop can own the
/// send codec while the reader task owns the recv codec.
pub enum ControlWriter {
    /// Plaintext fallback.
    Plaintext,
    /// AEAD-protected writer.
    Protected { send: ControlCodec },
}

impl ControlReader {
    /// Read one control message, storing the decoded line in `line_buf`.
    ///
    /// Plaintext: newline-delimited JSON via `fill_buf`/`consume`.
    /// Protected: 4-byte length prefix + AEAD frame, decrypted via `recv.open`.
    pub async fn read_message<R>(
        &mut self,
        reader: &mut R,
        line_buf: &mut String,
    ) -> anyhow::Result<()>
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        match self {
            Self::Plaintext => {
                use tokio::io::AsyncBufReadExt;
                line_buf.clear();
                let mut total = 0;
                loop {
                    const MAX_LINE_LENGTH: usize = 1024 * 1024;
                    let bytes = reader.fill_buf().await?;
                    if bytes.is_empty() {
                        return Ok(());
                    }
                    if let Some(pos) = bytes.iter().position(|&b| b == b'\n') {
                        let to_read = pos + 1;
                        if total + to_read > MAX_LINE_LENGTH {
                            return Err(anyhow!("Line exceeds maximum length"));
                        }
                        line_buf.push_str(&String::from_utf8_lossy(&bytes[..to_read]));
                        reader.consume(to_read);
                        return Ok(());
                    }
                    let len = bytes.len();
                    if total + len > MAX_LINE_LENGTH {
                        return Err(anyhow!("Line exceeds maximum length"));
                    }
                    line_buf.push_str(&String::from_utf8_lossy(bytes));
                    reader.consume(len);
                    total += len;
                }
            }
            Self::Protected { recv } => {
                use tokio::io::AsyncReadExt;
                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf).await?;
                let len = u32::from_be_bytes(len_buf) as usize;
                const MAX_FRAME: usize = 1024 * 1024;
                if len > MAX_FRAME {
                    return Err(anyhow!("protected frame too large: {len} bytes"));
                }
                let mut payload = vec![0u8; len];
                reader.read_exact(&mut payload).await?;
                let plaintext = recv.open(&payload)?;
                line_buf.clear();
                line_buf.push_str(&String::from_utf8_lossy(&plaintext));
                Ok(())
            }
        }
    }
}

impl ControlWriter {
    /// Write a serialized control message.
    ///
    /// Plaintext: appends `\n` and writes raw.
    /// Protected: seals in an AEAD frame with a 4-byte length prefix.
    pub async fn write_message<W>(
        &mut self,
        writer: &mut W,
        serialized: &str,
    ) -> std::io::Result<()>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::AsyncWriteExt;
        match self {
            Self::Plaintext => writer.write_all(format!("{serialized}\n").as_bytes()).await,
            Self::Protected { send } => {
                let payload = send
                    .seal(serialized.as_bytes())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                let len = payload.len() as u32;
                writer.write_all(&len.to_be_bytes()).await?;
                writer.write_all(&payload).await
            }
        }
    }
}

/// Constant-time comparison.
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

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_keys() -> ([u8; 32], [u8; 32], [u8; 32]) {
        derive_keys("deadbeef", "cafebabe", "shared-secret").unwrap()
    }

    #[test]
    fn derive_keys_is_deterministic() {
        let (c2s_a, s2c_a, auth_a) = test_keys();
        let (c2s_b, s2c_b, auth_b) = test_keys();
        assert_eq!(c2s_a, c2s_b);
        assert_eq!(s2c_a, s2c_b);
        assert_eq!(auth_a, auth_b);
    }

    #[test]
    fn derive_keys_differ_per_direction() {
        let (c2s, s2c, _) = test_keys();
        assert_ne!(c2s, s2c);
    }

    #[test]
    fn derive_keys_differ_per_psk() {
        let (c2s_a, _, _) = derive_keys("deadbeef", "cafebabe", "secret-a").unwrap();
        let (c2s_b, _, _) = derive_keys("deadbeef", "cafebabe", "secret-b").unwrap();
        assert_ne!(c2s_a, c2s_b);
    }

    #[test]
    fn derive_keys_differ_per_nonce() {
        let (c2s_a, _, _) = derive_keys("nonce1", "cafebabe", "secret").unwrap();
        let (c2s_b, _, _) = derive_keys("nonce2", "cafebabe", "secret").unwrap();
        assert_ne!(c2s_a, c2s_b);
    }

    #[test]
    fn seal_open_roundtrip() {
        let (c2s, _, _) = test_keys();
        let mut sender = ControlCodec::new(&c2s, Direction::C2S);
        let mut receiver = ControlCodec::new(&c2s, Direction::C2S);

        let msg = b"{\"type\":\"test\"}\n";
        let frame = sender.seal(msg).unwrap();
        let plaintext = receiver.open(&frame).unwrap();
        assert_eq!(plaintext, msg);
    }

    #[test]
    fn seal_open_multiple_messages() {
        let (c2s, _, _) = test_keys();
        let mut sender = ControlCodec::new(&c2s, Direction::C2S);
        let mut receiver = ControlCodec::new(&c2s, Direction::C2S);

        for i in 0..5 {
            let msg = format!("message-{i}\n");
            let frame = sender.seal(msg.as_bytes()).unwrap();
            let plaintext = receiver.open(&frame).unwrap();
            assert_eq!(plaintext, msg.as_bytes());
        }
    }

    #[test]
    fn direction_keys_are_isolated() {
        let (c2s, s2c, _) = test_keys();
        let mut sender = ControlCodec::new(&c2s, Direction::C2S);
        let mut wrong_receiver = ControlCodec::new(&s2c, Direction::S2C);

        let msg = b"hello\n";
        let frame = sender.seal(msg).unwrap();
        // Wrong key/direction should fail to decrypt.
        assert!(wrong_receiver.open(&frame).is_err());
    }

    #[test]
    fn sequence_replay_is_rejected() {
        let (c2s, _, _) = test_keys();
        let mut sender = ControlCodec::new(&c2s, Direction::C2S);
        let mut receiver = ControlCodec::new(&c2s, Direction::C2S);

        let frame = sender.seal(b"msg1\n").unwrap();
        receiver.open(&frame).unwrap(); // seq 0 — ok
        // Replaying the same frame must fail: receiver now expects seq 1.
        assert!(receiver.open(&frame).is_err()); // seq 0 again — rejected
    }

    #[test]
    fn sequence_mismatch_is_rejected() {
        let (c2s, _, _) = test_keys();
        let mut sender = ControlCodec::new(&c2s, Direction::C2S);
        let mut receiver = ControlCodec::new(&c2s, Direction::C2S);

        sender.seal(b"msg1\n").unwrap(); // seq 0 — not received
        let frame2 = sender.seal(b"msg2\n").unwrap(); // seq 1
        // Receiver expects seq 0, frame says seq 1.
        assert!(receiver.open(&frame2).is_err());
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let (c2s, _, _) = test_keys();
        let mut sender = ControlCodec::new(&c2s, Direction::C2S);
        let mut receiver = ControlCodec::new(&c2s, Direction::C2S);

        let frame = sender.seal(b"hello\n").unwrap();
        let mut tampered = frame.clone();
        tampered[10] ^= 0x01;
        assert!(receiver.open(&tampered).is_err());
    }

    #[test]
    fn server_proof_roundtrip() {
        let client_hello = b"{\"type\":\"hello\",\"version\":\"1.1\"}";
        let server_hello =
            b"{\"type\":\"hello\",\"version\":\"1.1\",\"auth\":{\"nonce\":\"cafebabe\"}}";
        let proof =
            compute_server_proof(client_hello, server_hello, "deadbeef", "cafebabe", "secret")
                .unwrap();
        assert!(
            verify_server_proof(
                client_hello,
                server_hello,
                "deadbeef",
                "cafebabe",
                "secret",
                &proof,
            )
            .unwrap()
        );
    }

    #[test]
    fn server_proof_rejects_wrong_psk() {
        let client_hello = b"client";
        let server_hello = b"server";
        let proof =
            compute_server_proof(client_hello, server_hello, "cn", "sn", "correct").unwrap();
        assert!(
            !verify_server_proof(client_hello, server_hello, "cn", "sn", "wrong", &proof,).unwrap()
        );
    }

    #[test]
    fn server_proof_rejects_modified_transcript() {
        let client_hello = b"client";
        let server_hello = b"server";
        let proof = compute_server_proof(client_hello, server_hello, "cn", "sn", "secret").unwrap();
        // Modified client hello bytes should fail.
        assert!(
            !verify_server_proof(
                b"client-modified",
                server_hello,
                "cn",
                "sn",
                "secret",
                &proof,
            )
            .unwrap()
        );
    }
}
