# 0006 — Implement B2 over the S3-compatible endpoint

Status: Accepted, 2026-06

## Context

snapdir supports Backblaze B2 as a `b2://` store. Backblaze offers both a native B2 API
and an S3-compatible API. The port already has a native, ring-only `aws-sdk-s3`-based
S3 store (ADR-0004, ADR-0005). The oracle's `b2://bucket/prefix` parsing is identical to
its `s3://` parsing.

## Decision

Implement `B2Store` as a thin wrapper over `S3Store`, configured with Backblaze's
S3-compatible regional endpoint (`https://s3.<region>.backblazeb2.com`). No store logic
is duplicated; the wrapper only supplies the endpoint URL and region. Region precedence
is argument > `SNAPDIR_B2_REGION` > `AWS_REGION` > a default. `b2://` URL parsing
matches `s3://` per the oracle. Authentication uses the standard AWS credential chain
(the B2 application key id/secret presented as the AWS access key id/secret).

## Alternatives considered

- **Implement the native B2 API.** Rejected: more code, a second transfer/auth path to
  maintain, and unnecessary given the S3-compatible endpoint behaves identically for
  snapdir's object operations.
- **A separate, copy-pasted S3-like store.** Rejected: it would duplicate push/fetch,
  retry, and verify logic that the S3 store already owns.

## Consequences

- B2 reuses the audited S3 push/fetch/verify path and adds no new dependencies.
- The S3-compatible endpoint must match the application key's region, which surfaced as
  a real operator-side test-environment requirement (the bucket region and key region
  must agree).
- The legacy Bash oracle's native-B2-CLI cold-fetch path is a separate, retired concern
  — see ADR-0023.
