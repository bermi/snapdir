# 0001 — Differential-oracle methodology

Status: Accepted (superseded by [ADR-0024](0024-retire-the-bash-oracle.md)), 2026-06

## Context

The goal of the port was byte-for-byte interoperability with the existing Bash
`snapdir`: identical manifest lines, snapshot IDs, object/manifest keys, and bucket
layout, so caches and remote stores written by either implementation stay mutually
readable. A specification document alone was not a sufficient source of truth, because
the existing public docs carried known bugs (for example `--link` vs `--linked`,
`verify-transactions` vs `ensure-no-errors`) and several behaviours were only knowable
by reading the shell.

## Decision

Treat the frozen Bash scripts (`snapdir`, `snapdir-manifest`, the `snapdir-*-store`
helpers, `snapdir-sqlite3-catalog`) plus `utils/qa-fixtures/` as the executable source
of truth. Every Rust output was diffed against the live oracle: the manifest/ID
differential harness drove both implementations over a fixture corpus and compared
their output with `cmp`; the remote-store harnesses ran cross-tool push/fetch in both
directions. A divergence from the oracle was a defect in the Rust port, not in the
spec.

## Alternatives considered

- **Spec-only port.** Rejected: the docs were known-buggy and incomplete, so a
  spec-driven reimplementation would have encoded the doc bugs.
- **Manual golden snapshots transcribed once.** Rejected at this stage: static golden
  values cannot catch behaviours not anticipated by the author. (They become the
  anchor later — see ADR-0024 — but only after the contract was exhaustively pinned
  against the live oracle.)

## Consequences

- The Rust implementation was validated against real shell behaviour, catching subtle
  rules (symlink stat semantics, directory-size summation, sort/dedup hashing).
- The Bash scripts had to remain frozen and untouched for the duration of the port;
  they were read-only behavioural references.
- This methodology was historical: once the contract was fully captured as Rust
  golden-constant tests and a format SHA-lock, the live oracle was retired
  (ADR-0024). This record is kept for the historical rationale.
