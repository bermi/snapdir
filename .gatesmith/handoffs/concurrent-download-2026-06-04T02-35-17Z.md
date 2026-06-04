# stores handoff for concurrent-download @ 2026-06-04T02:35:17Z

## Summary

Extracted a shared, injectable, hermetically-testable fetch orchestrator and
rewired `S3Store::fetch_files` / `GcsStore::fetch_files` (and therefore `B2Store`,
which delegates to `S3Store`) to download objects CONCURRENTLY instead of the old
sequential `for entry … fetch_verified().await … write_atomic()` loop.

New module `crates/snapdir-stores/src/fetch.rs`:

- `pub(crate) async fn fetch_files_concurrent(manifest, dest, &TransferConfig,
  &RateLimiter, download: impl Fn(&ManifestEntry) -> Fut<Output=Result<Vec<u8>,
  StoreError>>)`.
  1. **First pass (sequential):** `create_dir_all` for every `Directory` entry,
     pre-create each `File` entry's parent dir, and collect the `File` entries
     that are NOT `file_present_and_verified(target, checksum, &Blake3Hasher)`.
     The skip-present-and-verified short-circuit happens BEFORE any download —
     a present+verified file does ZERO downloads (preserves Phase-12
     pull-skip-existing). The injected `download` closure is never called for it.
  2. **Concurrent pass:** `run_concurrent(to_download, config.concurrency, …)`;
     each task does `rate_limiter.acquire(entry.size).await` (throttle by the
     manifest-declared object size), then `download(entry).await?`, then
     `write_atomic(&target, &bytes)?`. Distinct content-addressed entries write
     distinct targets, so concurrent writes are safe and `create_dir_all` already
     ran in pass 1.
  3. First error wins (propagated by `run_concurrent`, which cancels the rest).
- `write_atomic` was hoisted here (shared by both stores; the per-store copies
  were removed). `strip_leading_dot_slash` stays in each store (still used by
  `push` + existing unit tests).

S3/GCS `fetch_files` are now thin: build
`let limiter = RateLimiter::new(self.config.max_bytes_per_sec);` and call the
orchestrator inside the existing `block_on`, injecting
`|entry| async { let key = self.location.object_key(&entry.checksum);
self.fetch_verified(&key, &entry.checksum).await }`. PRESERVED: the per-object
BLAKE3 verify + 5x retry budget (`fetch_verified` unchanged), atomic write, and
object keys / sharding / manifest format (untouched). `&self` and `&limiter` are
shared immutably across tasks; `RateLimiter` was already `Clone`/`Arc`-backed so
no changes were needed there.

Hermetic tests (no network/MinIO), named `concurrent_download*`:
- `concurrent_download_orchestrator_materializes_all` — fake download closure
  returning canned bytes keyed by checksum; asserts every file lands at the right
  path with the right bytes, dirs created, and (AtomicUsize high-water mark) peak
  in-flight == min(concurrency, n) at concurrency=4 and == 1 at concurrency=1.
- `concurrent_download_skips_present_and_verified` — pre-creates a target already
  matching its checksum; asserts the closure is NEVER called for it (zero
  downloads) and IS called only for the missing entry.
- `concurrent_download_propagates_error` — a closure that errors on one entry →
  orchestrator returns that error.

## Files changed

```
 crates/snapdir-stores/src/fetch.rs     | (new) shared orchestrator + 3 tests
 crates/snapdir-stores/src/gcs_store.rs | 66 +++++++---------------------------
 crates/snapdir-stores/src/lib.rs       |  1 +
 crates/snapdir-stores/src/s3_store.rs  | 66 +++++++---------------------------
 3 files changed, 27 insertions(+), 106 deletions(-)  (+ new fetch.rs)
```

Only `crates/snapdir-stores/` touched. No core/cli/catalog edits.

## Local verification result

`cargo test -p snapdir-stores --locked concurrent_download -- --nocapture` (last lines):

```
running 3 tests
test fetch::tests::concurrent_download_propagates_error ... ok
test fetch::tests::concurrent_download_skips_present_and_verified ... ok
test fetch::tests::concurrent_download_orchestrator_materializes_all ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 65 filtered out; finished in 0.15s
```

Also green:
- `cargo test -p snapdir-stores --locked` → 68 unit + 5 shim + 1 doc test, all pass
  (incl. existing file_store skip/repair tests and s3/gcs key/parse tests).
- `cargo clippy -p snapdir-stores --all-targets --all-features --locked -- -D warnings` → clean.
- `cargo fmt -p snapdir-stores --check` → clean (exit 0).
- `shasum -a 256 -c .gatesmith/manifest-format.sha.lock` → all OK.

## Reuse check / Blockers

- No core/cli/catalog edits; strictly inside `crates/snapdir-stores/`.
- Reused existing `run_concurrent` + `RateLimiter` (Arc/Clone, no changes needed)
  and `util::file_present_and_verified`.
- Skip-present-and-verified preserved (zero downloads for present+verified files,
  asserted by test); per-object BLAKE3 verify + 5x retry preserved
  (`fetch_verified` untouched); atomic temp+rename write preserved.
- Object keys, sharding, and manifest format unchanged. No `gcloud`/`aws`/`b2`
  shelling introduced. No new dependencies (tests use a local `TempDir` helper).
- sha-lock OK; clippy + fmt clean.

Ready for PM verification: YES
