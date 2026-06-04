# stores handoff for stream-store-trait @ 2026-06-04T17:18:46Z

## Summary

Added the object/manifest-level, content-addressed, verified `StreamStore`
trait (foundation for a later store-to-store sync orchestrator) entirely within
`crates/snapdir-stores/`. No `snapdir-core` / cli / catalog edits — `StreamStore`
is a NEW trait in the stores crate with `Store` as its supertrait.

- **New module `src/stream.rs`** (`pub mod stream;` + `pub use stream::StreamStore;`
  in `lib.rs`). Defines `pub trait StreamStore: Store` with the four sync methods:
  `has_object`, `get_object`, `put_object`, `put_manifest`. Each read/write is
  BLAKE3-verified against the address it is filed under; corruption surfaces as
  `StoreError::Integrity` rather than bad bytes / a misfiled blob.
- **FileStore** (`file_store.rs`): `has_object` = `object_disk_path().exists()`;
  `get_object` reads the object path (`ObjectNotFound` if absent), verifies BLAKE3
  == checksum (`Integrity` on mismatch); `put_object` verifies bytes hash to the
  checksum BEFORE writing, then temp-sibling + atomic rename (reusing the existing
  `temp_sibling`/rename discipline, creating parent dirs); `put_manifest` delegates
  to the existing `write_manifest(.., manifest_disk_path(id), id, &Blake3Hasher)`.
- **S3Store** (`s3_store.rs`) and **GcsStore** (`gcs_store.rs`): same shape, using
  the private async `key_exists`/`get_bytes`/`put_bytes` + `object_key`/`manifest_key`,
  each wrapped in `self.runtime.block_on(...)`. `get_object` does
  `get_bytes(object_key).await?.ok_or(ObjectNotFound)` then BLAKE3-verifies;
  `put_object` verifies first, then `put_bytes`; `put_manifest` mirrors the EXISTING
  manifest-write tail in `push` (`to_string()` + `'\n'`, verify `hash_hex == id`,
  `put_bytes(manifest_key, ...)`).
- **B2Store** (`b2_store.rs`): every `StreamStore` method delegates to `self.inner`
  (the inner `S3Store`'s impl).
- **ExternalStore** (shim.rs): intentionally NOT implemented (shell/local-path based,
  cannot stream raw object blobs).
- Tests (`stream_store*`, hermetic via FileStore) added to `file_store.rs`
  `#[cfg(test)]`: object round-trip, get-object rejects corruption, put-object
  rejects wrong checksum (nothing stored), put-manifest round-trips through
  `get_manifest`.

## Files changed

```
 crates/snapdir-stores/src/b2_store.rs   |  19 ++++
 crates/snapdir-stores/src/file_store.rs | 156 ++++++++++++++++++++++++++++++++
 crates/snapdir-stores/src/gcs_store.rs  |  65 +++++++++++++
 crates/snapdir-stores/src/lib.rs        |   5 +
 crates/snapdir-stores/src/s3_store.rs   |  65 +++++++++++++
 5 files changed, 310 insertions(+)
```
(plus the new untracked `crates/snapdir-stores/src/stream.rs`)

## Local verification result

```
$ cargo test -p snapdir-stores --locked stream_store -- --nocapture
running 4 tests
test file_store::tests::stream_store_put_object_rejects_wrong_checksum ... ok
test file_store::tests::stream_store_get_object_rejects_corruption ... ok
test file_store::tests::stream_store_filestore_object_roundtrip ... ok
test file_store::tests::stream_store_put_manifest_roundtrips ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 76 filtered out

$ cargo test -p snapdir-stores --locked
test result: ok. 80 passed; 0 failed; ...   (unit)
test result: ok. 5 passed; 0 failed; ...    (shim_external_store integration)
test result: ok. 1 passed; 0 failed; ...    (doc-tests)

$ cargo clippy -p snapdir-stores --all-targets --all-features --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.96s   (clean)

$ cargo fmt -p snapdir-stores --check
fmt clean

$ shasum -a 256 -c .gatesmith/manifest-format.sha.lock
crates/snapdir-core/src/manifest.rs: OK
crates/snapdir-core/src/merkle.rs: OK
crates/snapdir-core/src/excludes.rs: OK
```

## Reuse check / Blockers

- No core edits (the `Store` trait is untouched; `StreamStore` is a new
  supertrait-of-`Store` in the stores lane).
- Reused existing primitives: FileStore `object_disk_path`/`manifest_disk_path`/
  `write_manifest`/`temp_sibling`; S3/GCS `key_exists`/`get_bytes`/`put_bytes`/
  `object_key`/`manifest_key`/`runtime.block_on`; `Blake3Hasher` + the
  `StoreError::Integrity { address, expected, actual }` shape.
- Sharded object/manifest keys and the manifest byte-format are unchanged
  (manifest write mirrors `push`'s `to_string()` + `'\n'`); `manifest-format.sha.lock`
  is OK.
- clippy `-D warnings` + fmt clean. No new dependencies. No shelling out.
- All changes confined to `crates/snapdir-stores/`. (Pre-existing unrelated
  untracked file `.claude/ralph-loop.local.md` not touched.)
- No cross-lane needs; no blockers.

Ready for PM verification: YES
