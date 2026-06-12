# snapdir

Content-addressable directory snapshots: hash a directory into a deterministic
ID, push it to object storage, and pull it back byte-for-byte verified anywhere.

`snapdir` snapshots a directory by content. Every snapshot is a BLAKE3
manifest — one line per file/dir, sorted by path, with directory checksums
computed as a merkle hash of their children — so identical content produces an
identical **snapshot ID** on any machine. Objects and manifests are stored at
content-addressed sharded keys: identical files and snapshots are stored once
and interoperate across stores. A single static binary with **zero runtime
dependencies** — all hashing and storage is in-process.

## Install

```sh
cargo install snapdir
```

This installs the `snapdir` executable. Run `snapdir --help` to get started.
Prebuilt release archives (static musl + per-platform builds, no Rust toolchain
needed) are on the [releases page](https://github.com/snapdir/snapdir/releases).

Previously the binary was installed via `cargo install snapdir-cli`; that crate
is now the implementation library this crate wraps (its versions ≤ 1.5.0 still
install the binary).

## Quick start — 60 seconds, no setup

No cloud account needed: snapshot a directory and round-trip it through a
**local** store.

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

Swap `file://$PWD/store` for `s3://…`, `gs://…`, `b2://…`, or an
`ssh://`/`sftp://` remote and the exact same commands push to the cloud.

## Stores

Pick a backend by `--store` URI scheme:

| Scheme    | Backend                                                |
| --------- | ------------------------------------------------------ |
| `file://` | Local filesystem                                       |
| `s3://`   | Amazon S3 (AWS credential chain)                       |
| `b2://`   | Backblaze B2 (S3-compatible API)                       |
| `gs://`   | Google Cloud Storage (ADC)                             |
| `ssh://`  | Any host with SSH shell access (system OpenSSH client) |
| `sftp://` | Any SFTP server, incl. restricted/chroot accounts      |

The cloud backends are built in — native SDKs and standard credential chains,
no bespoke env vars, no CLI shell-outs. The `ssh://` and `sftp://` stores ship
as two external-store binaries; `cargo install snapdir-ssh-store` provides both.

On the receiving side of an accelerated `ssh://` push, `SNAPDIR_FSYNC` controls
crash durability: `batch` (the default) fsyncs every received object before
committing the manifest, so a crash mid-receive can never leave a manifest
pointing at objects that aren't durably on disk. That safety costs ~20% on a
small-files receive (measured v1 +19.5% / zstd +29.9% on 5,000 × 4 KiB on
Linux); `SNAPDIR_FSYNC=off` is faster but not crash-safe. The cost is on the
receive-pack path only — the ordinary `file://`/S3/GCS push path is unaffected.

## Scheduled inventories — one object pool, many manifests

A snapshot's manifest and its content objects don't have to live together. The
global `--objects-store` / `$SNAPDIR_OBJECTS_STORE` flag routes **objects** to
one shared pool's `.objects/`, while **manifests** go wherever `--store` points
(`.manifests/`). One pool, many manifest locations — and the caller owns the
layout (by date, host, or environment):

```sh
# Cron: a new manifest path per run, all sharing ONE object pool.
snapdir push \
  --objects-store s3://inventory/objects \
  --store "s3://inventory/manifests/$(date +%Y/%m/%d)" \
  /var/lib/app/data
```

Objects are content-addressed, so re-pushing to the same pool only costs the
**changed bytes** — unchanged objects are skipped. Scheduled inventories of
mostly-static data are therefore cheap. Leave `--objects-store` unset and
behavior is byte-for-byte unchanged.

For **bucket-to-bucket** copies, `snapdir sync --from-objects/--to-objects`
names an explicit object pool per side — source and destination can be different
buckets, and objects already present in the destination pool are skipped.

`snapdir diff` compares two sides — each a **set of manifest locations**
(`--from`/`--to`, both repeatable and unioned per side) — and reports file-level
changes (`A`/`D`/`M`). It **reads manifests only**, never downloading an object,
so diffing across scheduled inventories is cheap:

```sh
snapdir diff \
  --from s3://inventory/manifests/2026/06/10 \
  --to   s3://inventory/manifests/2026/06/11
```

`--all` also lists unchanged paths, `--json` emits a `{status, path}` array,
`--exit-code` gives git-style exit codes (`1` on any difference), and
`--on-conflict <error|last-wins>` resolves a same-path/differing-content
collision when two refs are unioned on one side.

## Links

- Full documentation — install, command reference, guides, and use cases:
  **[snapdir.org](https://snapdir.org)**
- Source: the [canonical repository](https://github.com/snapdir/snapdir)
- Implementation: the [`snapdir-cli`](https://crates.io/crates/snapdir-cli)
  library crate — this crate is the flagship-named shim that ships the binary

## License

MIT
