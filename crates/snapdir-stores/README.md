# snapdir-stores

Storage backends for [snapdir](https://github.com/snapdir/snapdir) — content-addressable
directory snapshots.

This crate provides the concrete `Store` implementations used by snapdir:

- `FileStore` — local filesystem store.
- S3, Backblaze B2, and Google Cloud Storage stores built on the native cloud
  SDKs (no shelling out to `aws`, `b2`, or `gcloud`).
- An external-store shim for delegating to custom backends.

It is part of the snapdir project. See the
[canonical repository](https://github.com/snapdir/snapdir) for full documentation
and the CLI.

## License

MIT
