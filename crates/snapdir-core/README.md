# snapdir-core

Core library for [snapdir](https://github.com/snapdir/snapdir) — content-addressable
directory snapshots.

This crate provides the building blocks shared across the snapdir workspace:

- The manifest format (parse, render, and validate snapdir manifests).
- BLAKE3 merkle hashing of files and directory trees.
- The `Store` trait that backends implement.
- Directory walking and the local object cache.

It is part of the snapdir project. Full documentation, the CLI, and the available
storage backends are at **[snapdir.org](https://snapdir.org)**; the source lives in the
[canonical repository](https://github.com/snapdir/snapdir).

## License

MIT
