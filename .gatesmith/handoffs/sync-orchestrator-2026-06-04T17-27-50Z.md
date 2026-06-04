# stores handoff for sync-orchestrator @ 2026-06-04T17:27:50Z

## Summary

Added streaming store-to-store snapshot copy (`sync_snapshot`) in
`crates/snapdir-stores/`. It copies ONE snapshot's manifest + raw objects
directly source-store → dest-store through memory only — the function takes
**no `&Path` anywhere**, which is the structural guarantee that nothing is
staged on local disk.

- New `pub struct BlockingRateLimiter` in `transfer.rs`: a **synchronous** token
  bucket (`std::sync::Mutex` + `std::time::Instant` + `std::thread::sleep`) that
  mirrors `RateLimiter::acquire`'s math (refill at `rate`, ~1s burst, deficit
  wait for over-capacity objects). `new(None)`/`new(Some(0))` = unlimited no-op.
  Arc-shareable/Clone. The existing async `RateLimiter` is **untouched**, so the
  Phase-13 fetch/push paths are unaffected.
- New `pub mod sync` (`src/sync.rs`) exporting `sync_snapshot` + `SyncReport`.
  `sync_snapshot(from: &(dyn StreamStore + Sync), to: &(dyn StreamStore + Sync),
  id, config, dry_run)`:
  - **Fast path:** `to.get_manifest(id).is_ok()` → already mirrored, zero report.
  - Verifies the source manifest via `from.get_manifest(id)` (hashes to `id`).
  - Filters to `File` entries (skips `Directory`), parallelizes them across a
    rayon `ThreadPool` sized to `config.concurrency` (`par_iter().try_for_each`),
    exactly like `FileStore::parallel_copy`. Per object: skip if
    `to.has_object`; else if `dry_run` count "would copy" (no get/put); else
    `from.get_object` → `limiter.acquire_blocking(len)` → `to.put_object`. Bytes
    live only in memory.
  - **Manifest-last / all-or-nothing:** `to.put_manifest` only after the rayon
    pass returns `Ok` and `!dry_run`. First object error stops the pass and
    writes NO manifest.
  - Counters are `AtomicUsize`/`AtomicU64`; the `BlockingRateLimiter` is built
    once (`Arc`) and shared by the closure.

### Sync-vs-async rate limiter

`StreamStore`'s methods are SYNC and `block_on` their store's private runtime
internally, so driving them from the async `run_concurrent`/`RateLimiter` would
nest tokio runtimes and panic. The orchestrator therefore uses a **rayon pool
of plain OS threads** (no nested runtime) and a **separate synchronous
`BlockingRateLimiter`** (`std::thread::sleep`, not `.await`). The async
`RateLimiter` was left exactly as-is.

`StreamStore` did not need a new supertrait: the `+ Sync` bound on the `dyn`
references in `sync_snapshot`'s signature is sufficient, and FileStore/S3Store/
GcsStore/B2Store are already `Sync` (confirmed — full suite + new tests pass
when shared across rayon threads).

## Files changed

```
 crates/snapdir-stores/src/lib.rs      |  10 ++-  (pub mod sync; exports; module doc)
 crates/snapdir-stores/src/transfer.rs | 158 ++++  (BlockingRateLimiter + unit test)
 crates/snapdir-stores/src/sync.rs     | NEW     (sync_snapshot, SyncReport, tests)
```

(All within `crates/snapdir-stores/`. `sync.rs` is new/untracked.)

## Local verification result

```
cargo test -p snapdir-stores --locked sync_snapshot -- --nocapture
running 7 tests
test sync::tests::sync_snapshot_dry_run_writes_nothing ... ok
test sync::tests::sync_snapshot_all_or_nothing ... ok
test sync::tests::sync_snapshot_mirrors_snapshot ... ok
test sync::tests::sync_snapshot_skip_present_is_incremental ... ok
test sync::tests::sync_snapshot_no_local_fs ... ok
test sync::tests::sync_snapshot_skip_present_per_object ... ok
test transfer::tests::sync_snapshot_blocking_rate_limiter ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 80 filtered out

cargo test -p snapdir-stores --locked
test result: ok. 87 passed; 0 failed (lib) + 5 passed (shim) + 1 doc-test
  (async RateLimiter / fetch / push / transfer_config tests all still green)

cargo clippy -p snapdir-stores --all-targets --all-features --locked -- -D warnings
  Finished — no warnings

cargo fmt -p snapdir-stores  — clean

shasum -a 256 -c .gatesmith/manifest-format.sha.lock
  crates/snapdir-core/src/manifest.rs: OK
  crates/snapdir-core/src/merkle.rs: OK
  crates/snapdir-core/src/excludes.rs: OK
```

## Reuse check / Blockers

- No core/cli/catalog edits — diff is confined to `crates/snapdir-stores/`.
- Concurrency via rayon pool sized to `config.concurrency` (no nested tokio
  runtime); the SYNC `BlockingRateLimiter` is SEPARATE from the async
  `RateLimiter` (no Phase-13 regression — fetch/push/transfer_config stay green).
- `sync_snapshot` takes NO `&Path` (memory-only; `sync_snapshot_no_local_fs`
  asserts nothing is created outside the two store dirs).
- Manifest-last / all-or-nothing verified (`sync_snapshot_all_or_nothing`:
  Err returned and dest has no manifest).
- sha-lock OK; clippy + fmt clean.

Ready for PM verification: YES
