# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 0.4.x   | ✓         |
| < 0.4   | ✗         |

## Reporting a Vulnerability

If you discover a security vulnerability in xfr, please report it by opening a GitHub issue at:

https://github.com/lance0/xfr/issues

For sensitive vulnerabilities that should not be disclosed publicly, please email the maintainer directly.

### What to include

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

### Response timeline

- Initial response: within 48 hours
- Status update: within 7 days
- Fix timeline: depends on severity

## Security Considerations

xfr is a network bandwidth testing tool. When running the server:

- The server accepts connections from any client by default
- Use `--psk` for pre-shared key authentication (HMAC-SHA256 challenge-response)
- Use `--allow` / `--deny` for IP-based access control lists
- Use `--rate-limit` to prevent abuse from individual IPs
- Consider firewall rules to restrict access
- Use `--one-off` mode for single-use testing

### Transport Encryption

| Mode | Encryption | Authentication |
|------|------------|----------------|
| TCP  | None       | PSK optional   |
| UDP  | None       | PSK optional   |
| QUIC | TLS 1.3    | PSK optional   |

For encrypted + authenticated connections, use QUIC with PSK:
```bash
xfr serve --psk "secretkey"
xfr <host> -Q --psk "secretkey"
```

When running the client:

- Only connect to trusted servers
- Use `--psk` when connecting to authenticated servers
- Use `-Q` (QUIC) for encrypted transport
- Be aware that bandwidth tests can saturate network links
