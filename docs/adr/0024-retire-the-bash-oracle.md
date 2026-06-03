# 0024 — Retire the Bash oracle (full cut)

Status: Accepted, 2026-06

## Context

During the port the frozen Bash scripts and `utils/qa-fixtures/` served as the live
behavioural reference, against which every Rust output was checked. The port is now
complete (all interop proven), and the byte contract is fully captured. Keeping the Bash
scripts and their comparison tooling around indefinitely carries maintenance and
confusion cost, and the legacy B2 cold-fetch path is a known dead end (ADR-0023).

## Decision

Retire the Bash oracle with a full cut:

- Delete the 8 root Bash scripts, the root bash-era Dockerfile, the pre-commit hook, and
  `utils/qa-fixtures/`.
- Remove the bash test scaffolding and de-bash CI.
- Re-anchor the byte contract as **pure-Rust golden-constant tests**
  (`crates/snapdir-core/tests/compat_golden.rs`), which assert the frozen byte layout
  directly. The bash golden fixtures are dropped (they are gone with the scripts).
- Add a repo-wide grep guard so no bash references remain. Keep the `ExternalStore` shim
  (it is a runtime feature, not test scaffolding).

The live oracle is gone; correctness is henceforth defined by the Rust golden tests.

## Alternatives considered

- **Keep the oracle as a permanent reference.** Rejected: ongoing maintenance of retired
  shell code, and it tempts re-running a comparison model the port has outgrown.
- **Archive the bash rather than delete it.** Rejected: the operator chose a clean
  deletion; history preserves it in git.

## Consequences

- The repository is Rust-only; no shell scripts to maintain or accidentally depend on.
- The byte contract is now the golden-constant tests, a self-contained anchor that lands
  before the scripts are deleted.
- The shift is deliberate: the live behavioural comparison is replaced by static golden
  assertions.
