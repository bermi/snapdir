# stores handoff for pull-skip-existing @ 2026-06-04T00:53:37Z

## Summary
`pull`/`fetch_files` was re-fetching every object on every run because the per-file
copy/download was unconditional. Added a fetch-side skip-if-present-and-verified
gate to all three stores: for every `PathType::File` entry, BEFORE any copy/GET,
if the destination file already exists, is a regular file, and its locally
recomputed BLAKE3 equals `entry.checksum`, the entry is skipped (no copy, no
network read). A mismatching/corrupt local file falls through and is repaired by
the existing fetch/persist path (overwrite). Directory handling
(`create_dir_all`) is unchanged.

In `FileStore::fetch_files` the skip is placed BEFORE the `source.exists()`
`ObjectNotFound` check, so a fully-populated dest succeeds even when the store
object is gone (the zero-object-reads proof). `S3Store`/`GcsStore` place the same
guard before `fetch_verified(...)`, so the network GET is avoided entirely.

`hash_file` (was private to `file_store.rs`) was lifted into a new crate-private
module `crates/snapdir-stores/src/util.rs` alongside a shared
`file_present_and_verified(target, expected, &hasher)` helper, so the three
backends share one definition of the skip decision. `file_store.rs` now imports
both; `s3_store.rs`/`gcs_store.rs` import `file_present_and_verified`. BLAKE3 is
recomputed consistently with `Blake3Hasher::new()` (the object content address
the stores already verify against). No object/manifest keys, push ordering, or
verify/retry discipline changed — this is fetch-side only; manifest format and
on-disk layout are untouched.

## Files changed
```
 crates/snapdir-stores/src/file_store.rs | 128 ++++++++++++++++++++++++++++++--
 crates/snapdir-stores/src/gcs_store.rs  |  10 +++
 crates/snapdir-stores/src/lib.rs        |   1 +
 crates/snapdir-stores/src/s3_store.rs   |  10 +++
 4 files changed, 143 insertions(+), 6 deletions(-)
```
Plus new file: `crates/snapdir-stores/src/util.rs` (crate-private shared helpers).

## Tests
FileStore is hermetic, so the proof is anchored there (in its `#[cfg(test)]` mod):
- `fetch_skip_present_verified` (primary): push a small tree to a temp FileStore,
  fetch into `dest` (populates it), then `rm -rf` the store's entire `.objects`
  tree (so any object read fails ObjectNotFound), fetch into the SAME `dest`
  again → returns Ok (ZERO object reads; every file skipped via local checksum),
  dest contents asserted intact.
- `file_store_fetch_repairs_corrupt_dest_and_skips_intact`: corrupt one dest file
  while removing an unrelated entry's store object → second fetch re-fetches and
  repairs the corrupt file and skips the intact one (only possible if the intact
  one is skipped, since its object is gone).
- `file_store_fetch_mismatch_then_missing_object_errors`: corrupt a dest file AND
  remove its store object → fetch errors `ObjectNotFound` (cannot repair),
  proving the skip is checksum-gated, not mere existence.

S3/GCS: the identical guard is applied; the skip logic is the shared
`file_present_and_verified` helper proven hermetically by the FileStore tests, so
the cloud guards are code-symmetric with FileStore. No env-gated emulator harness
was added in this spawn (the crate's live S3/GCS tests are creds/endpoint-gated
and exercise the same shared helper).

## Local verification result (last lines)
```
$ cargo test -p snapdir-stores --locked fetch_skip_present_verified -- --nocapture
running 1 test
test file_store::tests::fetch_skip_present_verified ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 60 filtered out

$ cargo test -p snapdir-stores --locked
test result: ok. 61 passed; 0 failed; ... (unit)
test result: ok. 5 passed; 0 failed;  ... (shim_external_store integration)
test result: ok. 1 passed; 0 failed;  ... (doc-tests)

$ cargo clippy -p snapdir-stores --all-targets --all-features --locked -- -D warnings
    Finished `dev` profile ... (no warnings)

$ cargo fmt -p snapdir-stores --check   -> clean (exit 0)

$ shasum -a 256 -c .gatesmith/manifest-format.sha.lock
crates/snapdir-core/src/manifest.rs: OK
crates/snapdir-core/src/merkle.rs: OK
crates/snapdir-core/src/excludes.rs: OK
```

## Reuse check / Blockers
- No core edits — `manifest-format.sha.lock` OK (manifest/merkle/excludes frozen).
- Object/manifest keys, push ordering, and verify/retry discipline unchanged
  (fetch-side only).
- BLAKE3 recompute via `Blake3Hasher::new()`, consistent with existing
  content-address verification.
- S3/GCS guards are code-symmetric with the FileStore-proven `file_present_and_verified`
  helper.
- clippy + fmt clean; in-lane only (`crates/snapdir-stores/`); no commit made (PM commits).

Ready for PM verification: YES
