# snapdir-cli

The `snapdir` command-line implementation — content-addressable directory snapshots.

`snapdir` takes content-addressable snapshots of directories using BLAKE3 merkle
hashing, and pushes/pulls them to local or cloud object stores (S3, Backblaze B2,
Google Cloud Storage, and ssh/sftp remotes). This crate is the implementation
library behind the `snapdir` binary.

## Install

The `snapdir` binary now ships in the [`snapdir`](https://crates.io/crates/snapdir)
crate:

```sh
cargo install snapdir
```

This installs the `snapdir` executable. Run `snapdir --help` to get started.

`snapdir-cli` versions ≤ 1.5.0 keep installing the binary directly
(`cargo install snapdir-cli@1.5.0` still works). From 1.5.1 this crate installs
no binary itself: it exposes `snapdir_cli::run()`, the binary entrypoint the
`snapdir` crate's `main` calls. `run()` is a binary entrypoint, not a stable
library API — the crate keeps publishing and the CLI surface stays supported,
but no semver guarantees are made beyond the documented `snapdir` binary
behavior.

It is part of the snapdir project. Full documentation — install, command reference,
guides, and use cases — is at **[snapdir.org](https://snapdir.org)**; the source lives
in the [canonical repository](https://github.com/snapdir/snapdir).

## License

MIT
