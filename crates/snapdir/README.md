# snapdir

Content-addressed directory snapshots — the `snapdir` CLI.

`snapdir` takes content-addressable snapshots of directories using BLAKE3 merkle
hashing, and pushes/pulls them to local or cloud object stores (S3, Backblaze B2,
Google Cloud Storage, and ssh/sftp remotes).

## Install

```sh
cargo install snapdir
```

This installs the `snapdir` executable. Run `snapdir --help` to get started.

The implementation lives in the [`snapdir-cli`](https://crates.io/crates/snapdir-cli)
library crate; this crate ships the binary under the flagship `snapdir` name.

Full documentation — install, command reference, guides, and use cases — is at
**[snapdir.org](https://snapdir.org)**; the source lives in the
[canonical repository](https://github.com/snapdir/snapdir).

## License

MIT
