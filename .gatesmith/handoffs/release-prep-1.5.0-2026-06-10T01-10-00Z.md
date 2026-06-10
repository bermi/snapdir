# Handoff: release-prep-1.5.0 (phase 25)

## Summary

Prepared the 1.5.0 release, mirroring the release-prep-1.4.0 gate:

- Root `Cargo.toml`: bumped `[workspace.package] version` 1.4.0 → 1.5.0 and the
  three internal path-dep versions in `[workspace.dependencies]`
  (snapdir-core, snapdir-catalog, snapdir-stores) 1.4.0 → 1.5.0.
- Regenerated `Cargo.lock` via `cargo build --workspace`; the six internal
  package entries (core/catalog/stores/cli/ssh-store/benches) moved to 1.5.0.
  `cargo build --workspace --locked` is clean.
- `docs/rust-port/CHANGELOG.md`: renamed `## [Unreleased]` to
  `## [1.5.0] — 2026-06-10` (em dash, matching the existing `## [1.4.0] —
  2026-06-09` heading style), inserted a fresh empty `## [Unreleased]` section
  above it, and updated the compare-links footer:
  `[Unreleased]: ...compare/v1.5.0...HEAD` plus a new
  `[1.5.0]: ...compare/v1.4.0...v1.5.0` line (existing link style matched
  exactly).
- No edits to `.github/workflows/release.yml` (read-check only, see below).

## Files changed (git diff --stat)

```
 Cargo.lock                  | 12 ++++++------
 Cargo.toml                  |  8 ++++----
 docs/rust-port/CHANGELOG.md |  5 ++++-
 3 files changed, 14 insertions(+), 11 deletions(-)
```

## Local verification

- Gate check: `grep -qE '^version = "1.5.0"' Cargo.toml && grep -q '## \[1.5.0\]'
  docs/rust-port/CHANGELOG.md && cargo build --workspace --locked` → exit 0.
- `cargo test --workspace --locked` → **554 passed, 0 failed** across the
  workspace (unit + integration + trycmd + doc-tests).
- `typos docs/rust-port/CHANGELOG.md` → clean (no findings).

Last 20 lines of `cargo test --workspace --locked`:

```
running 2 tests
test crates/snapdir-core/src/store.rs - store::manifest_path (line 91) ... ok
test crates/snapdir-core/src/store.rs - store::object_path (line 69) ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.38s

   Doc-tests snapdir_ssh_store

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests snapdir_stores

running 1 test
test crates/snapdir-stores/src/router.rs - router::resolve_adapter (line 138) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.26s
```

## CHANGELOG completeness check

The 1.5.0 section (written by the ssh-docs gate) was read and verified
complete and accurate — no amendments were needed:

- **Added — `sftp://` + `ssh://` stores**: present (new `snapdir-ssh-store`
  crate, two external-store binaries, env families, ControlMaster
  multiplexing, un-weakenable security floor, OpenSSH ≥ 8.5, sync
  unsupported).
- **Added — SNAPPACK acceleration**: present (runtime `wire=1` negotiation,
  remote diff, single send-pack|receive-pack pipe, manifest-last,
  byte-identical, graceful fallback, `SNAPDIR_SSH_NO_ACCEL` /
  `_FORCE_ACCEL` / `_PULL_SENDALL`, spec link).
- **Added — hidden plumbing + `version --capabilities`**: present (one entry
  covering both: capabilities line format plus the three hidden subcommands
  `objects-needed` / `send-pack` / `receive-pack`, fail-closed validation,
  incremental BLAKE3).
- **Added — `StreamStore::objects_needed`**: present (defaulted trait method,
  order-preserving, fail-closed, across file/s3/gs/b2).
- **Fixed — external-store CLI wiring bug**: present (trees vs sharded store
  roots; push stages into the cache and pushes from the cache root; fetch
  lands in the cache root, manifest-last; built-in stores never affected).

## release.yml 5-crate confirmation

Read-check only (no edits). `.github/workflows/release.yml` line 335 publish
loop:

```
for crate in snapdir-core snapdir-catalog snapdir-stores snapdir-ssh-store snapdir-cli; do
```

All 5 crates present, in the required order core → catalog → stores →
ssh-store → cli. Note for the operator (already documented in the workflow
comments around lines 278–289): `snapdir-ssh-store` is a NEW crate name, so
its first publish cannot use Trusted Publishing.

## Other checks

- `grep -rn '1\.4\.0' Cargo.toml` → no matches (no version-related leftovers).
- All six member crates (`crates/*/Cargo.toml` + `benches/Cargo.toml`) inherit
  via `version.workspace = true`; no crate pins its own version. No crate
  references snapdir-ssh-store as a dependency, so no fourth internal
  path-dep version exists to bump.

Ready for PM verification: YES
