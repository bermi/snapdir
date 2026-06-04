# stores handoff for concurrent-upload @ 2026-06-04T02:42:57Z

## Summary

Added a shared, injectable push orchestrator that mirrors `fetch.rs`, making
`S3Store::push` / `GcsStore::push` (and `B2Store`, which delegates to its inner
`S3Store::push`) upload objects **concurrently** while preserving every push
invariant.

New module `crates/snapdir-stores/src/push.rs`:

- `push_objects_concurrent(manifest, &TransferConfig, upload_one, write_manifest)`
  — runs `run_concurrent` over the manifest's `File` entries at
  `config.concurrency`, then calls `write_manifest()` **exactly once and only
  after all object uploads return `Ok`**. If any upload errors, `run_concurrent`
  returns that error and the orchestrator returns it **without** calling
  `write_manifest` (manifest-last / all-or-nothing). Object order is irrelevant
  (content-addressed). Directories are filtered out (no object bytes).
- `upload_object(entry, object_key, source, &RateLimiter, key_exists, put_bytes)`
  — the shared per-object step S3/GCS inject: `key_exists(object_key)` → if
  present, SKIP (no read, no put = per-object content-addressed skip); else
  `read_verified` then `put_bytes`.
- `read_verified(entry, source, &RateLimiter)` — the single shared read +
  BLAKE3-verify (invalid-source guard → `StoreError::Integrity` on mismatch) +
  `rate_limiter.acquire(len)` step, so S3 and GCS never duplicate the verify.

Each store's `push` is now thin: it keeps `snapshot_id`, builds
`RateLimiter::new(self.config.max_bytes_per_sec)`, keeps the
**skip-if-manifest-present early return** (`if key_exists(manifest_key) return Ok`)
as a cheap pre-check, then calls the orchestrator injecting closures over
`self.key_exists` / `self.put_bytes` and the existing manifest-write path
(unchanged: manifest text built the same way, still verified to hash back to its
id before `put_bytes`). Object keys, sharding, manifest format, and the
manifest's own verify are unchanged. GCS's `put_bytes` still boxes its large
upload future internally, so that is preserved through the injected closure.

## Files changed

```
 crates/snapdir-stores/src/gcs_store.rs | 100 ++++++++++++++++-----------------
 crates/snapdir-stores/src/lib.rs       |   1 +
 crates/snapdir-stores/src/s3_store.rs  | 100 ++++++++++++++++-----------------
 3 files changed, 101 insertions(+), 100 deletions(-)
 (new) crates/snapdir-stores/src/push.rs   [pub(crate) orchestrator + tests]
```

## Local verification result

`cargo test -p snapdir-stores --locked concurrent_upload -- --nocapture`:

```
running 4 tests
test push::tests::concurrent_upload_rejects_corrupt_source ... ok
test push::tests::concurrent_upload_skips_present_objects ... ok
test push::tests::concurrent_upload_all_or_nothing_on_failure ... ok
test push::tests::concurrent_upload_all_objects_then_manifest ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 68 filtered out
```

Full suite `cargo test -p snapdir-stores --locked`: **72 unit + 5 integration +
1 doc test passed, 0 failed** (no regressions).

`cargo clippy -p snapdir-stores --all-targets --all-features --locked -- -D warnings`:
clean. `cargo fmt -p snapdir-stores -- --check`: clean.

`shasum -a 256 -c .gatesmith/manifest-format.sha.lock`: all OK (manifest.rs,
merkle.rs, excludes.rs).

The four required `concurrent_upload*` tests drive the orchestrator with FAKE
injected closures (no network/MinIO):
- `concurrent_upload_all_objects_then_manifest` — every absent object uploaded,
  in-flight high-water == min(concurrency, n) (== 1 at concurrency 1), and
  `write_manifest` called exactly once and only after all uploads completed
  (asserted via an uploads-completed-count snapshot taken inside write_manifest).
- `concurrent_upload_skips_present_objects` — present object keys are never
  uploaded.
- `concurrent_upload_all_or_nothing_on_failure` — one upload errors →
  orchestrator returns Err and `write_manifest` flag stays false (NEVER called).
- `concurrent_upload_rejects_corrupt_source` — a tampered source whose bytes no
  longer match the checksum → `Integrity` error, no manifest.

## Reuse check / Blockers

- No core/cli/catalog edits — lane stayed strictly in `crates/snapdir-stores/`.
- Reused `run_concurrent` + `RateLimiter` + `TransferConfig` from `transfer.rs`;
  mirrored the `fetch.rs` injectable-orchestrator pattern (`&self`/`&limiter`
  shared across buffered tasks, same borrow shape).
- Manifest-last / all-or-nothing proven by test; per-object skip-present + the
  shared read+verify (single definition, no S3/GCS duplication) preserved; the
  store-level skip-if-manifest-present early return kept.
- Object keys / sharding / manifest format / manifest self-verify unchanged.
- sha-lock OK; clippy + fmt clean.

Ready for PM verification: YES
