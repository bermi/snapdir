# 0025 — Keep native-certs in the scratch image

Status: Accepted, 2026-06

## Context

The shipped Docker image is `FROM scratch` with the static musl binary plus CA
certificates via native-certs (ADR-0012). During the Phase 11 modernization, one option
was to drop native-certs and bundle a webpki root set into the binary, which would make
the image literally binary-only. snapdir's stores include private and self-hosted
S3-compatible endpoints, whose certificates chain to roots a fixed bundled set may not
contain.

## Decision

Keep native-certs (and ship the CA certificate bundle in the scratch image) rather than
switching to a webpki-bundled root set. The image stays a static binary plus CA certs.
The design intent is "zero runtime executables" — no shell, no package manager — not
"literally only a binary".

## Alternatives considered

- **Bundle webpki-roots into the binary, ship a binary-only image.** Rejected: a fixed
  bundled root set can fail to validate private/self-hosted S3-compatible endpoints whose
  CAs are installed system-wide; native-certs reads the platform/system trust store and
  is more compatible with those deployments.

## Consequences

- TLS to private and self-hosted endpoints keeps working because the image ships a real
  CA bundle.
- The "zero runtime executables" property holds (no shell/package manager), even though
  the image is not literally binary-only.
- No change to the stores' TLS stack was needed for this decision; it is purely about
  what the image carries.
