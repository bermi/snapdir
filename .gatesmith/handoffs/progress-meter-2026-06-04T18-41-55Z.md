# core handoff for progress-meter @ 2026-06-04T18:41:55Z

## Summary

Added a pure, lock-free progress meter to `snapdir-core` and wired the
filesystem walk to record into it. No terminal/IO/env code — the meter is just
atomics behind `&self`, sharable via `Arc<Meter>`.

- **New `progress` module (`src/progress.rs`, pub)** exporting `Meter`,
  `MeterSnapshot`, `Phase`:
  - `Phase { Idle, Hashing, Transfer }` — `Default = Idle`, `<-> u8` via private
    const `as_u8`/`from_u8` for `AtomicU8` storage (out-of-range decodes to
    `Idle`).
  - `MeterSnapshot` — `Copy + Default`, fields `bytes_in/bytes_out/objects_done/
    objects_total/objects_skipped/in_flight: u64` + `phase: Phase`.
  - `Meter` — `AtomicU64` for the six counters + `AtomicU8` for phase; `Debug +
    Default`, `Send + Sync` (all atomics). Methods all `&self`,
    `Ordering::Relaxed`: `new`, `add_in`, `add_out`, `object_started`
    (in_flight+=1), `object_finished` (in_flight-=1 via saturating CAS so a
    stray finish can't underflow, objects_done+=1), `set_total`, `add_skipped`,
    `set_phase`, `phase`, `snapshot`.
  - Re-exported from `lib.rs`: `pub mod progress;` +
    `pub use progress::{Meter, MeterSnapshot, Phase};`.
- **Walk recording (`src/walk.rs`)** — added
  `pub fn walk_with_meter(root, options, hasher, meter: Option<&Meter>)`
  holding the old walk body; the existing `pub fn walk(...)` now just delegates
  `walk_with_meter(root, options, hasher, None)` (signature unchanged, so the
  CLI lane keeps compiling). `meter: Option<&Meter>` is threaded through
  `discover_dir` (and its recursion). When `Some`: `set_phase(Phase::Hashing)`
  at entry, and at the one point a regular file's bytes are read+hashed,
  `meter.add_in(bytes.len() as u64)` + `meter.object_finished()`. Directories
  and symlinks record nothing. Re-exported `walk_with_meter` from `lib.rs`.
- The hash hook is the only recording site, gated on `Some`, so the manifest
  output is byte-identical with or without a meter (proved by a test).

## Files changed

```
 crates/snapdir-core/src/lib.rs  |  4 +-
 crates/snapdir-core/src/walk.rs | 87 ++++++++++++++++++++++++++++++++++++++++++
 crates/snapdir-core/src/progress.rs | 246 +++++++++++++++++++++ (new, untracked)
```

(`git diff --stat` shows lib.rs + walk.rs; `progress.rs` is a new untracked file
of 246 lines.)

## Local verification result

```
$ cargo test -p snapdir-core --locked progress_meter -- --nocapture
running 4 tests
test progress::tests::progress_meter_in_flight_gauge ... ok
test progress::tests::progress_meter_counters_and_snapshot ... ok
test walk::tests::progress_meter_walk_records_files_and_bytes ... ok
test walk::tests::progress_meter_walk_output_unchanged ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 74 filtered out

$ cargo test -p snapdir-core --locked
test result: ok. 78 passed; 0 failed; ...   (lib)
test result: ok. 16 passed; 0 failed; ...   (compat_golden)
test result: ok.  8 passed; 0 failed; ...   (golden_b3sum)
test result: ok.  3 passed; 0 failed; ...   (proptest_roundtrip)
test result: ok.  2 passed; 0 failed; ...   (doc-tests)

$ cargo clippy -p snapdir-core --all-targets --all-features --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.52s   (clean)

$ cargo fmt -p snapdir-core -- --check
fmt OK

$ shasum -a 256 -c .gatesmith/manifest-format.sha.lock
crates/snapdir-core/src/manifest.rs: OK
crates/snapdir-core/src/merkle.rs: OK
crates/snapdir-core/src/excludes.rs: OK
```

## Reuse check / Blockers

- Pure atomics, no IO/env/terminal code; library purity preserved.
- SHA-locked files (`manifest.rs`, `merkle.rs`, `excludes.rs`) untouched —
  sha-lock check is all OK.
- Walk output is identical with/without the meter
  (`progress_meter_walk_output_unchanged` asserts byte-identical manifests).
- Existing `walk(...)` signature unchanged (delegates with `None`); CLI lane
  unaffected.
- clippy `-D warnings` clean, `cargo fmt --check` clean.
- No new dependencies. The CLI renderer that reads the meter is a later,
  different-lane gate — no cross-lane blockers here.

Ready for PM verification: YES
