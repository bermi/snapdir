# Changelog

All notable changes to the snapdir Rust port are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

## [1.8.0] - 2026-06-16

This release is a CLI usability pass: stricter argument handling, a more honest
`defaults` report, a clearer progress indicator, working `id` stdin, and a batch
of store/sync/recovery/error-message fixes. The manifest byte-format,
content-addressed layout, and snapshot ids are unchanged.

### BREAKING

- **Global flags must now follow their subcommand.** Write
  `snapdir push --store X` rather than `snapdir --store X push`. Global flags
  placed before the subcommand are no longer accepted.
- **Each command accepts only the flags that apply to it.** Passing a flag a
  command does not use now fails with a clear error (e.g. `the argument
  '--limit-rate' cannot be used with 'manifest'`) instead of being silently
  ignored. Per-command `--help` now lists only that command's flags. Scripts that
  relied on snapdir quietly tolerating inapplicable flags will now get an error.

### Added

- **`snapdir id` now reads a manifest from stdin.** With no path argument,
  `snapdir id` reads a manifest piped on stdin, so `snapdir manifest <dir> |
  snapdir id` reproduces `snapdir id <dir>`. Previously it silently hashed the
  current directory regardless of any piped input. With no path and nothing
  piped, it now errors instead of silently scanning the current directory.

### Changed

- **`snapdir defaults` now reports every effective setting.** It prints each
  resolved value together with its source — `flag`, `env`, or `default` — for
  cache-dir, store, jobs, walk-jobs, limit-rate, retries, fsync, clonefile, and
  the rest, and it reflects override flags such as `--cache-dir`. Previously it
  printed only a handful of legacy environment lines.
- **Live progress now shows discovery and a real percentage.** For `id`,
  `manifest`, `stage`, and `push`, the progress indicator surfaces a visible
  discovery phase and a true percentage computed over the file count, instead of
  sitting at 0% against a byte-count denominator. Snapshot ids are unchanged.
- **`sync` now reports unique objects copied.** The summary counts the number of
  distinct objects actually copied rather than the file-reference count, and it
  no longer reports skipped objects when the destination starts out empty.

### Removed

- **Removed the `--debug` flag** (it had no effect) and the **`--paths` flag**
  (it silently filtered nothing).

### Fixed

- **Invalid flag values are now rejected.** `--color` and `--limit-rate` reject
  invalid values instead of accepting them silently, and an invalid store URI
  now reports the valid form (`file://<path>`, …).
- **`manifest --id` is no longer silently ignored.**
- **`diff`/`sync` against a nonexistent store now error clearly** instead of
  treating the missing store as empty, which previously produced a fabricated
  full diff.
- **`fetch`/`pull` restore objects missing from the local cache.** When a cached
  snapshot's objects were missing locally, `fetch`/`pull` previously reported the
  snapshot as cached and left the cache broken; they now re-fetch the missing
  objects. `verify-cache` now detects and reports missing objects (not only
  corrupt ones), naming the affected file path.
- **Clearer error messages.** A missing store now names `--store` /
  `SNAPDIR_STORE`; fetching a split snapshot without `--objects-store` /
  `--from-objects` now hints at the objects-store; and `verify --help` no longer
  claims it checks a "staged" snapshot — it verifies a snapshot in a store.

## [1.7.0] - 2026-06-14

### Added

- **`--walk-jobs <N>` / `$SNAPDIR_WALK_JOBS` — parallel, memory-mapped directory
  walk and file hashing.** Snapshotting a tree now hashes files across a bounded
  rayon pool and uses blake3's memory-mapped path for large files, so `id`,
  `manifest`, `stage`, and `push` no longer hash every file single-threaded —
  multiple× faster on large trees. The new global `--walk-jobs <N>` flag (and
  `$SNAPDIR_WALK_JOBS` env) sizes the walk pool; `0`/auto picks the number of
  CPUs (capped). This is **distinct from `--jobs` / `$SNAPDIR_JOBS`** (which
  controls transfer concurrency). Purely a performance win: **snapshot ids are
  unchanged (byte-identical)** with the feature on or off and across every
  `--walk-jobs` value — additive, default behavior preserved, the frozen
  manifest format untouched.
- **Cross-platform copy-on-write object clones — macOS (APFS `clonefile`) and
  Linux (`FICLONE` reflink).** When the source file and the snapdir store share
  a copy-on-write-capable filesystem, object copies during `stage`, `push`, and
  `checkout`/`fetch` now make a CoW clone instead of byte-copying: on macOS via
  `clonefile(2)` on the same APFS volume, and on Linux via the `FICLONE` ioctl
  reflink on Btrfs, XFS (`reflink=1`), OpenZFS 2.2+, OCFS2, and bcachefs. A
  large object is materialized for ~zero additional physical bytes (the clone
  shares extents) and without rewriting its data. This is additive and falls
  back gracefully to a plain `fs::copy` everywhere a reflink/clone cannot apply
  — non-CoW filesystems (ext4, F2FS, tmpfs), across filesystem boundaries, and
  on unsupported platforms. Set `SNAPDIR_CLONEFILE=0` to disable the fast-path
  entirely. Object bytes and snapshot ids are unchanged (byte-identical) with
  the fast-path on or off.
