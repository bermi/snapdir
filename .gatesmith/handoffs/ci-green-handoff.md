# ci-green (Phase 11) — LOCAL green-up handoff

## Summary

`ci.yaml` went red on the upgraded/de-bashed tree. The oracle test failures were
already cleared by `remove-bash-test-harnesses`; the remaining failures were purely
MECHANICAL. This gate fixed all of them and brought the **full local CI-equivalent
suite** (`utils/ci/pre-push.sh`) to a clean **exit 0**:

1. **rustfmt drift** (test code only) — fixed with `cargo fmt --all`. Only three test
   files changed: `crates/snapdir-cli/tests/manifest.rs`,
   `crates/snapdir-core/src/walk.rs` (test module, >line 554), and
   `crates/snapdir-core/tests/compat_golden.rs`. Production code untouched; the three
   sha-locked files did **not** change.
2. **cargo-shear** — removed the unused `snapdir-core.workspace = true` from
   `crates/snapdir-catalog/Cargo.toml` (zero `snapdir_core` usage in its `src/`).
   Shear surfaced a **second, pre-existing** misplacement the gate brief had not listed:
   `benches/Cargo.toml` carried `snapdir-core` in `[dependencies]` though it is used
   only by the `hot_paths` bench (a dev/bench target; `benches/src/lib.rs` is doc-only).
   Moved it to `[dev-dependencies]` — the canonical shear fix. Both build green.
3. **typos** — every finding was a FALSE POSITIVE on intentional content, so they were
   handled via `_typos.toml` config (NOT by editing golden data or identifiers):
   - `persit` → the VERBATIM (misspelled) function name `_snapdir_file_store_persit`
     from the now-deleted Bash oracle `snapdir-file-store` (confirmed against
     `git show 98cd355^:snapdir-file-store` line 303); the doc-comments quote it exactly.
   - `anc` / `anc_lines` / `oracle_anc` — local variable identifiers in the catalog tests.
   - `CHECKs` — the SQL `CHECK` keyword pluralized in a prose comment.
   - `ba` (`8af03a1b…ba82b9be`) and `caf` (`8aed4caf…`) — fragments of the FROZEN golden
     content-address digests in `manifest-spec.md` / `walk.rs` tests. Preserved as data;
     never "corrected".
   No prose/comment/golden bytes were mangled.

## Files changed (git diff --stat — committed by PM as 6932d7f)

    Cargo.lock                                 |  1 -
    _typos.toml                                | 14 +++++++++++++-
    benches/Cargo.toml                         |  5 +++--
    crates/snapdir-catalog/Cargo.toml          |  1 -
    crates/snapdir-cli/tests/manifest.rs       |  5 ++++-
    crates/snapdir-core/src/walk.rs            | 31 ++++++++++++++++++++++++------
    crates/snapdir-core/tests/compat_golden.rs | 16 ++++++++-------
    7 files changed, 54 insertions(+), 19 deletions(-)

`Cargo.lock`: the only change is removing the `snapdir-core` line from
`snapdir-catalog`'s dependency block (one line). `--locked` build green afterward.

## Local verification result

**`bash utils/ci/pre-push.sh` → exit 0 (PREPUSH_EXIT=0).** The script exits 0 ONLY when
its `FAILURES` array is empty (every blocking check passed); a single failure — including
musl-release or the coverage floor — would exit 1. Per-group result:

| Group | Check | Result |
|-------|-------|--------|
| 1 Lint | rustfmt --check | PASS |
| 1 Lint | clippy -D warnings --all-features | PASS |
| 1 Lint | typos | PASS |
| 1 Lint | actionlint | PASS |
| 1 Lint | cargo-shear | PASS |
| 1 Lint | cargo-semver-checks | non-blocking (`snapdir-core` not on crates.io → `\|\| true`, mirrors ci.yaml) |
| 2 Supply chain | cargo-deny | PASS (only license-not-encountered + duplicate-version WARNINGS, non-fatal) |
| 2 Supply chain | cargo-audit | PASS |
| 3 Build+Test | build (workspace, --all-features, --locked) | PASS |
| 3 Build+Test | test (workspace, --all-features, --locked) | PASS (all suites 0 failed) |
| 4 Static musl | musl debug (docker amd64 rust:1.96-slim-bookworm) | PASS (`ok musl build (debug…)`) |
| 4 Static musl | musl release (docker amd64) | PASS (gated by exit 0) |
| 5 Doctests | cargo test --doc | PASS |
| 6 Coverage | cargo llvm-cov --fail-under-lines 75 | PASS |

Note: MSRV 1.85 toolchain not installed on host → that sub-step is skipped with a `warn`
(CI covers it); it does not gate.

### Coverage (authoritative, direct re-run with the script's flags)

    cargo llvm-cov --workspace --all-features --locked --fail-under-lines 75
    TOTAL  Regions 78.37%  Functions 75.46%  Lines 78.03%   → exit 0

**Lines = 78.03%** (the column `--fail-under-lines` keys on), comfortably above the
**75%** floor. Threshold was NOT lowered.

### sha-lock (frozen interface) — re-verified after `cargo fmt --all`

    shasum -a 256 -c .gatesmith/manifest-format.sha.lock
    crates/snapdir-core/src/manifest.rs: OK
    crates/snapdir-core/src/merkle.rs:   OK
    crates/snapdir-core/src/excludes.rs: OK

fmt did not touch any of the three frozen files; no revert was needed.

## Reuse check / Blockers

- **Locked files untouched / OK** — manifest.rs / merkle.rs / excludes.rs all `OK`
  before and after `cargo fmt --all`. All `walk.rs` fmt hunks are inside `mod tests`
  (>line 554); production walk code is byte-identical.
- **fmt clean** — `cargo fmt --all --check` exits 0.
- **Unused dep removed + build green** — `snapdir-core` gone from `snapdir-catalog`;
  `snapdir-core` moved to `[dev-dependencies]` in `benches`; `cargo build --workspace
  --locked` exits 0.
- **typos clean via config** — `typos` exits 0; no golden data or identifiers edited.
- **cargo-shear clean** — `cargo shear` exits 0 (no issues found).
- **clippy --all-features green** — `-D warnings` exit 0.
- **musl debug + release built** — both in the amd64 docker container; release gated by
  the suite's exit 0.
- **coverage ≥ 75%** — 78.03% lines.
- **cargo test green** — all workspace test suites 0 failed; doctests green.
- No logic changes; `ci.yaml` and `utils/ci/pre-push.sh` untouched. The
  `benches/Cargo.toml` dev-dep move was the one item beyond the brief's explicit list —
  same gate (cargo-shear), same mechanical class, in-scope.

Ready for PM verification: YES
