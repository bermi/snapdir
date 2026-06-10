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

## Links

- Full documentation — install, command reference, guides, and use cases:
  **[snapdir.org](https://snapdir.org)**
- Source: the [canonical repository](https://github.com/snapdir/snapdir)
- Implementation: the [`snapdir-cli`](https://crates.io/crates/snapdir-cli)
  library crate — this crate is the flagship-named shim that ships the binary

## License

MIT
