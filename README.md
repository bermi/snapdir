# snapdir

Content-addressable directory snapshots: hash a directory into a deterministic ID, push it to object storage, and pull it back byte-for-byte verified anywhere.

`snapdir` snapshots a directory by content. Every snapshot is a BLAKE3 manifest — one line per file/dir as `TYPE PERMISSIONS CHECKSUM SIZE PATH`, sorted by path, with directory checksums computed as a merkle hash of their children. The **snapshot ID** is the BLAKE3 of the manifest text, so identical content produces an identical ID on any machine. Objects and manifests are stored at content-addressed sharded keys, so identical files and snapshots are stored once and interoperate across stores.

Single static binary, zero runtime dependencies. v1.0.0.

## Quick start

```sh
# Hash a directory: print its manifest and its deterministic snapshot ID
snapdir manifest ./mydir
snapdir id ./mydir

# Push a snapshot to S3 (objects + manifest, deduplicated, skip-if-present)
snapdir push --store s3://my-bucket/snapshots ./mydir

# Pull it back anywhere; objects are re-hashed and verified on fetch
snapdir pull --store s3://my-bucket/snapshots --id <snapshot-id> ./restored

# Verify a stored snapshot is byte-for-byte intact (re-hash every object)
snapdir verify --store s3://my-bucket/snapshots --id <snapshot-id>
```

`./restored` re-manifests to the same snapshot ID, with permissions restored.

## Install

A single, statically-linked binary with **no runtime dependencies** — nothing else to install; all hashing and storage is in-process.

```sh
# Prebuilt release archives (cargo-dist; static musl + per-platform builds)
# https://github.com/bermi/snapdir/releases

# From source
cargo install --git https://github.com/bermi/snapdir snapdir-cli
```

### Docker

The published image is built `FROM scratch`: it contains the fully-static musl `snapdir` binary and the bundled CA roots (`ca-certificates.crt`) for HTTPS to S3/B2/GCS — and **nothing else**. There is no libc, no shell, and no other runtime executables in the image.

```sh
# Run the published image
docker run --rm ghcr.io/bermi/snapdir manifest /data

# Or build the same scratch image from a clean checkout (no build-args needed)
docker build -t snapdir .
```

## Use cases

- Reproducible build/dataset artifacts addressed by content hash.
- Deduplicated backup and restore to object storage from a single binary.
- Content distribution with end-to-end integrity — don't trust the store; `verify` re-hashes every object.
- Dataset and model versioning by deterministic snapshot ID.
- CI artifact caching keyed by directory content.
- Cross-cloud replication (S3↔B2↔GCS) without re-uploading unchanged data.

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

## Compared to

- **`git`** — snapshots arbitrary directories with no working copy, index, or commit history to carry around.
- **`restic` / `borg`** — plain content-addressed objects instead of a proprietary repo format; any tool can read the layout.
- **`rclone`** — adds content-addressing and verifiable snapshot IDs on top of object storage, not just file sync.
- **`ostree` / `casync` / IPFS** — a single static binary that writes directly to S3/B2/GCS object stores.

## Status & links

- v1.0.0. 14 subcommands: `manifest id stage push fetch pull checkout verify verify-cache flush-cache locations ancestors revisions defaults`.
- An embedded redb catalog tracks where snapshots live (`locations` / `ancestors` / `revisions`).
- Changelog: [docs/rust-port/CHANGELOG.md](docs/rust-port/CHANGELOG.md)
- Migrating from the earlier version: [docs/rust-port/migration.md](docs/rust-port/migration.md)
- Architecture Decision Records: [docs/adr/](docs/adr/)

## License

MIT — Copyright (c) 2022 Bermi Ferrer
