# snapdir

Content-addressable directory snapshots: hash a directory into a deterministic ID, push it to object storage, and pull it back byte-for-byte verified anywhere.

`snapdir` snapshots a directory by content. Every snapshot is a BLAKE3 manifest — one line per file/dir as `TYPE PERMISSIONS CHECKSUM SIZE PATH`, sorted by path, with directory checksums computed as a merkle hash of their children. The **snapshot ID** is the BLAKE3 of the manifest text, so identical content produces an identical ID on any machine. Objects and manifests are stored at content-addressed sharded keys, so identical files and snapshots are stored once and interoperate across stores.

A single static binary with **zero runtime dependencies** — all hashing and storage is in-process; nothing to install alongside it.

📖 **Documentation:** **[snapdir.org](https://snapdir.org)** — install guide, command reference, concepts, and use cases.

## Install

```sh
# crates.io — installs the `snapdir` binary
cargo install snapdir
```

Other ways:

```sh
# Prebuilt release archives (static musl + per-platform builds)
#   https://github.com/snapdir/snapdir/releases

# As a Rust library
cargo add snapdir-core
```

> **Migrating from `cargo install snapdir-cli`?** That crate is now the
> implementation library behind the CLI. Its versions ≤ 1.5.0 still install
> the binary; from 1.6.0 the binary lives in the `snapdir` crate.

## Quick start — 60 seconds, no setup

No cloud account needed: snapshot a directory and round-trip it through a **local** store.

```sh
# A directory to snapshot
mkdir -p demo && echo hello > demo/a.txt

# Its content has a deterministic ID — same content, same ID, on any machine
snapdir id ./demo

# Push it to a store. Here the store is just a local directory.
# `push` prints the snapshot ID.
id=$(snapdir push --store "file://$PWD/store" ./demo)

# Pull it back somewhere else; every object is re-hashed and verified on fetch
snapdir pull --store "file://$PWD/store" --id "$id" ./restored

snapdir id ./restored   # prints the same $id — byte-for-byte identical
```

Swap `file://$PWD/store` for `s3://…`, `gs://…`, or `b2://…` and the exact same commands push to the cloud.

## Sync a directory across local, S3 & GCS

snapdir keeps a directory in sync across backends **by content**: the same snapshot lands under the same content-addressed keys everywhere, unchanged data is never re-uploaded, and every restore is verified. (It's snapshot-based, not a live-sync daemon — re-run `push` to publish a new version.)

```sh
# Snapshot once; push to S3 and to a local mirror (push prints the id)
id=$(snapdir push --store s3://my-bucket/snaps ./project)
snapdir push --store "file://$PWD/mirror" ./project

# Replicate S3 → GCS without the original directory: pull from S3, push to GCS
snapdir pull --store s3://my-bucket/snaps --id "$id" /tmp/from-s3
snapdir push --store gs://my-bucket/snaps /tmp/from-s3

# Pull from ANY backend, anywhere — byte-for-byte verified on fetch
snapdir pull --store gs://my-bucket/snaps --id "$id" ./restored

# Change a file and re-push: a new snapshot, only the changed object uploads
echo "an edit" >> ./project/a.txt
snapdir push --store s3://my-bucket/snaps ./project   # new id; unchanged objects skipped
```

A runnable, self-checking version — pushes to local + S3 + GCS, replicates S3→GCS, pulls from each and asserts the restores match — is in **[`examples/sync-local-s3-gcs.sh`](examples/sync-local-s3-gcs.sh)**. It runs with **zero cloud setup** (all `file://`), or against real clouds when you point it at them:

```sh
# all-local, no credentials:
examples/sync-local-s3-gcs.sh

# against real buckets (S3 via the AWS chain, GCS via ADC):
SNAPDIR_S3_TEST_STORE=s3://my-bucket/snaps \
SNAPDIR_GCS_TEST_STORE=gs://my-bucket/snaps \
examples/sync-local-s3-gcs.sh
```

## How it works

A manifest line is `TYPE PERMISSIONS CHECKSUM SIZE PATH`, sorted by path:

```text
D 700 dba5865c…5e7af4b 0 ./
F 600 af1349b9…41f3262 0 ./bar.txt
F 600 af1349b9…41f3262 0 ./foo.txt
```

- A directory's checksum is the BLAKE3 of its children's checksums (sorted, deduped, concatenated) — a merkle hash of the tree.
- The snapshot ID is the BLAKE3 of the `#`-stripped manifest text, so the same content always yields the same ID.
- Objects and manifests live under a 3-level sharded layout (`.objects/<h…>`, `.manifests/<id…>`) keyed on the hex digest — identical across the local cache and every store, so a store written by one snapdir is readable by any other.

Alternate checksums (`--checksum-bin md5sum`/`sha256sum`) and keyed BLAKE3 (`SNAPDIR_MANIFEST_CONTEXT`) are supported. Full format: [docs/rust-port/manifest-spec.md](docs/rust-port/manifest-spec.md).

## Stores

Pick a backend by `--store` URI scheme:

| Scheme    | Backend          | Auth                                                      |
| --------- | ---------------- | -------------------------------------------------------- |
| `file://` | Local filesystem | —                                                        |
| `s3://`   | Amazon S3        | AWS credential chain (env / profiles / SSO / metadata)   |
| `b2://`   | Backblaze B2     | B2 application key as `AWS_*` over the S3-compatible API  |
| `gs://`   | Google Cloud Storage | ADC / `GOOGLE_APPLICATION_CREDENTIALS` / metadata     |
| `ssh://`  | Any host with SSH shell access | SSH keys / agent via the system OpenSSH client |
| `sftp://` | Any SFTP server (incl. restricted/chroot accounts) | SSH keys / agent via the system OpenSSH client |

Cloud backends use native SDKs and standard credential chains — no bespoke env vars, no CLI shell-outs. Any other scheme dispatches to a `snapdir-<scheme>-store` binary on `PATH`; the `ssh://` and `sftp://` stores ship as two such binaries (`cargo install snapdir-ssh-store` provides both).

### SSH & SFTP stores

Store URLs take the form `ssh://[user@]host[:port]/abs/base/path` (likewise `sftp://`). Use `ssh://` when the remote gives you a shell — and it auto-accelerates when snapdir is installed remotely; use `sftp://` for restricted/chroot accounts (it speaks pure SFTP, so it works even under `ForceCommand internal-sftp` with no remote shell at all). Both require the `snapdir-ssh-store`/`snapdir-sftp-store` binaries on `PATH` and drive your system `ssh`/`sftp` client, so your `~/.ssh/config`, keys, agent, and `ProxyJump` setups keep working. Embedded passwords (`user:password@`) are rejected — authenticate with a key or an agent.

Each scheme reads its own env family — `SNAPDIR_SSH_STORE_*` for `ssh://`, `SNAPDIR_SFTP_STORE_*` for `sftp://`:

| Variable (suffix) | Default | Meaning |
| --- | --- | --- |
| `IDENTITY_FILE` | — | Private key path; also sets `IdentitiesOnly=yes` |
| `KNOWN_HOSTS` | — | `UserKnownHostsFile` override |
| `PORT` | — | Remote port (a port in the URL wins) |
| `CONNECT_TIMEOUT` | `10` | `ConnectTimeout` seconds |
| `JOBS` | `4` | Transfer parallelism (falls back to `SNAPDIR_JOBS`, then `SNAPDIR_MAX_JOBS`) |
| `CONTROL_PERSIST` | `60` | `ControlMaster` linger seconds (one TCP+auth handshake per operation) |
| `UMASK` | `077` | Umask for remote writes (`ssh://` only; `sftp://` uses explicit `chmod 600`) |
| `EXTRA_OPTS` | — | Extra `Key=Value` ssh options, appended **last** |

Both engines enforce an **un-weakenable, modern-only security floor** on every `ssh`/`sftp` invocation: modern-only key exchange (post-quantum hybrid `sntrup761x25519` first, then X25519), AEAD-only ciphers (ChaCha20-Poly1305, AES-GCM), Ed25519/RSA-SHA-2/ECDSA host keys (SHA-1 `ssh-rsa` and DSS excluded), `StrictHostKeyChecking=yes` always, `BatchMode=yes` (never an interactive prompt), and no password or keyboard-interactive auth. OpenSSH **≥ 8.5** is required locally (checked via `ssh -V`, fail-closed). Because OpenSSH takes the first value obtained for each option and the floor is always emitted first, `EXTRA_OPTS` structurally **cannot weaken the floor** — e.g. `EXTRA_OPTS="StrictHostKeyChecking=no"` is inert; extras can only add options the floor doesn't set.

When the remote host has a wire-compatible `snapdir` on its `PATH`, `ssh://` transfers automatically switch to a pack-stream protocol that diffs objects remotely and streams only what's missing in O(1) round trips (falling back gracefully otherwise). Runtime toggles: `SNAPDIR_SSH_NO_ACCEL=1` forces the plain path, `SNAPDIR_SSH_FORCE_ACCEL=1` errors instead of falling back, and `SNAPDIR_SSH_PULL_SENDALL=1` makes an accelerated fetch request the full object list. Protocol details: [docs/rust-port/ssh-wire-protocol.md](docs/rust-port/ssh-wire-protocol.md).

The accelerated pack stream **auto-negotiates zstd compression** (SNAPPACK 1Z): compression engages only when *both* ends are this release or newer, so a mixed-version pair simply stays uncompressed — no flag, no version mismatch. The level defaults to zstd's level 3 and is tunable with `SNAPDIR_SSH_ZSTD_LEVEL` (accepted range `1`–`19`; out-of-range values are clamped). Because SNAPPACK now compresses *above* the transport, on a WAN — or with HPN-SSH — disable the SSH client's own compression to avoid double-compressing already-compressed bytes: `EXTRA_OPTS="Compression=no"` (or `-o Compression=no` in your `~/.ssh/config`).

Limitation: `snapdir sync` does not support `ssh://`/`sftp://` stores (they have no in-process streaming surface) — `push`, `fetch`, `pull`, and `checkout` all work.

Crash durability on the receive side is controlled by `SNAPDIR_FSYNC`: `batch` (the default) fsyncs all received objects before committing the manifest last, so a manifest that survives a crash is backed by durable objects; `off` skips the barrier and relies on the OS to flush. On a journaling filesystem this gives the same crash-consistency story git provides — and no more. The `batch` default costs ~20% on a small-files receive (measured v1 +19.5% / zstd +29.9% on 5,000 × 4 KiB on Linux) — a fixed per-object fsync cost on the receive-pack path only (the ordinary `file://`/S3/GCS push path is unaffected); `SNAPDIR_FSYNC=off` opts out for speed at the cost of crash-safety.

## Scheduled inventories — one object pool, many manifest locations

A snapshot is a manifest plus the content objects it references, and those two halves don't have to live together. The global `--objects-store` / `$SNAPDIR_OBJECTS_STORE` flag routes **objects** to one shared pool's `.objects/` while **manifests** route to wherever `--store` points (`.manifests/`). One pool, many manifest locations — and **you** own the manifest layout (by date, host, or environment).

```sh
# Cron: a fresh manifest path per run, all sharing ONE object pool.
snapdir push \
  --objects-store s3://inventory/objects \
  --store "s3://inventory/manifests/$(date +%Y/%m/%d)" \
  /var/lib/app/data
```

Because objects are content-addressed, re-pushing to the same pool only costs the **changed bytes** — unchanged objects are already present and are skipped. So a daily inventory of mostly-static data is cheap: each run writes a new manifest and uploads only what actually changed. The catalog records the `--store` (manifest-side) URI. `--objects-store` is global, so it applies to `fetch`/`pull`/`verify` the same way; leave it unset and behavior is byte-for-byte unchanged. An external `custom://` store is rejected on either side (both halves are resolved in-process).

### Bucket-to-bucket sync with split pools

`snapdir sync` copies a snapshot directly between stores. The per-side `--from-objects` / `--to-objects` flags name an explicit object pool for each side, so source and destination can be **different buckets** with their own object pools — and cross-pool dedup still applies: objects already present in the destination pool are skipped.

```sh
snapdir sync --id "$id" \
  --from        s3://src/manifests --from-objects s3://src/objects \
  --to          gs://dst/manifests --to-objects   gs://dst/objects
```

These flags are distinct from the global `--objects-store`; when a side omits its `--*-objects` flag, that side is a plain colocated store. (`sync` does not support `ssh://`/`sftp://` stores.)

### `snapdir diff` — compare inventories, manifests only

`snapdir diff` compares two sides, each a **set of manifest locations**, and reports file-level changes — `A` (added), `D` (deleted), `M` (modified). It **reads manifests only**: it never downloads an object, so diffing across scheduled inventories is cheap even when the object pool is huge or unreachable.

```sh
# What changed between yesterday's and today's inventory?
snapdir diff \
  --from s3://inventory/manifests/2026/06/10 \
  --to   s3://inventory/manifests/2026/06/11
```

- `--from` / `--to` are **repeatable** and the refs on each side are **unioned** — point a side at several manifest stores (or, with the global `--id`, pin one side to a single manifest) and they merge into one logical set.
- `--all` also emits unchanged (`=`) paths; `--json` emits a `{status, path}` array instead of porcelain `X\t./path` lines.
- `--exit-code` adopts git's `diff --exit-code` semantics: exit `1` when any difference is found (after writing the report), `0` otherwise.
- `--on-conflict <error|last-wins>` controls an intra-side collision (the same path with differing content unioned across two refs on one side): `error` (the default) fails hard naming the path; `last-wins` lets the last ref win.

## Rate limiting & retries

For the network backends (`s3://`, `gs://`, `b2://`), snapdir paces its requests and retries transient failures so transfers stay polite to the provider and survive throttling. The local `file://` store does no network retrying. No extra dependencies are pulled in for any of this — it is all in-process.

### Retries & backoff

Transient network failures — HTTP `429` / `503`, S3 `SlowDown`, GCS `RESOURCE_EXHAUSTED`, request timeouts, and connection reset/closed — are retried with **full-jitter exponential backoff**. A non-transient error (for example a `404` not-found) fails immediately; it is never retried.

Each backoff is `random(0, min(cap, base × 2^attempt))`. When the server returns a `Retry-After` header (or a GCS backoff hint), it is honored as a floor — snapdir never retries sooner than the server asked, but may wait longer. Each SDK's own built-in retries are turned off so snapdir's policy is the single authority.

Defaults: **5 total attempts** (the first try plus up to four retries), **250 ms** base, doubling, capped at **30 s**.

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--max-retries` | `SNAPDIR_MAX_RETRIES` | `5` | Total attempts per request, including the first |
| `--retry-base-ms` | `SNAPDIR_RETRY_BASE_MS` | `250` | Base backoff delay (ms) |
| `--retry-max-ms` | `SNAPDIR_RETRY_MAX_MS` | `30000` | Backoff cap (ms) |

### Request & bandwidth rate limiting

| Flag | Env | Meaning |
| --- | --- | --- |
| `--max-requests` | `SNAPDIR_MAX_REQUESTS` | Cap on network requests per second |
| `--limit-rate` | `SNAPDIR_LIMIT_RATE` | Aggregate byte-throughput cap (e.g. `10M`, `512K`) |

When you don't set these, snapdir applies a conservative **per-backend default**, taken as the lower of each provider's published read/write limits:

| Backend | Requests/s | Bandwidth | Source |
| --- | --- | --- | --- |
| `s3://` | 3500 | uncapped | [AWS S3](https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance.html) |
| `gs://` (GCS) | 1000 | uncapped | [Google Cloud Storage](https://docs.cloud.google.com/storage/docs/request-rate) |
| `b2://` | 20 | 25 MiB/s | [Backblaze B2](https://www.backblaze.com/docs/cloud-storage-rate-limits) |
| `file://` / local | uncapped | uncapped | — |

Precedence, highest to lowest: **`--flag` > `SNAPDIR_*` env > per-backend default > global default.**

### Adaptive concurrency (opt-in)

`--adaptive` / `SNAPDIR_ADAPTIVE` is a separate, opt-in tuner that auto-tunes transfer concurrency (and network byte-rate) toward a polite fraction of measured capacity. It is **off by default** — default behavior is full speed. See the changelog for details.

## Use cases

- Reproducible build/dataset artifacts addressed by content hash.
- Deduplicated backup and restore to object storage from a single binary.
- Content distribution with end-to-end integrity — don't trust the store; `verify` re-hashes every object.
- Dataset and model versioning by deterministic snapshot ID.
- CI artifact caching keyed by directory content.
- Cross-cloud replication (S3↔B2↔GCS) without re-uploading unchanged data.

## Compared to

- **`git`** — snapshots arbitrary directories with no working copy, index, or commit history to carry around.
- **`restic` / `borg`** — plain content-addressed objects instead of a proprietary repo format; any tool can read the layout.
- **`rclone`** — adds content-addressing and verifiable snapshot IDs on top of object storage, not just file sync.
- **`ostree` / `casync` / IPFS** — a single static binary that writes directly to S3/B2/GCS object stores.

## Status & links

- **v1.5.0.** 15 subcommands: `manifest id stage push fetch pull checkout verify verify-cache flush-cache locations ancestors revisions defaults sync`.
- Full documentation, guides, and command reference: **[snapdir.org](https://snapdir.org)**.
- An embedded redb catalog tracks where snapshots live (`locations` / `ancestors` / `revisions`).
- Changelog: [docs/rust-port/CHANGELOG.md](docs/rust-port/CHANGELOG.md)
- Migrating from the earlier version: [docs/rust-port/migration.md](docs/rust-port/migration.md)
- Architecture Decision Records: [docs/adr/](docs/adr/)

## License

MIT — Copyright (c) 2022 Bermi Ferrer
