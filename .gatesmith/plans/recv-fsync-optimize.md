# Phase 29 — receive-pack durability cost investigation (Option 1)

## Why

The Phase-27 release gate (`release-perf-linux-1.7.0`) measured the receive
`batch`-vs-`off` cost on a **Linux** runner (bench-verify run 27390795784,
`snappack pack/receive/a`, 5k×4KiB) and got **v1 +19.5% / zstd +29.9%** — far
over the ≤5% acceptance. The Phase-27 assumption that Linux `sync_file_range`
makes batched durability ~free was **falsified**. Before changing the
`SNAPDIR_FSYNC` default or shipping the cost, investigate whether the cost is
removable while keeping crash-safety-by-default.

## What `batch` does today (from `recv-fsync-batch-stores`, `crates/snapdir-stores/src/fsync.rs`)

Per the design, for each pack the receiver does, in mode `Batch`:
1. **Per object** — a *writeout hint*: Linux `sync_file_range(WRITE)` (async
   writeback start), macOS `fsync` (writeout-only). ~5k syscalls for 5k objects.
2. **At the barrier (once, before the manifest)** — a *wait* pass: Linux
   `sync_file_range(WAIT_BEFORE|WRITE|WAIT_AFTER)` over the written ranges
   (or `sync_data` fallback) — this is what makes the objects durable.
3. **Manifest** — `write_manifest_durable`: fsync temp → rename → fsync parent
   shard dir (the "2 full syncs per pack"). Manifest is written LAST.

The **crash-safety guarantee** = "every referenced object is durable on disk
before the manifest exists" comes from **step 2 (the barrier) + step 3 (manifest
last)**. **Step 1 (the per-object writeout hints) is only a pipelining
optimization** — it starts writeback early so the barrier wait is shorter. It is
NOT required for correctness.

## Hypothesis

The **~5k per-object writeout-hint syscalls (step 1)** are the bulk of the
+20-30% cost — not the 2 barrier/manifest syncs. Dropping/conditioning them
while keeping steps 2+3 should recover most of the cost AND preserve the
manifest-last crash-safety guarantee.

## Approach (gate `recv-fsync-optimize-stores`, stores lane)

1. Read `crates/snapdir-stores/src/fsync.rs` + the `FileSink`/`read_pack`
   `Batch` path in `pack.rs`. Confirm steps 1/2/3 above and that step 1 is the
   per-object hint.
2. **Remove or condition the per-object writeout hint (step 1).** Keep the
   barrier wait (step 2) and the durable manifest write (step 3) UNCHANGED — they
   are the durability contract. Options, cheapest first:
   - drop the per-object hint entirely (barrier still flushes everything); OR
   - keep a single batched writeout hint over all ranges just before the wait
     (one syscall, not 5k); OR
   - make it a tunable that defaults off.
   Do NOT weaken the barrier or manifest-last ordering.
3. **Keep ALL existing `pack` tests green** — especially off-vs-batch identical
   filing, barrier-before-`put_manifest` ordering (recording/spy sink), and
   truncation/resume. The durability contract must be byte-identical; only the
   per-object hint changes. If a test pins the *number* of per-object syncs,
   update that count (it's an implementation detail, not the safety contract) and
   flag it.
4. **Measure locally** (macOS is fine for the HYPOTHESIS — it also does a
   per-object sync, so if dropping it helps here it'll help on Linux): run
   `cargo bench -p snapdir-benches --bench snappack -- 'pack/receive/a'` before
   and after, report the local batch-vs-off before/after in the handoff. (Not the
   acceptance — the acceptance is the Linux re-measure.)
5. Verification: `cargo test -p snapdir-stores pack --locked` (+ clippy/fmt).
   Lane: `crates/snapdir-stores/` only.

## Acceptance (gate `release-perf-linux-1.7.0`, re-measure)

After the optimization lands, the PM re-runs bench-verify on the Linux runner and
reads the NEW receive batch-vs-off delta on `pack/receive/a` (v1+zstd):
- **≤5%** → durability-by-default is salvaged; release proceeds (`release-prep-1.7.0`).
- **still >5%** → the per-object hints were NOT the (whole) cost; ESCALATE to the
  operator to fall back to **Option 2** (default `off`, opt-in `batch`) or
  **Option 3** (accept + document the cost). Do NOT relax the gate silently.

## Safety note

This is an optimization of EXISTING, already-tested durability code. The
non-negotiable invariant: a `batch` receive must still leave every object durable
before the manifest is observable. The barrier (step 2) + manifest-last (step 3)
provide that; the per-object hint (step 1) does not. If the experiment shows the
barrier alone can't be made cheap enough without losing durability, that's a real
signal to take Option 2/3 — not to weaken the guarantee.
