# Changelog

All notable changes to the snapdir Rust port are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.4.0] — 2026-06-09

### Added

- **Transient-failure retries with full-jitter exponential backoff.** Network
  store calls (`s3://`, `gs://`, `b2://`) now retry transient failures — HTTP
  `429`/`503`, S3 `SlowDown`, GCS `RESOURCE_EXHAUSTED`, request timeouts, and
  connection reset/closed — under full-jitter exponential backoff, while a
  non-transient error (e.g. `404` not-found) fails immediately. A server
  `Retry-After` header (or GCS backoff hint) is honored as a floor on the wait.
  Each SDK's built-in retries are disabled so snapdir's policy is the single
  authority. Defaults are **5 total attempts** (the first try plus up to four
  retries), **250 ms** base, doubling, capped at **30 s**; configurable via
  `--max-retries`/`SNAPDIR_MAX_RETRIES`, `--retry-base-ms`/`SNAPDIR_RETRY_BASE_MS`,
  and `--retry-max-ms`/`SNAPDIR_RETRY_MAX_MS`. The local `file://` store does no
  network retrying.
- **Per-second request-rate limiting (`--max-requests`/`SNAPDIR_MAX_REQUESTS`).**
  Complements the existing aggregate byte-throughput cap
  (`--limit-rate`/`SNAPDIR_LIMIT_RATE`) with a requests-per-second cap for the
  network stores. When unset, a conservative **per-backend default** applies,
  taken as the lower of each provider's published read/write limits:
  `s3://` 3500 req/s, `gs://` 1000 req/s, `b2://` 20 req/s + 25 MiB/s, and no
  caps for `file://`/local (sources: AWS S3, GCS, Backblaze B2). Precedence,
  highest to lowest: `--flag` > `SNAPDIR_*` env > per-backend default > global
  default.

### Fixed

- **Input-path normalization.** `snapdir push`/`manifest`/`id`/`stage` now treat
  `foo`, `./foo`, `foo/`, and `./foo/` identically — every form produces the same
  manifest and snapshot id. Previously a trailing slash or a `./` prefix could
  leak absolute paths or a malformed entry into the manifest.
- **`--store` and `sync --from` default to `$SNAPDIR_STORE`.** When the flag is
  omitted and the env var is set, snapdir uses it; an explicit flag still wins.
  `sync --to` stays explicit (a sync needs two distinct stores).
- **crates.io crate pages now render a README.** Each published crate
  (`snapdir-core`/`-catalog`/`-stores`/`-cli`) ships its own README, so the
  crates.io pages are no longer blank.

These additions pull in **no new dependencies** and leave the manifest
byte-format and content-addressed layout unchanged, so snapshots stay fully
interoperable with 1.x.

## [1.3.0]

### Added

- **Opt-in adaptive transfer tuning (`--adaptive[=FRACTION]`).** When passed,
  `push`/`fetch`/`pull`/`checkout`/`stage`/`sync` auto-tune transfer concurrency
  (and, for the network stores, the aggregate byte-rate) toward a polite
  **fraction of measured capacity (default 0.8)**: it probes in-band (TCP-style
  slow-start → AIMD with a latency-gradient guardrail), backs off fast under
  throttling/timeouts or when CPU/memory are tight so it doesn't overwhelm the
  host or co-tenants, and re-probes every ~15s to use newly-free capacity. A
  `--max-jobs` flag (and `SNAPDIR_ADAPTIVE`/`SNAPDIR_MAX_JOBS` env) bound it.
  **Off by default — default behavior is unchanged (full speed)**; `--jobs`/
  `--limit-rate` remain hard overrides. Works across the local `file://` store
  and S3/GCS/B2. Transfers remain byte-identical regardless of the tuner (it
  changes only scheduling/rate).
- **Clearer, steadier transfer progress.** The single-line progress display now
  unambiguously labels counts (`N/M files`) vs sizes (unit-suffixed bytes), uses
  fixed-width columns so the layout no longer reflows as digits change, and shows
  a smoothed, throttled ETA that settles instead of flickering. When adaptive is
  on it surfaces the live `(auto <fraction>)` target.

The manifest byte-format and content-addressed object/manifest layout are unchanged, so
snapshots remain fully interoperable with 1.x.

## [1.2.0]

### Added

- **`snapdir sync` — streaming store-to-store snapshot copy.** A 15th subcommand
  that copies ONE snapshot (manifest + raw content-addressed objects) directly from
  one store to another, streaming through memory with no local-filesystem staging.
  Backed by a new `StreamStore` trait and a `sync_snapshot` orchestrator; it reuses
  the concurrency and aggregate rate-limiting from 1.1.0 (manifest-last,
  skip-already-present). Works across the S3, GCS, and B2 stores and the local
  `file://` store.
- **Live transfer & hashing progress dashboard.** A single-line, self-updating
  stderr progress indicator (spinner/bar plus from→to bytes/s and objects/s,
  concurrency, and best-effort memory/CPU) for `push`/`fetch`/`pull`/`checkout`/
  `stage`/`sync` and the local walk. It is shown only on an interactive TTY
  (auto-disabled when piped); `--no-progress`, `--quiet/-q`, and `--color` control
  it, and it honors `NO_COLOR`/`TERM=dumb`. stdout stays the scriptable snapshot id.
