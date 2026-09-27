# Security policy

## Supported versions

Only the latest release receives security fixes.

## Reporting a vulnerability

Report vulnerabilities privately through
[GitHub's private vulnerability reporting](https://github.com/nillia/PocketSpot/security/advisories/new),
not in a public issue. Include the version, the device and firmware, and the
steps to reproduce. Expect an acknowledgement within a week.

## Sensitive data

PocketSpot stores a Spotify login on the device. Never include any of these
in reports, issues, pull requests or attachments:

- access or refresh tokens, credentials files or session files
- pairing codes or pairing URLs
- unredacted logs

If a log is needed, check it first and remove anything that looks like a
token, code or URL query string.
