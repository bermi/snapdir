# stores handoff for filestore-parallel @ 2026-06-04T02:49:49Z

## Summary

Parallelized `FileStore::push` and `FileStore::fetch_files` object copies across
a **bounded rayon thread pool** sized by `self.config.concurrency` (the local
`file://` backend stays synchronous — no async/`block_on`, and the network
`RateLimiter` does NOT apply to local copies, only the concurrency cap).

- Added a private `FileStore::parallel_copy(&[(source, target, checksum)])`
  helper. It builds a `rayon::ThreadPoolBuilder::new().num_threads(concurrency)`
  pool and runs `pool.install(|| jobs.par_iter().try_for_each(|...| persist(...,
  &Blake3Hasher::new())))`. `try_for_each` propagates the first `StoreError` and
  stops. A fresh stateless `Blake3Hasher` is created per task (cheap; sidesteps
  any `Sync` concern). Empty job list short-circuits with no pool build.
- `push`: keeps the snapshot-id compute + skip-if-manifest-present early return,
  and the skip-if-present-per-object check. It now COLLECTS the absent objects
  into copy jobs, runs them through `parallel_copy`, and only on `Ok` calls
  `write_manifest(...)`. **MANIFEST-LAST / ALL-OR-NOTHING preserved**: any copy
  error returns immediately and writes NO manifest.
- `fetch_files`: keeps the FIRST sequential pass that `create_dir_all`s every
  `Directory` entry, pre-creates each `File`'s parent (so parallel copies never
  race on a shared ancestor `create_dir_all`), short-circuits
  `file_present_and_verified` files (Phase-12 skip-if-present-and-verified, zero
  object reads), and returns `ObjectNotFound` when a needed source object is
  missing. The remaining file copies are collected and run through the same
  bounded `parallel_copy` pool.
- `concurrency == 1` → a 1-thread pool → deterministic single-threaded
  sequential copy (verified byte-identical to a parallel run).
- PRESERVED: `persist()` semantics (copy+verify+retry+atomic rename), object
  keys, sharding, manifest format. No async S3/GCS path, fetch.rs/push.rs
  orchestrators, or core/cli/catalog edits.

`rayon = "1"` added to `crates/snapdir-stores/Cargo.toml`. rayon 1.12.0 was
already resolved in the lock transitively; the only lock delta is naming it as a
direct dep of `snapdir-stores` (root Cargo.lock regen — the accepted exception).

## Files changed

```
 Cargo.lock                              |   1 +
 crates/snapdir-stores/Cargo.toml        |   8 +
 crates/snapdir-stores/src/file_store.rs | 293 ++++++++++++++++++++++++++++++--
 3 files changed, 290 insertions(+), 12 deletions(-)
```

(crates/snapdir-stores/ only, + root Cargo.lock.)

## Local verification result

`cargo test -p snapdir-stores --locked filestore_parallel`:

```
running 4 tests
test file_store::tests::filestore_parallel_all_or_nothing_bad_object ... ok
test file_store::tests::filestore_parallel_concurrency_one_sequential ... ok
test file_store::tests::filestore_parallel_roundtrip_byte_identical ... ok
test file_store::tests::filestore_parallel_large_n_round_trips ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 72 filtered out
```

`cargo test -p snapdir-stores --locked` (full): 76 lib + 5 shim + 1 doc tests
pass; 0 failed. The existing file_store skip/repair/roundtrip tests stay green.

`cargo clippy -p snapdir-stores --all-targets --all-features --locked -- -D
warnings`: clean. `cargo fmt -p snapdir-stores -- --check`: clean.

`shasum -a 256 -c .gatesmith/manifest-format.sha.lock`: all OK (manifest.rs /
merkle.rs / excludes.rs unchanged).

## Reuse check / Blockers

- No core/cli/catalog edits; lane confined to crates/snapdir-stores/ (+ root
  Cargo.lock regen, the accepted exception).
- rayon added (`rayon = "1"`); already in the lock graph transitively (1.12.0),
  so it satisfies the >=3-day crate-age cooldown.
- `persist()` / skip-present / skip-present-and-verified / manifest-last /
  all-or-nothing / `ObjectNotFound` all preserved.
- sha-lock OK; clippy + fmt clean. Did NOT touch the async S3/GCS path or the
  shared fetch.rs/push.rs orchestrators. PM did not commit (per process).

Ready for PM verification: YES
