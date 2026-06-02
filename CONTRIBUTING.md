# Contributing to snapdir

snapdir is a Rust workspace producing a single dependency-free `snapdir`
binary for content-addressable directory snapshots. Contributions are welcome.

## Getting started

```bash
git clone https://github.com/bermi/snapdir
cd snapdir
cargo build --workspace
```

The toolchain is pinned in `rust-toolchain.toml` (currently 1.96.0); rustup
will install it automatically. The supported MSRV is 1.85.

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
