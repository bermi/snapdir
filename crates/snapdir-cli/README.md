# snapdir-cli

The `snapdir` command-line binary — content-addressable directory snapshots.

`snapdir` takes content-addressable snapshots of directories using BLAKE3 merkle
hashing, and pushes/pulls them to local or cloud object stores (S3, Backblaze B2,
Google Cloud Storage). This crate ships the `snapdir` binary that exposes all
subcommands.

## Install

```sh
cargo install snapdir-cli
```

This installs the `snapdir` executable. Run `snapdir --help` to get started.

It is part of the snapdir project. Full documentation — install, command reference,
guides, and use cases — is at **[snapdir.org](https://snapdir.org)**; the source lives
in the [canonical repository](https://github.com/snapdir/snapdir).

## License

MIT
