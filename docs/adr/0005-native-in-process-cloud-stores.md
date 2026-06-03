# 0005 — Native in-process cloud stores, no shelling out

Status: Accepted, 2026-06

## Context

The Bash version drives the `aws`, `gcloud`, and `b2` command-line tools (and `b3sum`,
`sqlite3`) as subprocesses. The port's goal is a single statically-linked binary with
zero runtime dependencies: requiring those external CLIs at runtime would defeat that.

## Decision

Implement all cloud stores in-process using native Rust SDKs: `aws-sdk-s3` for S3 and
B2, `google-cloud-storage` for GCS. The shipped binary never shells out to `aws`,
`gcloud`, `b2`, `b3sum`, or `sqlite3`. External system binaries are permitted only in
the test suite, never in `crates/`. The emit-shell-command shim is retained
only for third-party external stores, dispatched by the store router.

This is proven by a zero-external-dependency test that runs the Rust round-trip
with `aws`, `b2`, and `gcloud` removed from `PATH` via a sanitized symlink farm.

## Alternatives considered

- **Shell out to the cloud CLIs** (as Bash does). Rejected: adds runtime dependencies,
  defeats the static single-binary goal, and is slower (process-per-operation).
- **Re-sign requests by hand over a thin HTTP client.** Rejected: the SDKs already
  handle SigV4, credential chains, retries, and endpoint resolution correctly.

## Consequences

- Authentication is delegated to each SDK's own credential chain (no bespoke snapdir
  env vars), simplifying the auth story.
- The binary depends on the SDK crates and their TLS stack — see ADR-0004 for the
  resulting `ring`/aws-lc constraint.
- The PATH-sanitized test is a standing guard that no accidental shell-out slips in.
