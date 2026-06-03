# Changelog

All notable changes to the snapdir Rust port are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] — Port complete

The Rust port is **complete** and the legacy Bash implementation has been
removed. With nothing left to differentially test against, the byte-format
contract is now guarded entirely in Rust, the dependency tree is modernized, and
the distribution story (static musl on `scratch`, release archives, ADRs) is
finalized.

### Added

- **Migration guide** (`docs/rust-port/migration.md`) and **manifest
  specification** (`docs/rust-port/manifest-spec.md`) documenting the frozen
  manifest format, the content-addressable storage layout, the directory merkle
  rule, and the snapshot-ID derivation.
- **Architecture Decision Records** (`docs/adr/`) capturing the significant port
  decisions (manifest-format freeze, snapshot-ID derivation, ring TLS provider,
  in-process cloud stores, redb catalog, scratch image, bundled CA roots,
  retiring the Bash implementation, dependency-cooldown policy, and more).
- **Rust golden-format contract** — `crates/snapdir-core/tests/compat_golden.rs`
  pins the exact manifest line bytes, directory merkle checksums, and snapshot
  IDs as golden constants, replacing the live comparison against the Bash version
  as the guarantor of byte-format stability. Any accidental change to the line
  format, ordering, checksum algorithm, sharded layout, or exclude sets fails the
  golden tests.
- **Local pre-push CI hook** (`utils/ci/pre-push.sh`, installed via
  `make install-hooks`) running the fast CI legs (~2–4 min) before every push;
  the slow musl + coverage legs run in CI and via `make ci-local`.
- **`scratch` Docker image** — a `FROM scratch` final stage shipping only the
  fully-static musl `snapdir` binary plus the bundled CA roots
  (`ca-certificates.crt`): zero runtime executables, no libc, no shell.

### Changed

- **Dependencies modernized.** TLS/crypto moved to **rustls 0.23** with the
  **ring** provider over **hyper 1.x**; the AWS SDK crates were bumped to their
  latest releases and the `google-cloud-storage` SDK was unpinned. All updates
  honor a **3-day minimum-release-age cooldown** (supply-chain hardening).
- **MSRV raised to 1.91.1**, driven by the AWS SDK crates.

### Removed

- **The legacy Bash implementation was removed.** Its role as the behavioral
  source of truth is now served by the Rust golden-format tests. The shipped
  binary remains fully in-process with no runtime dependency on external
  executables.

## [0.5.0] — Rust port

The Bash `snapdir` (v0.5.0) is ported to a single, statically-linkable,
**zero-runtime-dependency** Rust binary. The `snapdir` binary absorbs every
`snapdir-*` helper as a subcommand, while remaining **byte-for-byte
interoperable** with the Bash version: identical manifest lines, identical
snapshot IDs, and identical object/manifest keys and bucket layout, so Rust- and
Bash-written caches and remote buckets stay mutually readable.

### Added

- **Single `snapdir` binary** exposing all 14 subcommands (`manifest`, `id`,
  `stage`, `push`, `fetch`, `pull`, `checkout`, `verify`, `verify-cache`,
  `flush-cache`, `locations`, `ancestors`, `revisions`, `defaults`, plus
  `test`/`version`/`help`) via a clap v4 derive interface.
- **In-process BLAKE3** hashing via the `blake3` crate — no shelling out to
  `b3sum`. Includes keyed mode (`SNAPDIR_MANIFEST_CONTEXT` →
  `blake3::derive_key`) and the `--checksum-bin` matrix (`md5sum`/`sha256sum`)
  reproduced in-process via the `md-5`/`sha2` crates.
- **In-process filesystem walk** producing the frozen manifest format, with
  symlink follow/no-follow, `--absolute`, and the `%system%`/`%common%` exclude
  macros — verified byte-for-byte against the original snapdir's manifest output.
- **Native-SDK remote stores** — S3 (`aws-sdk-s3`), B2 (Backblaze's
  S3-compatible endpoint, a thin wrapper over the S3 store), and GCS
  (`google-cloud-storage`). No shelling out to `aws`, `b2`, or `gcloud`.
- **redb-backed internal catalog** replacing the SQLite catalog. The catalog is
  private and rebuildable — there is no on-disk catalog interop and no
  SQLite→redb importer; rebuild it from a store with `snapdir catalog rebuild`.
  Only the JSON-line *output* (`locations`/`ancestors`/`revisions`) stays
  byte-for-byte format-identical to the Bash tool.
- **External-store shim** retained for third-party `snapdir-*-store` binaries:
  the binary emits the store's shell command rather than embedding it. Built-in
  stores (`file`/`s3`/`b2`/`gs`) stay fully in-process.
- **`file://` FileStore** with the sharded `.objects`/`.manifests` layout,
  objects-before-manifest push (skip-if-present), and verified fetch
  (temp download → BLAKE3 verify → retry ≤5 → atomic rename).
- **Interop verification** proving byte-identical manifests and snapshot IDs
  Bash↔Rust across every checksum/keyed/no-follow mode, plus live cross-tool
  checks for S3 (MinIO) and GCS.
- **Performance**: in-process walk + BLAKE3 makes the Rust `manifest` command
  ~33.6× faster on many-small files and ~2.69× faster on few-large files than the
  Bash version, with byte-identical output.

### Changed

- **Catalog backend** moved from SQLite (shelling out to `sqlite3`) to the
  pure-Rust embedded `redb` key-value store. Catalog state is now internal and
  rebuildable via `snapdir catalog rebuild` rather than migrated.
- **Authentication** for remote stores is delegated entirely to each native
  SDK's own credential chain (AWS env/profiles/SSO/instance metadata;
  GCS `GOOGLE_APPLICATION_CREDENTIALS`/ADC/metadata) — no bespoke snapdir env
  vars.
- **TLS/crypto** uses the **ring** rustls provider (not aws-lc-rs) so the static
  musl build stays clean; `aws-lc-rs` is banned from the dependency tree.

### Fixed

- Documentation flag/command names corrected from the Bash docs' known bugs: the
  checkout flag is **`--linked`** (the old docs showed `--link`), and the
  store-side transaction check subcommand is **`ensure-no-errors`** (the old
  docs showed `verify-transactions`). The frozen Bash scripts already used the
  correct names; only the docs were wrong, and the Rust-port docs use the real
  names.
- Corrected the documented snapshot-ID derivation: the snapshot ID is the BLAKE3
  of the `#`-stripped manifest text (including the trailing newline), **not** the
  root directory checksum. (The earlier "root dir checksum = snapshot ID" wording
  was a documented bug; the core implementation and the frozen contract now state
  the real derivation. See `docs/rust-port/manifest-spec.md` §4.)

### Removed

- No runtime dependency on external binaries (`b3sum`, `sqlite3`, `aws`, `b2`,
  `gcloud`) in the shipped binary. External tools are used only by the test
  suite.

[Unreleased]: https://github.com/bermi/snapdir/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/bermi/snapdir/releases/tag/v1.0.0
[0.5.0]: https://github.com/bermi/snapdir/releases/tag/v0.5.0
