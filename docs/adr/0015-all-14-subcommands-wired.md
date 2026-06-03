# 0015 — Wire all 14 CLI subcommands, no stubs

Status: Accepted, 2026-06

## Context

The CLI surface reproduces the oracle's orchestrator subcommands: `manifest`, `id`,
`stage`, `push`, `fetch`, `pull`, `checkout`, `verify`, `verify-cache`, `flush-cache`,
`locations`, `ancestors`, `revisions`, `defaults`. Mid-port, 7 of these were still CLI
stubs that printed "not implemented yet", even though the underlying library logic
already existed (`snapdir-core::cache` had `verify_cache`/`flush_cache`;
`snapdir-catalog` had `locations`/`ancestors`/`revisions`/`rebuild`). Release was
conditioned on feature-completeness.

## Decision

Wire all 14 subcommands to real implementations — no stubs — before release:

- `stage`/`verify-cache`/`flush-cache` → `snapdir-core::cache`.
- `locations`/`ancestors`/`revisions` → `snapdir-catalog` with the frozen JSON
  serializers (ADR-0008).
- `defaults` → reproduces the oracle's `snapdir_defaults` env/option output.

A trycmd surface snapshot and assert_cmd end-to-end tests cover the wired commands; the
acceptance check guards against vacuous test filters (`running [1-9]`).

## Alternatives considered

- **Ship with some commands stubbed.** Rejected: the operator required
  feature-completeness for release; stubs are not a releasable surface.
- **Drop the unimplemented subcommands from the surface.** Rejected: the surface must
  reproduce the oracle's 14 subcommands for compatibility.

## Consequences

- The CLI is feature-complete; no subcommand prints "not implemented yet".
- A few honest divergences are documented where the single binary has no separate
  helper binaries (e.g. `defaults` bin-path lines).
- Release sign-off could proceed once all 14 were wired and the migration guide
  refreshed to match.
