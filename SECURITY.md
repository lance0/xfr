# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| Latest release | ✓ |
| Older releases | ✗ |

## Reporting a Vulnerability

Report security vulnerabilities through [GitHub private vulnerability
reporting](https://github.com/lance0/xfr/security/advisories/new). Do not include
vulnerability details in a public issue.

### What to include

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

## Security Considerations

xfr is a network bandwidth testing tool. When running the server:

- The server accepts connections from any client by default
- Use `--psk` for mutual pre-shared key authentication and protected control
  messages
- Use `--allow` / `--deny` for IP-based access control lists
- Use `--rate-limit` to prevent abuse from individual IPs
- Consider firewall rules to restrict access
- Use `--one-off` mode for single-use testing

### Transport Encryption

| Mode | Bulk test data | Control channel |
|------|----------------|-----------------|
| TCP  | Plaintext | Plaintext without PSK; ChaCha20-Poly1305 after PSK authentication |
| UDP  | Plaintext | Uses the same TCP control channel behavior |
| QUIC | TLS 1.3; server identity is not verified | TLS 1.3; PSK can add protected application control |

With PSK enabled, peers authenticate each other using HMAC-SHA256 proofs and
post-auth control messages are protected with ChaCha20-Poly1305. TCP and UDP
bulk payloads remain plaintext.

QUIC encrypts each TLS connection, but xfr uses a self-signed server certificate
without PKI verification. The PSK proof and protected control channel are not
bound to that TLS session or to its bulk streams. An active relay can therefore
terminate and forward QUIC connections while accessing the bulk payload. QUIC
with PSK is not end-to-end bulk-data authentication or confidentiality against
such a relay; use an authenticated VPN when those properties are required.

To combine QUIC transport encryption with PSK-protected control:
```bash
xfr serve --psk "secretkey"
xfr <host> -Q --psk "secretkey"
```

Use a strong, high-entropy PSK (16+ random characters) to prevent offline brute-force attacks.

When running the client:

- Only connect to trusted servers
- Use `--psk` when connecting to authenticated servers
- Use `-Q` (QUIC) for encrypted transport
- Be aware that bandwidth tests can saturate network links
