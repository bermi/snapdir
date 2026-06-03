# 0022 — Testing strategy: proptest, trycmd, cargo-fuzz

Status: Accepted, 2026-06

## Context

Beyond the differential and golden tests that pin the interop contract, the port needs
property-level confidence in the manifest parser/emitter, snapshot confidence in the CLI
surface, and robustness against malformed input — without making every commit slow.

## Decision

Adopt a layered testing strategy:

- **proptest** for manifest round-trips: `parse(emit(entry)) == entry`, fields recovered
  verbatim (including embedded spaces in paths), and whole-manifest emit→parse fixed
  points modulo `sort -k5`. These live in the `snapdir-core` crate against the public
  API; the frozen format files are untouched.
- **trycmd / assert_cmd** for CLI snapshot tests: top-level and per-subcommand `--help`,
  parse errors, and end-to-end push/fetch/checkout/verify round-trips, with the version
  redacted.
- **cargo-fuzz** for the manifest parser: a libfuzzer target feeding arbitrary bytes to
  the real public parser, asserting no panic and parse→Display→parse stability. The fuzz
  crate has its own `[workspace]` so it is isolated from the root workspace. It runs as a
  **nightly cron**, not per-commit, because fuzzing is open-ended.

## Alternatives considered

- **Example-based unit tests only.** Rejected: misses parser edge cases that property
  and fuzz testing surface.
- **Run the fuzzer on every commit.** Rejected: open-ended runtime; a nightly cron gives
  ongoing coverage without slowing the per-commit loop.

## Consequences

- The parser/emitter is checked for symmetry across a large input space.
- The CLI surface is snapshot-pinned, so accidental help/output changes are caught.
- Fuzzing provides ongoing robustness coverage off the critical path.
