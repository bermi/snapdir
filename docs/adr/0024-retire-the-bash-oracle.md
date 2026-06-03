# 0024 — Retire the Bash oracle (full cut)

Status: Accepted (supersedes [ADR-0001](0001-differential-oracle-methodology.md)), 2026-06

## Context

The differential-oracle methodology (ADR-0001) used the frozen Bash scripts and
`utils/qa-fixtures/` as the live behavioural source of truth, diffing every Rust output
against them. By Phase 11 the port is complete (all interop proven), and the byte
contract is fully captured. Keeping the Bash scripts and their differential harnesses
around indefinitely carries maintenance and confusion cost, and the legacy B2 cold-fetch
path is a known dead end (ADR-0023).

## Decision

Retire the Bash oracle with a full cut:

- Delete the 8 root Bash scripts, the root bash-era Dockerfile, the pre-commit hook, and
  `utils/qa-fixtures/`.
- Remove the bash test harnesses and de-bash CI.
- Re-anchor the byte contract as **pure-Rust golden-constant tests** plus the retained
  `manifest-format.sha.lock`. The golden-fixtures lock is dropped (its fixtures are
  gone); the format lock stays.
- Archive the oracle-differential gates, rewrite the PM prompt, and add a repo-wide grep
  guard so no bash references remain. Keep the `ExternalStore` shim (it is a runtime
  feature, not oracle scaffolding).

This supersedes ADR-0001: the live oracle is gone; correctness is henceforth defined by
the Rust golden tests and the format SHA-lock.

## Alternatives considered

- **Keep the oracle as a permanent reference.** Rejected: ongoing maintenance of retired
  shell code, and it tempts re-running a differential model the port has outgrown.
- **Archive the bash rather than delete it.** Rejected: the operator chose a clean
  deletion; history preserves it in git.

## Consequences

- The repository is Rust-only; no shell scripts to maintain or accidentally depend on.
- The byte contract is now the golden-constant tests + `manifest-format.sha.lock`, a
  self-contained anchor that must land before deletion (the `compat-golden-tests` gate).
- The methodology shift is deliberate: the live oracle-differential model is replaced by
  static golden assertions.
