# 0020 — Interop-diff keystone gate

Status: Accepted, 2026-06

## Context

Byte-for-byte manifest interoperability is the hard constraint of the port. If the Rust
`snapdir manifest`/`id` ever diverged from the oracle, every downstream feature (stores,
catalog, CLI) would be built on a broken foundation. This needed to be proven before any
downstream phase unlocked.

## Decision

Make `interop-diff` a hard keystone gate that downstream phases depend on. A differential
harness (`tests/interop/run.sh`) drives both the Bash oracle and the Rust binary over a
deterministic fixture corpus (nested, symlinks, perms, unicode, spaces, empty, duplicate,
large) and compares output with `cmp` across all checksum modes:
`b3sum`/`md5sum`/`sha256sum`, keyed mode, and `--no-follow`. The gate passes only when
manifests and snapshot IDs are byte-identical across every case on both Linux and macOS,
with zero hard diffs. It is a human checkpoint backed by the harness (ADR-0018).

Because the harness initially could not pass while `snapdir manifest` was still a stub,
prerequisite gates (`core-walk`, `cli-manifest-wire`) were added to wire the real
filesystem walk first.

## Alternatives considered

- **Spot-check a few manifests by hand.** Rejected: cannot prove byte-identity across the
  format's many rules and modes.
- **Defer interop validation until later.** Rejected: a late-discovered format diff would
  invalidate downstream work; this gate fences downstream phases on purpose.

## Consequences

- Byte-for-byte interop was proven (15/15 corpus cases identical) before stores/catalog
  work proceeded.
- Any manifest or ID diff freezes downstream until resolved.
- The harness later seeded the pattern reused for remote-store interop (ADR-0023).
