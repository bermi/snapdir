# ci-green (phase 11) — fix the 3 CI failures

## Summary

Fixed the three CI failures from ci.yaml run 26889287870 (sha 6932d7f). The local
pre-push suite passed because all three are environment/CI-only issues:

1. **macOS Test matrix** — the `snapdir-mock-store` test shim used bash-4 associative
   arrays (`declare -A`), which fail on CI macos-latest's `/usr/bin/env bash` (bash 3.2).
2. **Static musl (debug+release)** — `rust-toolchain.toml` pinned channel 1.96.0, which
   overrode the active toolchain and stripped the musl target that
   `dtolnay/rust-toolchain@stable` added; it also silently forced the whole
   `[1.85, stable, beta]` matrix to run 1.96.0.
3. **Lint** — `taiki-e/install-action` failed to install an unpinned `actionlint`.

A real **MSRV correction** was required (see below): the workspace MSRV is **1.91.1**,
not 1.85.

## Files changed

```
 .github/workflows/ci.yaml                      |  4 ++--
 CONTRIBUTING.md                                |  5 +++--
 Cargo.toml                                     |  2 +-
 Dockerfile                                     |  2 +-
 crates/snapdir-stores/tests/snapdir-mock-store | 31 ++++++++++++++++++++------
 utils/ci/pre-push.sh                           |  6 +++---
 rust-toolchain.toml                            |  4 ---- (deleted)
```

- `crates/snapdir-stores/tests/snapdir-mock-store` — rewritten bash-3.2 compatible.
- `rust-toolchain.toml` — **removed** (`git rm`).
- `.github/workflows/ci.yaml` — actionlint now installed via a **direct pinned GitHub
  release download** (rhysd/actionlint **v1.7.12** tarball → `/usr/local/bin`), replacing
  the `taiki-e/install-action` step entirely (see FAILURE 3); test-matrix MSRV `"1.85"` →
  `"1.91.1"`.
- `Cargo.toml` — workspace `rust-version` `1.85` → `1.91.1`.
- `Dockerfile` / `CONTRIBUTING.md` — stale comments/text that referenced the now-removed
  `rust-toolchain.toml` updated (no behavioral change; Dockerfile still uses the
  `rust:1.96-slim-bookworm` base image directly).
- `utils/ci/pre-push.sh` — local pre-push gate MSRV synced to CI: `MSRV="1.85"` →
  `MSRV="1.91.1"` (line 51) plus the two stale "1.85" comment references (lines 17, 350)
  → 1.91.1, so the local gate faithfully mirrors ci.yaml's matrix lowest entry.

## Local verification result

- **Shim test:** `cargo test -p snapdir-stores --test shim_external_store` → **5 passed**.
  Also ran the rewritten script directly under the system **bash 3.2.57** (`/bin/bash`)
  for all four contract paths (version / get-manifest-command / get-push-command /
  get-fetch-files-command) — all emit identical scripts and exit 0. `bash -n` clean; no
  remaining bash-4 features (`declare -A`, `${,,}`/`${^^}`, `mapfile`/`readarray`, `&>>`).
- **`cargo +1.85 check --workspace --all-features --locked`** → **FAILS** (MSRV
  correction needed — evidence below).
- **`cargo +1.91.1 check --workspace --all-features --locked`** → **PASSES** (clean).
- **`cargo build --workspace --all-features --locked`** (default toolchain 1.96) → OK.
- **`actionlint -color`** on all workflows (local actionlint 1.7.12), incl. the rewritten
  ci.yaml install step + edited s3-store.yml/b2-store.yml → **exit 0**, nothing flagged.
- **ci.yaml YAML validity** confirmed via `ruby -ryaml` (VALID) and the embedded
  install-step shell block passes `bash -n`.
- **`shasum -a 256 -c .gatesmith/manifest-format.sha.lock`** → all 3 frozen files
  (`manifest.rs`, `merkle.rs`, `excludes.rs`) **OK** (untouched; no fmt run).
- **`bash -n utils/ci/pre-push.sh`** → SYNTAX OK; **`shellcheck utils/ci/pre-push.sh`** →
  CLEAN (after the MSRV sync). No "1.85" string remains in the file.

## Reuse check / Blockers

