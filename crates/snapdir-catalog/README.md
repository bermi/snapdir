# snapdir-catalog

Catalog library for [snapdir](https://github.com/snapdir/snapdir) — content-addressable
directory snapshots.

This crate implements snapdir's local catalog: a pure-Rust,
[redb](https://crates.io/crates/redb)-backed embedded store that tracks
**locations**, **ancestors**, and **revisions** of snapshots. It has no SQLite or
TLS dependencies.

It is part of the snapdir project. Full documentation, the CLI, and the available
storage backends are at **[snapdir.org](https://snapdir.org)**; the source lives in the
[canonical repository](https://github.com/snapdir/snapdir).

## License

MIT
