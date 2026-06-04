# stores handoff for stores-meter-wire @ 2026-06-04T19:03:59Z

## Summary

Threaded an optional `snapdir_core::Meter` through the four transfer
orchestrators so they record bytes-in/out + per-object progress. Recording is
purely advisory: with no meter (the constructor default `None`) behavior and
output are byte-identical and the cost is zero (the `Some` branch is a few
`Ordering::Relaxed` adds per object).

- **Meter rides like `config`.** Added `meter: Option<Arc<Meter>>` (default
  `None`) to `FileStore`, `S3Store`, `GcsStore`, each with a consuming
  `with_meter(self, Option<Arc<Meter>>) -> Self` builder. Existing constructors
  default it to `None`; the CLI (next gate) sets it after construction. `B2Store`
  has no transfer path of its own, so its `with_meter` forwards to the wrapped
  `S3Store` (added `use std::sync::Arc`). The core `Store` trait is unchanged —
  the meter is a concrete-store field, exactly like `config`.

- **Hook points (where in/out is recorded):**
  - `fetch_files_concurrent` (fetch.rs): new `meter: Option<&Meter>` param.
    `set_total(<sum of to-download entry.size>)`; per present-and-verified skip
    `add_skipped(1)`; per download `object_started()` → `add_in(bytes.len())`
    after the verified download → `add_out(bytes.len())` after `write_atomic`
    → `object_finished()`.
  - `push_objects_concurrent` + `upload_object` + `read_verified` (push.rs):
    new `meter: Option<&Meter>` params. `set_total(<sum of File entry.size>)`;
    `key_exists` skip → `add_skipped(1)`; else `object_started()`, `read_verified`
    records `add_in(len)` after the source verify, `put_bytes` success →
    `add_out(len)` + `object_finished()`.
  - `FileStore::parallel_copy` (file_store.rs): shares `self.meter.as_deref()`
    (`&Meter: Sync`) across the rayon closure. Per job `object_started()`, read
    source size, `persist` (read source + write target), then `add_in(len)` +
    `add_out(len)` + `object_finished()`. `fetch_files`/`push` also do
    `add_skipped(1)` for each present skip and `set_total(<to-copy bytes>)`.
  - `sync_snapshot` (sync.rs): new `meter: Option<&Meter>` PARAMETER (sync is
    store-to-store, not via one store's field). `set_phase(Transfer)` +
    `set_total(<File entry bytes>)` at start; `has_object` skip → `add_skipped(1)`;
    per copy `object_started()`, `get_object` → `add_in(len)`, `put_object` →
    `add_out(len)`, `object_finished()`. Existing `copied/skipped/bytes` atomics
    are untouched.

- **None ⇒ no-op:** every record site is guarded by `if let Some(m) = meter`;
  the `prefer BYTES total` choice means `objects_total` carries the byte total.

### Tests (all `meter_records*`, hermetic, FileStore-based)
- `sync::tests::meter_records_sync`: A→empty-B sync with `Some(&meter)` →
  bytes_in == bytes_out == total object bytes, objects_done == N, skipped == 0,
  phase == Transfer; a second sync into a pre-seeded (objects-present,
  manifest-absent) dest → objects_skipped == N, objects_done == 0, no bytes.
- `file_store::tests::meter_records_filestore_push_fetch`: a FileStore
  `with_meter(Some(arc))` push then fetch → after push bytes_in/out == total,
  objects_done == N; after fetch == 2×total / 2N (each op touches every object).
- `file_store::tests::meter_records_none_is_identical`: metered vs `None` push+
  fetch produce byte-identical objects at identical sharded keys, byte-identical
  dest trees, and the same snapshot id.

## Files changed

```
 crates/snapdir-stores/src/b2_store.rs   |  13 +++
 crates/snapdir-stores/src/fetch.rs      |  31 +++++-
 crates/snapdir-stores/src/file_store.rs | 184 +++++++++++++++++++++++++++++++-
 crates/snapdir-stores/src/gcs_store.rs  |  37 ++++++-
 crates/snapdir-stores/src/push.rs       |  35 +++++-
 crates/snapdir-stores/src/s3_store.rs   |  37 ++++++-
 crates/snapdir-stores/src/sync.rs       | 113 ++++++++++++++++++--
 7 files changed, 429 insertions(+), 21 deletions(-)
```

## Local verification result

```
=== meter_records ===
running 3 tests
test file_store::tests::meter_records_filestore_push_fetch ... ok
test sync::tests::meter_records_sync ... ok
test file_store::tests::meter_records_none_is_identical ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 87 filtered out

=== full suite (result lines) ===
test result: ok. 90 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out   (lib)
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out    (shim integration)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out    (doc-test)

=== clippy ===
    Finished `dev` profile [unoptimized + debuginfo] target(s)   (no warnings, -D warnings)

=== sha lock ===
crates/snapdir-core/src/manifest.rs: OK
crates/snapdir-core/src/merkle.rs: OK
crates/snapdir-core/src/excludes.rs: OK

cargo fmt -p snapdir-stores --check: clean
```

## Reuse check / Blockers

- No core/cli/catalog edits — only `crates/snapdir-stores/`. Used
  `snapdir_core::{Meter, Phase}` as-is; `Store` trait untouched.
- Meter rides exactly like `config` (concrete-store field + `with_meter`
  builder); `B2Store` forwards to its inner `S3Store`.
- `None` is byte-identical: `meter_records_none_is_identical` proves identical
  objects, dest trees, and snapshot id; every record site is `Some`-guarded.
- `Sync` across rayon (`parallel_copy`, `sync_snapshot`) and async
  (`buffer_unordered` via `fetch_files_concurrent`/`push_objects_concurrent`) —
  `&Meter` shares fine since `Meter: Sync`.
- Phase-13/14 fetch/push/sync/concurrent tests stay green (90 lib tests).
- clippy `-D warnings` clean, `cargo fmt` clean, manifest-format sha-lock OK
  (no frozen-format file touched).

Ready for PM verification: YES