### FAILURE 1 — bash-4 in mock store
Replaced `declare -A opt=()` associative-array option parsing with a bash-3.2-safe
approach: a `set_opt <name> <value>` helper that assigns plain `opt_<name>` scalars
(hyphens → underscores, e.g. `--staging-dir` → `opt_staging_dir`). Reads updated to
`${opt_store}`, `${opt_id}`, `${opt_staging_dir}`, `${opt_cache_dir}`. The emit-command
contract (get-manifest-command / get-fetch-files-command / get-push-command), the
`mock:///abs/dir` mapping, and objects-before-manifest ordering are byte-for-byte
identical. Kept `#!/usr/bin/env bash`.

### FAILURE 2 — rust-toolchain.toml + MSRV
**`rust-toolchain.toml` was REMOVED.** Confirmed it is safe: the root `Dockerfile` pins
Rust via its base image `rust:1.96-slim-bookworm` (FROM line) and only *mentioned* the
toml in a comment — it never COPYs/depends on it for toolchain selection. The heavy
`snapdir-msrv-check` image was NOT rebuilt (not required).

**MSRV decision — real correction to 1.91.1.** With the toml gone, the matrix's lowest
entry genuinely runs that toolchain, so I verified it:
- `cargo +1.85 check` **fails**. The binding constraint is the **AWS SDK crates** (used by
  the S3 store): `aws-config`, `aws-sdk-s3`, `aws-runtime`, `aws-smithy-*`, `aws-types`
  (23 crates) all `require rustc 1.91.1`. (Lesser floors also present: redb 1.89,
  serde_with/time/tonic 1.88, google-cloud-* 1.87, icu_* 1.86 — but 1.91.1 is the max.)
- `cargo +1.91.1 check --workspace --all-features --locked` **passes clean**, confirming
  1.91.1 is the true minimum.
- Therefore set workspace `Cargo.toml` `rust-version = "1.91.1"` and ci.yaml matrix lowest
  entry `"1.85"` → `"1.91.1"`; kept `stable` + `beta`.

With the toml removed, the musl job's `dtolnay@stable + targets:
x86_64-unknown-linux-musl` now keeps the musl target on the active stable toolchain, so
the "can't find crate for core" failure is resolved. (Local `cargo` now uses the dev's
default toolchain — expected.)

### FAILURE 3 — actionlint install
`taiki-e/install-action` cannot install actionlint at all in CI right now: unpinned →
"actionlint is not found"; `actionlint@1.7.7` → "install-action does not support
actionlint@1.7.7" → cargo-binstall fallback → not found. So **install-action was bypassed
entirely**. The install step is now a direct, pinned GitHub-release download:

```yaml
      - name: Install actionlint
        run: |
          VER=1.7.12
          curl -sSL "https://github.com/rhysd/actionlint/releases/download/v${VER}/actionlint_${VER}_linux_amd64.tar.gz" | tar -xz actionlint
          sudo mv actionlint /usr/local/bin/actionlint
          actionlint --version
```

The subsequent `- name: actionlint` / `run: actionlint -color` step is unchanged. The
other lint tools (typos/clippy/shear/semver) were already passing and were not touched.
Ran `actionlint -color` locally over ALL workflows (ci.yaml, s3-store.yml, b2-store.yml,
release.yml, supply-chain.yml) → exit 0; nothing flagged in the edited files. actionlint
runs shellcheck on `run:` blocks internally and flagged nothing in the new install step.

**actionlint version — 1.7.12 (was 1.7.7).** 1.7.7's RUN produced a FALSE POSITIVE:
`release.yml:119: label "macos-15-intel" is unknown` — that runner label is real and
working (the release dry-run built `x86_64-apple-darwin` on it); 1.7.7's static
known-labels list simply predates it. Bumped the download to `VER=1.7.12` (matches the
local actionlint that passes clean and knows `macos-15-intel`). **release.yml's
`macos-15-intel` label was NOT changed** — it is a real working runner. `pre-push.sh` does
not pin an actionlint version (it runs whatever `actionlint` is on `PATH`), so it was left
as-is for local/CI parity.

### sha-locks
Intact. The frozen `crates/snapdir-core/src/{manifest,merkle,excludes}.rs` were NOT
touched; sha-lock verifies OK.

Ready for PM verification: YES
