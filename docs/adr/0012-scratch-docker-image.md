# 0012 — Ship a `FROM scratch` Docker image

Status: Accepted, 2026-06

## Context

The Bash version shipped as an Alpine + Bash image carrying the shell scripts and their
CLI dependencies. The Rust port is a single statically-linked musl binary with zero
runtime dependencies (ADR-0005, ADR-0011), so it needs neither a shell nor a package
manager at runtime.

## Decision

Ship a `FROM scratch` Docker image containing only the static musl binary plus CA
certificates (via native-certs), replacing the old Alpine + Bash image. The image is
"zero runtime executables": no shell, no package manager — just the binary and the trust
store it needs for TLS.

## Alternatives considered

- **Alpine/`distroless` base.** Rejected: unnecessary for a static binary and larger;
  `scratch` is the minimal correct base.
- **Binary only, no CA certificates.** Rejected: TLS to S3/GCS/B2 and especially to
  private/self-hosted S3-compatible endpoints needs a trust store. CA certs are kept on
  purpose — see ADR-0025.

## Consequences

- The image is minimal (just binary + CA certs), replacing the heavier Alpine image.
- TLS works against public and private endpoints because the CA bundle ships.
- The image depends on the static musl build staying clean (ADR-0004, ADR-0011).
