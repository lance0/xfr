# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

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
- Consider firewall rules to restrict access
- Use `--one-off` mode for single-use testing
- The server does not implement authentication

When running the client:

- Only connect to trusted servers
- Be aware that bandwidth tests can saturate network links
