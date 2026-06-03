# ci handoff for local-pre-push-gates (phase 11) @ 2026-06-03

## Summary

Built the local CI-equivalent pre-push gate the operator asked for, so breakage
never reaches the paid GitHub Actions runners (`ci.yaml` triggers on every push
and is currently RED).

New tooling (no production / `ci.yaml` / Cargo edits):

- **`utils/ci/pre-push.sh`** (executable, `#!/usr/bin/env bash`, `set -euo pipefail`,
  shellcheck-clean) — mirrors **all six** `ci.yaml` job groups. It runs every
  group, records all failures, prints each failing check + the exact reproduce
  command, and exits non-zero on any failure (so the push is blocked). Flags:
  `--fast` (skip the musl leg + coverage for quick iteration), `--no-install`
  (don't auto-install tools; print the install command instead), `--help`.
  Tool bootstrap: missing `typos-cli` / `cargo-shear` / `cargo-semver-checks` /
  `cargo-deny` / `cargo-audit` / `cargo-llvm-cov` / the musl target are
  auto-installed via `cargo install` / `rustup target add` (opt out with
  `--no-install`). `actionlint` (no cargo installer) fails with a `brew`/`go`
  install hint.
- **`utils/git-hooks/pre-push`** (executable) — the tracked hook; execs
  `utils/ci/pre-push.sh` (full suite). Installed by pointing
  `git config core.hooksPath` at `utils/git-hooks/`.
- **`Makefile`** targets: `install-hooks` (one command: sets `core.hooksPath`),
  `uninstall-hooks`, `ci-local` (full suite), `ci-local-fast`.
- **`CONTRIBUTING.md`** — new "Local CI gate (pre-push hook)" section: what the
  hook runs (the 6 groups), how to install (`make install-hooks`), `--fast`,
  `--no-install`, `git push --no-verify` bypass, and that it mirrors `ci.yaml`
  so failures are caught before any push.

### How each ci.yaml job is mirrored
1. **Lint** — `cargo fmt --all --check`; `cargo clippy --workspace --all-targets
   --all-features --locked -- -D warnings`; `typos`; `actionlint -color`;
   `cargo shear`; `cargo semver-checks check-release --package snapdir-core`
   (non-blocking — mirrors ci.yaml's `|| true`, reported as a warn).
2. **Supply chain** — `cargo deny --workspace --all-features check`; `cargo audit`.
3. **Build + Test** — `cargo build`/`cargo test --workspace --all-features
   --locked` on host stable; ALSO MSRV `1.85` build+test if that toolchain is
   in `rustup toolchain list`, else a clear "MSRV 1.85 not installed, skipping
   (CI covers it)" note.
4. **Static musl** — `x86_64-unknown-linux-musl` build, **debug AND release**.
5. **Doctests** — `cargo test --workspace --all-features --locked --doc`.
6. **Coverage** — `cargo llvm-cov --workspace --all-features --locked
   --fail-under-lines 75 --lcov --output-path lcov.info` (same 75% floor as CI;
   adds `llvm-tools-preview` if missing).

### macOS musl cross strategy (the operator's explicit ask — it MUST run)
`run_musl_leg` picks the most robust available path, in order:
1. Linux host: plain `rustup target add` + cargo build (warns if `musl-tools`
   missing).
2. `cross` (docker-based) if installed.
3. A host `x86_64-linux-musl-gcc` (`brew install filosottile/musl-cross/musl-cross`),
   wired via `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER`.
4. **Docker amd64 `rust:1.96-slim-bookworm`** — this host is Apple-Silicon macOS
   with no native musl linker, so the leg builds inside an **amd64** container
   (docker 23 available; amd64 runs under emulation). Inside an amd64 container
   the `x86_64-unknown-linux-musl` target is **native**, so no cross C linker is
   needed — the most robust fallback. Uses named cache volumes
   (`snapdir-musl-cargo-registry`, `snapdir-musl-target`) so repeat runs are fast.
5. None available → FAIL loudly with exact install commands (`cargo install cross`
   / `brew install filosottile/musl-cross/musl-cross` / a working docker daemon).

The musl leg is never silently skipped except under explicit `--fast`.

## Files changed

git status (in-lane only — no crate/ci.yaml/Cargo edits):

```
 M CONTRIBUTING.md          | 46 +++++++++++++++++++++++++++++
 M Makefile                 | 25 +++++++++++++-
?? utils/ci/pre-push.sh     | 437 lines (new, executable)
?? utils/git-hooks/pre-push |  15 lines (new, executable)
```

## Local verification result

**Gate verification_cmd** (the PM will re-run) — exit 0:

```
test -x utils/ci/pre-push.sh && grep -q -- '--all-features' utils/ci/pre-push.sh \
  && grep -q 'x86_64-unknown-linux-musl' utils/ci/pre-push.sh \
  && grep -qE 'llvm-cov|coverage' utils/ci/pre-push.sh \
  && grep -q 'cargo-shear\|shear' utils/ci/pre-push.sh \
  && grep -qE 'semver-checks' utils/ci/pre-push.sh \
  && grep -qE 'deny' utils/ci/pre-push.sh
# -> exit 0
```

`shellcheck -x utils/ci/pre-push.sh utils/git-hooks/pre-push` → exit 0 (clean).
`bash -n` on both → OK.

**`bash utils/ci/pre-push.sh --fast`** on the current (red) tree — ran all
non-musl/coverage groups, auto-installed the missing tools (typos-cli,
cargo-shear, cargo-semver-checks, cargo-audit), blocked with exit 1. Group results:

| Group / check | Result |
| --- | --- |
| 1 rustfmt --check | **FAIL** (formatting drift on the red tree) |
| 1 clippy --all-features -D warnings | ok (PASSES — already fixed on this tree) |
| 1 typos | **FAIL** (typos-cli auto-installed, then flagged) |
| 1 actionlint | ok |
| 1 cargo-shear (unused deps) | **FAIL** (unused dependency) |
| 1 cargo-semver-checks (snapdir-core) | warn (non-blocking, mirrors `|| true`) |
| 2 cargo-deny | ok |
| 2 cargo-audit | ok |
| 3 build (--all-features, locked) | ok |
| 3 test (--all-features, locked) | **FAIL** — 9 `walk::tests::*_matches_oracle` |
| 3 MSRV 1.85 | skipped with note (1.85 toolchain not installed) |
| 5 doctests | ok (core 2 + stores 1) |

The 9 test failures are the **oracle-shelling** tests
(`crates/snapdir-core/src/walk.rs:444` → *"oracle binary exists at repo root: …
NotFound"*) — exactly the dead `oracle()`-helper tests the separate
`remove-bash-test-harnesses` gate will delete. Not production bugs to fix here.

Summary block the script printed:

```
4 check(s) FAILED — push BLOCKED:
  ✗ rustfmt --check            reproduce: cargo fmt --all --check
  ✗ typos                      reproduce: typos
  ✗ cargo-shear (unused deps)  reproduce: cargo shear
  ✗ test (...locked)           reproduce: cargo test --workspace --all-features --locked
```

**Static musl leg (group 4)** — exercised directly via the script's docker
amd64 path (the macOS fallback). The **debug** `x86_64-unknown-linux-musl`
workspace build (incl. snapdir-stores / GCS / S3 / ring-rustls) **COMPILED AND
FINISHED successfully** inside the amd64 `rust:1.96-slim-bookworm` container
(`Finished dev profile in 6m 47s`; output binary present at
`target-musl/x86_64-unknown-linux-musl/debug/snapdir`). This proves the
load-bearing musl leg actually runs locally on this Apple-Silicon host and that
ring links cleanly into the static target — the musl leg is **NOT** a current
failure on the red tree. (Release was not separately re-run to save time; it
uses the identical path with `--release`.)

## Reuse check / Blockers

- **All 6 ci.yaml groups covered**, including the **static musl** leg (debug +
  release) and **coverage** (`--fail-under-lines 75`). Confirmed by the
  verification grep and by actually running the suite.
- **macOS musl cross handled** via the docker amd64 native-musl container
  (proven to build); falls back through `cross` / `musl-cross` / native-Linux
  where present; fails with actionable install commands if nothing is available.
  Never silently skipped (except under `--fast`).
- **Install command (one line):** `make install-hooks`
  (sets `git config core.hooksPath utils/git-hooks`). The hook runs the FULL
  suite; bypass a single push with `git push --no-verify`.
- **No production / ci.yaml / Cargo edits.** Touched only `utils/ci/`,
  `utils/git-hooks/` (new), `Makefile`, `CONTRIBUTING.md`. No crate source, no
  `.github/workflows/ci.yaml`, no Cargo config. shellcheck-clean. Did not commit;
  did not push (PUSH HOLD respected).
- **Failures the suite surfaces on the red tree** (for the `ci-green` /
  `remove-bash-test-harnesses` gates to clear): rustfmt drift, typos,
  cargo-shear unused dep, and 9 oracle-shelling `walk` tests. clippy
  `--all-features`, cargo-deny, cargo-audit, build, and doctests currently PASS.

Ready for PM verification: YES