- **Clone fast-path now skips the redundant post-copy re-hash — a real
  `stage`/`checkout` speedup on both platforms.** Previously, even when an
  object was cloned copy-on-write, `persist()` re-read and re-hashed the result,
  so the clone saved disk space but not wall-clock time (the copy was never the
  bottleneck; the second full read was). The clone path now elides that
  redundant re-hash, turning the copy-on-write fast-path into a genuine speedup
  — multiple× faster `stage` and meaningfully faster `checkout` on large trees
  wherever a CoW clone fires, on macOS (APFS `clonefile`) or Linux (`FICLONE`
  reflink) alike. Correctness is preserved on two layers: **`stage`
  uses stat-validated trust** — the walk records the source file's stat and
  `persist` re-stats it at clone time, skipping the re-hash only if the source
  is unchanged since the walk (a changed source falls back to a full re-hash, so
  a mid-stage mutation is caught at write time); **`checkout` still verifies the
  source object once** (so on-disk object corruption is still detected) and only
  skips the redundant destination re-hash. Set **`SNAPDIR_VERIFY_COPIES=1`** to
  force the strict write-time re-hash even on the clone path. Object bytes and
  snapshot ids remain byte-identical, and the read-time BLAKE3 verification in
  `get_object`/`fetch` remains the integrity backstop regardless of this
  setting.
- **`--objects-store` / `$SNAPDIR_OBJECTS_STORE` — shared object pool, separate
  manifest locations.** This global flag routes content objects to one shared
  pool's `.objects/` while manifests go to `--store`'s `.manifests/`, so a
  scheduled inventory can write a fresh manifest path per run (by date / host /
  env) against a single deduplicated object pool. Re-pushing to the same pool
  only costs the changed bytes — unchanged content-addressed objects are
  skipped. Both halves resolve in-process; an external `custom://` store is
  rejected on either side. Unset leaves behavior byte-for-byte unchanged; the
  catalog records the `--store` (manifest-side) URI.
- **`snapdir sync --from-objects/--to-objects` — split object pools for
  bucket-to-bucket sync.** Each side names its own explicit object pool, so the
  source and destination can be different buckets; the streaming sync engine is
  unchanged, and objects already present in the destination pool are skipped
  (cross-pool dedup). These per-side flags are distinct from the global
  `--objects-store`; a side that omits its flag is a plain colocated store.
- **`snapdir diff` — file-level diff across manifest locations, reading
  manifests only.** Compares two sides, each a union of one-or-more manifest
  refs (`--from`/`--to`, both repeatable), classifying every path as `A` (added),
  `D` (deleted), or `M` (modified). It reads manifests only — it never downloads
  an object — so it stays cheap over large or unreachable object pools, which
  makes it well suited to comparing scheduled inventories. With the global
  `--id` a side pins to a single manifest. Flags: `--all` (also emit unchanged
  `=` paths), `--json` (a `{status, path}` array instead of porcelain
  `X\t./path` lines), `--exit-code` (git `diff --exit-code` semantics: exit `1`
  on any difference), and `--on-conflict <error|last-wins>` (intra-side
  same-path/differing-content collision policy; defaults to `error`).
- **SNAPPACK 1Z — auto-negotiated zstd transport for the `ssh://` accelerated
  pack stream.** The whole post-magic pack body is sent as a single zstd frame
  of the unchanged SNAPPACK 1 record grammar; the receiver sniffs the magic
  (`SNAPPACK 1Z\n`) and accepts both v1 and 1Z forever. Compression is additive
  (the wire version stays `1`; a new `snappack-zstd` capability token gates it),
  so it engages only when both ends advertise support — a mixed-version pair
  falls back to v1 with the v1 acceleration still taken. The level defaults to
  zstd level 3 and is tunable via `SNAPDIR_SSH_ZSTD_LEVEL` (`1`–`19`, clamped).
  Every decompressed byte is still BLAKE3-verified and the existing
  header/manifest bounds apply to the decompressed stream, so a decompression
  bomb costs CPU only. With SNAPPACK now compressing above the transport,
  prefer `Compression=no` on the SSH client (WAN / HPN-SSH) to avoid
  double-compressing.
- **`SNAPDIR_FSYNC` crash-durability knob on `receive-pack`.** Defaults to
  `batch`: all received objects are fsynced before the manifest is committed
  last, so a manifest that survives a crash is backed by durable objects (the
  manifest-last invariant holds across the crash boundary). `off` skips the
  barrier and relies on the OS to flush; any other value is a hard error. On a
  journaling filesystem this matches the crash-consistency guarantee git
  provides, and claims no more than that. Measured cost: the `batch` default
  adds ~20% on a small-files receive (v1 +19.5% / zstd +29.9% on a 5,000 ×
  4 KiB push, Linux CI) — a fixed per-object fsync cost on the receive-pack
  path only, accepted as the crash-safe default with `SNAPDIR_FSYNC=off` as
  the opt-out.

