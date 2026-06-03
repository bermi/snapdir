# 0013 — Enforce a 75% line-coverage floor

Status: Accepted, 2026-06

## Context

The coverage gate was originally a hollow check: an `echo` plus a human confirmation
over a CI job that ran `cargo llvm-cov … --fail-under-lines 0`. A floor of zero enforces
nothing, and the human-confirm step nearly produced a false pass.

## Decision

Enforce a real machine-checked line-coverage floor of **75%**:

```
cargo llvm-cov --workspace --all-features --locked --fail-under-lines 75 --summary-only
```

Both the local gate and the GitHub CI coverage job use the same floor (the CI job's
`--fail-under-lines 0` was changed to `75`). The human-confirm step was dropped in favour
of the machine check.

## Alternatives considered

- **`--fail-under-lines 0`** (the prior state). Rejected: enforces nothing.
- **A higher floor (e.g. 90%).** Rejected for now: the env-gated live cloud paths
  (s3/gcs/b2 stores) are uncovered hermetically — that interop is exercised by the shell
  harnesses, which are invisible to `llvm-cov`. Measured workspace coverage was ~79%, so
  75% is a meaningful floor that does not require credential-bearing CI. The floor was
  proven real (90 → fail, 75 → pass).

## Alternatives considered (cont.)

- **Keep the human-confirm.** Rejected per ADR-0018 (every checkpoint needs a machine
  check).

## Consequences

- Coverage regressions below 75% fail both locally and in CI.
- The floor accounts for the hermetically-uncovered live-cloud paths, which are covered
  instead by the integration harnesses.
- This was one of the near-false-passes that motivated the no-false-passes rule
  (ADR-0018).
