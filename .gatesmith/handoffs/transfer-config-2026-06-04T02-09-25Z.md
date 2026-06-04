# stores handoff for transfer-config @ 2026-06-04T02:09:25Z

## Summary

Foundation-only gate. Added a new `pub mod transfer` to `snapdir-stores` and
re-exported its public surface (`TransferConfig`, `RateLimiter`,
`run_concurrent`). No transfer loop was touched — push/fetch_files stay
sequential this gate.

- **`TransferConfig { concurrency: NonZeroUsize, max_bytes_per_sec: Option<u64> }`**
  (derive Debug, Clone). `Default` auto-detects `available_parallelism()`,
  clamps to `[1, 16]`, and leaves bandwidth unlimited. `TransferConfig::new`
  clamps `concurrency` to >= 1.
- **`RateLimiter`** — zero-dependency async token bucket on `tokio::time`
  (`Arc<Inner>` with a `tokio::sync::Mutex<Bucket>`; `Clone` shares the bucket).
  `new(None)`/`new(Some(0))` => unlimited no-op fast path; `acquire(n)` refills
  at `max_bytes_per_sec`/sec, bursts up to ~1s of budget, and correctly waits
  out the deficit for objects larger than capacity. No new rate-limiter crate.
- **`run_concurrent`** — generic bounded-concurrency driver via
  `futures::stream::iter(items).map(op).buffer_unordered(concurrency).try_collect()`.
  Runs up to `concurrency` ops in flight, returns the first `StoreError`.
- **Backward-compatible constructors** (each adds a `config: TransferConfig`
  field + a `transfer_config()` getter so the field is not dead under
  `-D warnings`):
  - `S3Store::connect_with(url, endpoint, config)`; `connect` delegates with default.
  - `GcsStore::connect_with(url, config)`; `connect` delegates.
  - `B2Store::connect_with(url, endpoint, region, config)` → inner
    `S3Store::connect_with`; `connect` delegates. (config lives on inner S3Store.)
  - `FileStore::from_root_with_config(root, config)` +
    `new_with_config(store, config)`; `new`/`from_root` delegate. TransferConfig
    derives Clone/Debug so FileStore's derives still hold.
- Cargo.toml: added `futures = "0.3"` and `"time"` + `"sync"` to the existing
  tokio dep (kept `rt-multi-thread`). No unrelated features.

Frozen sharding / manifest format / push+fetch loops untouched.

## Files changed

```
 Cargo.lock                              |   3 +
 crates/snapdir-stores/Cargo.toml        |  13 +-
 crates/snapdir-stores/src/b2_store.rs   |  30 ++-
 crates/snapdir-stores/src/file_store.rs |  29 ++-
 crates/snapdir-stores/src/gcs_store.rs  |  23 +++
 crates/snapdir-stores/src/lib.rs        |   5 +
 crates/snapdir-stores/src/s3_store.rs   |  28 +++
 crates/snapdir-stores/src/transfer.rs   | 321 +++++++++++++++++++++++++++++
 8 files changed, 449 insertions(+), 3 deletions(-)
```

(Root `Cargo.lock` regen is the accepted lane-fence exception: +3 lines, only
new dependency edges to the already-vendored `futures` 0.3.32 family — no new
crate version introduced.)

## Local verification result

`cargo test -p snapdir-stores --locked transfer_config -- --nocapture` (gate cmd):

```
   Compiling snapdir-stores v1.0.1 (/Users/bermi/code/snapdir/crates/snapdir-stores)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 6.74s
     Running unittests src/lib.rs (target/debug/deps/snapdir_stores-00bb4e93c1a2dc46)

running 4 tests
test transfer::tests::transfer_config_default_caps_concurrency ... ok
test transfer::tests::transfer_config_run_concurrent_propagates_error ... ok
test transfer::tests::transfer_config_run_concurrent_max_in_flight ... ok
test transfer::tests::transfer_config_rate_limiter ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 61 filtered out; finished in 1.01s

     Running tests/shim_external_store.rs (...)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s
```

Test coverage (all deterministic):
- `transfer_config_default_caps_concurrency`: default concurrency in `[1, 16]`;
  `new(0,..)` clamps to 1.
- `transfer_config_run_concurrent_max_in_flight`: peak in-flight ==
  `min(concurrency, N)` — 4 for (c=4,N=12), strictly **1** for (c=1,N=5), 3 for
  (c=8,N=3).
- `transfer_config_run_concurrent_propagates_error`: failing op's
  `StoreError::Backend{message:"boom"}` is surfaced.
- `transfer_config_rate_limiter`: 1000 B/s limiter takes >= ~0.9s to acquire
  ~2000 B; unlimited returns < 0.2s.

Full suite: `cargo test -p snapdir-stores --locked` → 65 lib + 5 integration +
1 doctest, all pass (no regressions).

## Reuse check / Blockers

- No core/cli/catalog edits — only `crates/snapdir-stores/` (+ root Cargo.lock regen).
- Zero-dependency limiter: no new rate-limiter crate; built on `tokio::time` +
  `tokio::sync::Mutex`.
- `futures = "0.3"` added (already in lock at 0.3.32; only edges added — no new
  crate). tokio gained `time` + `sync` only.
- `shasum -a 256 -c .gatesmith/manifest-format.sha.lock` → OK (core untouched).
- `bash utils/ci/check-crate-age.sh` → OK (all 429 registry crates >= 3 days;
  futures family included).
- `cargo clippy -p snapdir-stores --all-targets --all-features --locked -- -D warnings`
  → clean. `cargo fmt -p snapdir-stores --check` → clean.
- Transfer loops (push/fetch_files), frozen sharding, and manifest format
  untouched.

Ready for PM verification: YES
