# snapdir benches

Criterion **wall-clock** microbenchmarks for snapdir's perf-critical paths. These
exist to help you **make an informed decision** about a change's performance — they
are NOT a hard CI gate (wall-clock numbers are noisy and machine-dependent). The
deterministic, machine-independent perf **gate** (BLAKE3 instruction counts via
iai-callgrind) is coming in a later gate; until then, treat these as advisory.

All scenarios come from the crate's `bench_scenarios()` catalog (`src/lib.rs`) — a
single source of truth shared with the determinism gate and the upcoming perf gate.

## The two benches

- **`hot_paths`** — in-process micro hot paths: `hash` (BLAKE3 over buffer sizes),
  `walk`, and `manifest` emit/parse round-trip.
- **`pipeline`** — the full local pipeline over the BENCH-tier scenarios:
  `walk+hash`, `snapshot_id`, `stage/push`, `checkout/fetch`, and `sync` (A→B).
  push/fetch/sync use `iter_batched` with a fresh empty store/dest per iteration so
  each timed run does real copy work (the content-addressed store would otherwise
  skip already-present objects). Throughput is reported in bytes (MB/s) where
  meaningful.

## Baseline workflow (compare a change)

Capture a baseline before your change, then compare after:

```sh
# 1. On the unchanged tree, save a named baseline:
cargo bench -p snapdir-benches --bench pipeline -- --save-baseline before

# 2. Make your change, rebuild, then compare against the saved baseline:
cargo bench -p snapdir-benches --bench pipeline -- --baseline before
```

Criterion prints the per-benchmark delta (e.g. `change: -4.2% (improved)`), so you
can see whether a change helped or regressed each pipeline stage.

The same workflow applies to `hot_paths` (swap `--bench pipeline` for
`--bench hot_paths`). Run all benches with a plain `cargo bench -p snapdir-benches`.

For a quick smoke run (short, imprecise) while iterating:

```sh
cargo bench -p snapdir-benches --bench pipeline -- \
  --warm-up-time 1 --measurement-time 2 --sample-size 10
```
