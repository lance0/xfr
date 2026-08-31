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
| QUIC | TLS 1.3; identity unverified without PSK | TLS 1.3; exporter-bound PSK protection when enabled |

With PSK enabled, peers authenticate each other using HMAC-SHA256 proofs and
post-auth control messages are protected with ChaCha20-Poly1305. TCP and UDP
bulk payloads remain plaintext.

QUIC encrypts each TLS connection, but xfr uses a self-signed server certificate
without PKI verification. Without a PSK, server identity is not verified and an
active relay can terminate and forward the connection. With a PSK, xfr mixes the
RFC 9266 TLS exporter into the authentication proof and protected-control keys,
binding mutual authentication to the exact QUIC connection that carries the
bulk streams. A relay cannot splice authentication across two QUIC connections
without the PSK.

This binding is mandatory for QUIC + PSK: client and server refuse the session
unless both advertise `quic_channel_binding_v1`, so upgrade both peers together.
The self-signed certificate is still not a public PKI identity; use an
authenticated VPN when you need centrally managed endpoint identities or
isolation between different holders of a shared PSK.

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
