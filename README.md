# snapdir

Content-addressable directory snapshots: hash a directory into a deterministic ID, push it to object storage, and pull it back byte-for-byte verified anywhere.

`snapdir` snapshots a directory by content. Every snapshot is a BLAKE3 manifest — one line per file/dir as `TYPE PERMISSIONS CHECKSUM SIZE PATH`, sorted by path, with directory checksums computed as a merkle hash of their children. The **snapshot ID** is the BLAKE3 of the manifest text, so identical content produces an identical ID on any machine. Objects and manifests are stored at content-addressed sharded keys, so identical files and snapshots are stored once and interoperate across stores.

A single static binary with **zero runtime dependencies** — all hashing and storage is in-process; nothing to install alongside it.

## Install

```sh
# crates.io — installs the `snapdir` binary
cargo install snapdir-cli
```

Other ways:

```sh
# Prebuilt release archives (static musl + per-platform builds)
#   https://github.com/snapdir/snapdir/releases

# As a Rust library
cargo add snapdir-core
```

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

Cloud backends use native SDKs and standard credential chains — no bespoke env vars, no CLI shell-outs. Any other scheme dispatches to a `snapdir-<scheme>-store` binary on `PATH`.

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

- **v1.0.1.** 14 subcommands: `manifest id stage push fetch pull checkout verify verify-cache flush-cache locations ancestors revisions defaults`.
- An embedded redb catalog tracks where snapshots live (`locations` / `ancestors` / `revisions`).
- Changelog: [docs/rust-port/CHANGELOG.md](docs/rust-port/CHANGELOG.md)
- Migrating from the earlier version: [docs/rust-port/migration.md](docs/rust-port/migration.md)
- Architecture Decision Records: [docs/adr/](docs/adr/)

## License

MIT — Copyright (c) 2022 Bermi Ferrer