- **Deterministic benchmark & regression-gate suite.** A synthetic-scenario
  generator (regular files and dirs, fixed perms/bytes, no rng/time) drives: a golden
  snapshot-id plus structural-invariants plus full local round-trip determinism gate
  (runs everywhere as integration testing), criterion wall-clock decision benches, and
  an iai-callgrind instruction-count perf gate (5% threshold; macOS via a pinned
  Docker image, enforced in Linux CI). Benches are compile-checked in CI and pre-push.

The manifest byte-format and content-addressed object/manifest layout are unchanged, so
snapshots remain fully interoperable with 1.1.x.

## [1.1.0]

### Added

- **Concurrent object transfers.** `push` and `pull`/`fetch` now transfer objects
  concurrently instead of one at a time — across S3, GCS, and B2 (network) and the
  local `file://` store. Concurrency defaults to the number of available CPUs (capped
  at 16) and is tunable with `--jobs/-j N` (`SNAPDIR_JOBS`); `--jobs 1` restores fully
  sequential transfers.
- **Aggregate bandwidth limiting.** `--limit-rate RATE` (`SNAPDIR_LIMIT_RATE`) caps the
  *total* network throughput across all in-flight transfers via a single token bucket,
  using wget-style suffixes (e.g. `512K`, `10M`, `1G`). It applies to the network stores;
  local `file://` copies are not rate-limited.
- **`--verbose` transfer reporting.** Under `--verbose`, the transfer commands print the
  effective settings to stderr, e.g. `transfers: 8 concurrent, limit 10M`.

### Fixed

- **`--dryrun` is now honored.** The global `--dryrun` flag was declared but never
  checked, so `push --dryrun` (and other mutating commands) still wrote to the store.
  `push` (incl. staged `--id`), `stage`, `fetch`, `checkout`, `pull`, and `flush-cache`
  now perform zero writes under `--dryrun`, and `verify-cache --purge` does not purge.
- **`pull` no longer re-downloads data that is already local.** `fetch_files` skips any
  destination file already present whose checksum matches the manifest (no copy, and no
  network GET), and `fetch`/`pull` skip the store entirely when the snapshot is already
  cached — so a repeated pull of the same snapshot performs no redundant transfers.
  Corrupted local files are detected and repaired.

### Changed

- **`--exclude` and `--paths` accept multiple patterns** — repeated (`--exclude a
  --exclude b`) and/or comma-delimited (`--exclude a,b`) — combined as a logical OR
  (a path matches if it matches any pattern). The `%system%` / `%common%` macros are
  expanded per pattern. A single value behaves exactly as before.

The manifest byte-format and content-addressed object/manifest layout are unchanged, so
snapshots remain fully interoperable with 1.0.x.

## [1.0.1]

### Fixed

- **`snapdir push --store … --id <id>` (no PATH)** now pushes the *staged*
  snapshot named by `--id`. It previously ignored `--id` and fell through to the
  current-working-directory default, silently snapshotting the CWD instead of the
  staged snapshot (which looked like a hang when the CWD was large). Pushing by id
  materializes the snapshot from the local cache and uploads that, mirroring
  `fetch` in reverse.

### Removed

- **The published Docker/GHCR container image** and its build pipeline
  (`packaging/Dockerfile`, the root `Dockerfile`, the `docker-publish.yml`
  workflow, and the `docker` release job) are removed — the image is no longer
  maintained. Install via `cargo install snapdir-cli` or the prebuilt release
  archives. The library crates and signed release archives are unaffected.

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
  IDs as golden constants, replacing the live differential comparison as the
  guarantor of byte-format stability.
- **`manifest-format.sha.lock` tripwire** over the format-defining source, so any
  accidental change to the line format, ordering, checksum algorithm, sharded
  layout, or exclude sets trips CI and demands an explicit, reviewed bump.
- **Local pre-push CI gate** (`utils/ci/pre-push.sh`, installed via
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
  source of truth is now served by the Rust golden-format tests and the
  `manifest-format.sha.lock` tripwire. The shipped binary remains fully
  in-process with no runtime dependency on external executables.

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
- **Differential interop harness** (`tests/interop/run.sh`) proving byte-identical
  manifests and snapshot IDs Bash↔Rust across every checksum/keyed/no-follow
  mode, plus live cross-tool harnesses for S3 (MinIO) and GCS.
- **Performance**: in-process walk + BLAKE3 makes the Rust `manifest` command
  ~33.6× faster on many-small files and ~2.69× faster on few-large files than the
  Bash oracle, with byte-identical output.

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
  `gcloud`) in the shipped binary. External tools are used only by the test/oracle
  harness.

[Unreleased]: https://github.com/snapdir/snapdir/compare/v1.4.0...HEAD
[1.4.0]: https://github.com/snapdir/snapdir/compare/v1.3.0...v1.4.0
[1.3.0]: https://github.com/snapdir/snapdir/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/snapdir/snapdir/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/snapdir/snapdir/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/snapdir/snapdir/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/snapdir/snapdir/releases/tag/v1.0.0
[0.5.0]: https://github.com/snapdir/snapdir/releases/tag/v0.5.0
