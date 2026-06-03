# Contributing to snapdir

snapdir is a Rust workspace producing a single dependency-free `snapdir`
binary for content-addressable directory snapshots. Contributions are welcome.

## Getting started

```bash
git clone https://github.com/bermi/snapdir
cd snapdir
cargo build --workspace
```

Any toolchain at or above the MSRV works. The supported MSRV is 1.91.1
(driven by the AWS SDK crates used by the S3 store); CI tests MSRV, stable,
and beta.

## Workspace layout

| Path                     | Purpose                                                   |
| ------------------------ | --------------------------------------------------------- |
| `crates/snapdir-core`    | Manifest format, FS walk, BLAKE3/MD5/SHA-256 hashing, cache, `Store` trait |
| `crates/snapdir-catalog` | redb-backed catalog (locations / revisions / ancestors)   |
| `crates/snapdir-stores`  | `file://`, `s3://`, `b2://`, `gs://` store implementations |
| `crates/snapdir-cli`     | The `snapdir` binary (clap), wiring the crates together    |
| `benches/`               | Criterion micro-benchmarks (`snapdir-benches`)             |
| `tests/`                 | Integration + interop harnesses                            |

## Before you open a PR

Run the same checks CI enforces:

```bash
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo fmt --all` applies formatting in place. All three must be clean.

## Local CI gate (pre-push hook)

`ci.yaml` runs on every push and burns paid GitHub Actions minutes — including
on red commits. To catch failures *before* they reach CI, `utils/ci/pre-push.sh`
runs the full CI-equivalent suite locally and **blocks the push on any failure**.

Install it once as a git `pre-push` hook:

```bash
make install-hooks    # points git config core.hooksPath at utils/git-hooks/
```

From then on, `git push` runs the full suite first and aborts the push if
anything fails. Remove it with `make uninstall-hooks`; bypass a single push
(use sparingly) with `git push --no-verify`.

Run it manually any time:

```bash
make ci-local         # full suite (lint, supply-chain, build/test, musl, doctests, coverage)
make ci-local-fast    # same, minus the musl leg + coverage — for quick iteration
```

The script mirrors **every** `ci.yaml` job, so a green local run means CI is
green too:

1. **Lint** — `cargo fmt --check`, `clippy --all-features -D warnings`, `typos`,
   `actionlint`, `cargo shear`, and `cargo semver-checks` (non-blocking, as in CI).
2. **Supply chain** — `cargo deny check`, `cargo audit`.
3. **Build + test** — `cargo build`/`cargo test --workspace --all-features --locked`
   on host stable, plus the MSRV `1.85` toolchain if it is installed
   (`rustup toolchain install 1.85`), else skipped with a note (CI covers it).
4. **Static musl** — `x86_64-unknown-linux-musl` build, debug **and** release.
   On Linux this is a plain target add + `musl-tools`. On macOS (no native musl
   linker) the script uses the most robust path available: `cross` if installed,
   else a `musl-cross` linker (`brew install filosottile/musl-cross/musl-cross`),
   else it builds the leg inside an amd64 `rust:slim` docker container (native
   musl, no cross C linker needed). The musl leg always runs; `--fast` skips it.
5. **Doctests** — `cargo test --doc`.
6. **Coverage** — `cargo llvm-cov --fail-under-lines 75` (the same floor as CI).

Missing tools (`typos-cli`, `cargo-shear`, `cargo-semver-checks`, `cargo-deny`,
`cargo-audit`, `cargo-llvm-cov`, the musl target) are auto-installed via
`cargo install` / `rustup target add`. Pass `--no-install` to opt out and have
the script print the exact install command instead.

## The frozen Bash oracle

The Bash scripts at the repo root — `snapdir`, `snapdir-manifest`,
`snapdir-*-store`, `snapdir-sqlite3-catalog`, `snapdir-test` — and everything
under `utils/qa-fixtures/` are the **frozen interop oracle**. The differential
tests run the Rust binary against them to prove byte-for-byte compatibility of
manifests, snapshot IDs, and on-disk store layout.

Do not edit these scripts or the fixtures: if the Rust port disagrees with the
oracle, the oracle is the source of truth. Changing manifest line format,
ordering, the checksum algorithm, sharding, or exclude sets requires
maintainer approval. New behavior belongs in `crates/`, validated against the
oracle.

## Zero runtime dependencies

The shipped binary does everything in-process. Never shell out to `b3sum`,
`sqlite3`, `aws`, `b2`, or `gcloud` from `crates/` — external binaries are
allowed only in the test/oracle harness.

## Commits and PRs

Use [Conventional Commits](https://www.conventionalcommits.org)
(`feat:`, `fix:`, `docs:`, `test:`, …). Keep PRs focused, describe the change
and how you verified it, and make sure the checks above pass before requesting
review.
