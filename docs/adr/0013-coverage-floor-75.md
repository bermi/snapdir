# 0013 — Enforce a 75% line-coverage floor

Status: Accepted, 2026-06

## Context

The coverage check was originally hollow: an `echo` plus a manual confirmation over a CI
job that ran `cargo llvm-cov … --fail-under-lines 0`. A floor of zero enforces nothing,
and the manual confirmation could mask a regression.

## Decision

Enforce a real machine-checked line-coverage floor of **75%**:

```
cargo llvm-cov --workspace --all-features --locked --fail-under-lines 75 --summary-only
```

Both the local check and the GitHub CI coverage job use the same floor (the CI job's
`--fail-under-lines 0` was changed to `75`). The manual confirmation was dropped in
favour of the machine check.

## Alternatives considered

- **`--fail-under-lines 0`** (the prior state). Rejected: enforces nothing.
- **A higher floor (e.g. 90%).** Rejected for now: the env-gated live cloud paths
  (s3/gcs/b2 stores) are uncovered hermetically — that interop is exercised by the live
  integration tests, which are invisible to `llvm-cov`. Measured workspace coverage was
  ~79%, so 75% is a meaningful floor that does not require credential-bearing CI. The
  floor was proven real (90 → fail, 75 → pass).
- **Keep the manual confirmation.** Rejected: a coverage claim must be backed by an
  automated check, not a manual sign-off that can drift from reality.

## Consequences

- Coverage regressions below 75% fail both locally and in CI.
- The floor accounts for the hermetically-uncovered live-cloud paths, which are covered
  instead by the live integration tests.
- Replacing the manual confirmation with a machine check removes a step that could
  otherwise report a pass without actually measuring coverage.
