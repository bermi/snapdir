# 0018 — No false passes: every checkpoint has a machine check

Status: Accepted, 2026-06

## Context

Two gates nearly passed without verifying anything. The `remote-interop` gate's
verification was an `echo` followed by a lone human-confirm — a "Yes, all passed" with
zero evidence almost became a recorded pass; the operator flagged it
("I DON'T KNOW, YOU HAVE TO VERIFY"). The `coverage-gate` was an `echo` plus a
human-confirm over a CI job with a `--fail-under-lines 0` floor that enforced nothing
(ADR-0013).

## Decision

Adopt a hard rule: **no false passes**. Every human checkpoint must be backed by a
machine check that actually exercises the thing being signed off.

- `remote-interop` was changed to run a live differential harness (MinIO S3 Bash↔Rust
  cross-tool plus a zero-external-dependency lane), asserting `exit_code 0` and specific
  regex evidence; the hollow human-confirm was dropped.
- `coverage-gate` was changed to a real `--fail-under-lines 75` machine check
  (ADR-0013).

A human sign-off may remain, but only *in addition to* a passing machine check — never
as the sole gate.

## Alternatives considered

- **Trust human confirmation.** Rejected: it directly produced the two near-misses.
- **Drop human checkpoints entirely.** Rejected: some decisions (freeze, release
  sign-off) warrant human judgment — but always over real evidence.

## Consequences

- Verification commands must run something meaningful; an `echo`/`true` placeholder is a
  defect.
- Pass-criteria assert concrete evidence (exit codes, regex over harness output).
- This rule caught and corrected the GCS NotFound bug (ADR-0009) and the B2 credential
  and endpoint issues (ADR-0023), because the real harness ran instead of being
  rubber-stamped.