## [1.6.0] — 2026-06-11

### Added

- **`snapdir` crate: `cargo install snapdir` installs the CLI.** The flagship
  `snapdir` crate name on crates.io now ships the `snapdir` binary (a thin
  shim over the `snapdir-cli` implementation library), so the install command
  matches the binary name.

### Changed

- **`snapdir-cli` is now the implementation library.** The `snapdir` binary
  moved to the new `snapdir` crate; `snapdir-cli` keeps publishing and exposes
  `snapdir_cli::run()` — the binary entrypoint, not a semver-stable
  general-purpose API. Versions ≤ 1.5.0 of `snapdir-cli` are unaffected and
  still install the binary directly.

## [1.5.0] — 2026-06-10

### Added

- **`ssh://` and `sftp://` stores over the system OpenSSH client.** A new
  `snapdir-ssh-store` crate ships two external-store binaries —
  `snapdir-ssh-store` (`ssh://`, needs a remote POSIX shell) and
  `snapdir-sftp-store` (`sftp://`, pure SFTP; works against restricted
  `ForceCommand internal-sftp` chroot accounts) — with no SSH
  reimplementation and zero new crypto dependencies. Store URLs take
  `ssh://[user@]host[:port]/abs/base`; each scheme reads its own
  `SNAPDIR_{SSH,SFTP}_STORE_*` env family (`IDENTITY_FILE`, `KNOWN_HOSTS`,
  `PORT`, `CONNECT_TIMEOUT`, `JOBS`, `CONTROL_PERSIST`, `UMASK`,
  `EXTRA_OPTS`). Every invocation multiplexes over one `ControlMaster`
  connection and starts with an un-weakenable modern-only security floor
  (pinned kex/AEAD-cipher/host-key lists, `StrictHostKeyChecking=yes`,
  `BatchMode=yes`; `EXTRA_OPTS` are appended last and structurally cannot
  weaken it); OpenSSH ≥ 8.5 is required locally, fail-closed. `snapdir sync`
  does not support these stores.
- **Automatic `ssh://` acceleration via SNAPPACK.** When the remote host has
  a wire-compatible `snapdir` on its `PATH`, pushes and fetches negotiate at
  runtime (exact `wire=1` integer match, never semver) and switch to a
  self-verifying pack stream: the object list is diffed remotely and only
  missing objects ride one `send-pack | receive-pack` pipe, with the manifest
  as the last record, committed only after the verified `end` trailer —
  manifest-last preserved end-to-end, byte-identical to the plain path.
  Graceful fallback when the remote lacks the plumbing;
  `SNAPDIR_SSH_NO_ACCEL=1`, `SNAPDIR_SSH_FORCE_ACCEL=1`, and
  `SNAPDIR_SSH_PULL_SENDALL=1` control it. Spec:
  `docs/rust-port/ssh-wire-protocol.md`.
- **Hidden wire plumbing in the CLI.** `snapdir version --capabilities`
  prints the negotiation line (`snapdir <semver> wire=<u32> caps=<csv>`), and
  three hidden subcommands — `objects-needed`, `send-pack`, `receive-pack` —
  implement the pack protocol over any built-in store with fail-closed input
  validation and incremental BLAKE3 verification. The documented CLI surface
  and plain `snapdir version` output are unchanged.
- **`StreamStore::objects_needed`.** A defaulted trait method answering which
  of the offered checksums a store does not hold (order-preserving,
  fail-closed validation), available across the `file://`, `s3://`, `gs://`,
  and `b2://` stores.

### Fixed

- **External-store CLI wiring.** `snapdir push`/`fetch --store` against an
  external `snapdir-<scheme>-store` binary passed directory *trees* where the
  emit-command contract expects *sharded store roots*, breaking every
  external store end-to-end. Push now stages the snapshot into the local
  cache and pushes from the cache root, and fetch lands objects directly in
  the cache root, committing the manifest last. Affects all
  `snapdir-*-store` binaries; the built-in stores were never affected.

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

[Unreleased]: https://github.com/snapdir/snapdir/compare/v1.8.0...HEAD
[1.8.0]: https://github.com/snapdir/snapdir/compare/v1.7.0...v1.8.0
[1.7.0]: https://github.com/snapdir/snapdir/compare/v1.6.0...v1.7.0
[1.6.0]: https://github.com/snapdir/snapdir/compare/v1.5.0...v1.6.0
[1.5.0]: https://github.com/snapdir/snapdir/compare/v1.4.0...v1.5.0
[1.4.0]: https://github.com/snapdir/snapdir/compare/v1.3.0...v1.4.0
[1.3.0]: https://github.com/snapdir/snapdir/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/snapdir/snapdir/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/snapdir/snapdir/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/snapdir/snapdir/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/snapdir/snapdir/releases/tag/v1.0.0
[0.5.0]: https://github.com/snapdir/snapdir/releases/tag/v0.5.0
