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

It is part of the snapdir project. See the
[canonical repository](https://github.com/snapdir/snapdir) for full documentation.

## License

MIT
