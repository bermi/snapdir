# snapdir benches

Criterion **wall-clock** microbenchmarks for snapdir's perf-critical paths. These
exist to help you **make an informed decision** about a change's performance — they
are NOT a hard CI gate (wall-clock numbers are noisy and machine-dependent). The
deterministic, machine-independent perf **gate** (instruction counts via
iai-callgrind) lives alongside them in the `iai_hot` bench — see
[Instruction-count perf gate](#instruction-count-perf-gate-iai-callgrind) below.

All scenarios come from the crate's `bench_scenarios()` catalog (`src/lib.rs`) — a
single source of truth shared with the determinism gate and the upcoming perf gate.

## The benches

- **`hot_paths`** (criterion, wall-clock) — in-process micro hot paths: `hash`
  (BLAKE3 over buffer sizes), `walk`, and `manifest` emit/parse round-trip.
- **`pipeline`** (criterion, wall-clock) — the full local pipeline over the
  BENCH-tier scenarios: `walk+hash`, `snapshot_id`, `stage/push`,
  `checkout/fetch`, and `sync` (A→B). push/fetch/sync use `iter_batched` with a
  fresh empty store/dest per iteration so each timed run does real copy work (the
  content-addressed store would otherwise skip already-present objects).
  Throughput is reported in bytes (MB/s) where meaningful.
- **`iai_hot`** (iai-callgrind, **instruction counts** — the hard perf GATE) —
  `blake3`, `walk`, and `snapshot_id` over FIXED TINY deterministic inputs. See
  [Instruction-count perf gate](#instruction-count-perf-gate-iai-callgrind).

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

## Instruction-count perf gate (iai-callgrind)

The `iai_hot` bench is the **deterministic, machine-independent perf GATE**.
Instead of wall-clock time (noisy, host-dependent) it measures **CPU instruction
counts** (`Ir`) and `EstimatedCycles` under valgrind/callgrind, which are stable
across runs and machines. It runs the three perf-critical hot paths — `blake3`
(`hash_hex`), `walk` (`walk()` + manifest build), and `snapshot_id` — over **fixed
tiny deterministic inputs** (from the same `gate_scenarios()` / `deterministic_bytes`
source of truth), so the counts don't depend on the host. The bench **FAILS** when
`Ir` or `EstimatedCycles` regress by more than **5%** versus the saved baseline
(wired via `Callgrind::soft_limits` in `benches/benches/iai_hot.rs`).

The iai-callgrind dev-dependency is pinned EXACTLY (`iai-callgrind = "=0.16.1"` in
`Cargo.toml`); the matching `iai-callgrind-runner` MUST be installed at the SAME
version or it refuses to run.

### Run it on Linux (native valgrind)

Compiling needs nothing special and works everywhere (this is the gate's
compile-only check on macOS):

```sh
cargo bench -p snapdir-benches --bench iai_hot --no-run
```

The actual measurement needs valgrind (Linux only):

```sh
# Debian/Ubuntu: apt-get install -y valgrind
cargo install iai-callgrind-runner --version 0.16.1 --locked
cargo bench -p snapdir-benches --bench iai_hot
```

### Run it on macOS (via Docker)

macOS has no native valgrind, so run the gate inside a pinned Linux image that
matches CI. Requires **Docker** (daemon running). From the repo root:

```sh
bash benches/run-iai-docker.sh
```

The script is pinned but overridable: `RUST_IMAGE` (default
`rust:1.91-slim-bookworm`, MSRV-compatible) and `IAI_VERSION` (default `0.16.1`,
which MUST equal the `Cargo.toml` pin). It installs valgrind + the version-matched
runner in the container, mounts the repo, and runs `--bench iai_hot`.

### Baselines

iai-callgrind writes its baseline data under `target/iai/` (in `target/`, which is
**not** committed). On a fresh checkout the first run has no baseline to compare
against — it just records one. In CI the authoritative baseline is restored from a
saved artifact/cache before the run, and the gate compares the new counts against
it; a >5% `Ir`/`EstimatedCycles` regression fails the job.

To intentionally refresh the baseline (e.g. after an accepted, justified change in
instruction counts), re-run the bench to overwrite the stored baseline, or use a
named/saved baseline:

```sh
# Record/overwrite the default baseline:
cargo bench -p snapdir-benches --bench iai_hot

# Or save under an explicit name and compare against it later:
cargo bench -p snapdir-benches --bench iai_hot -- --save-baseline main
cargo bench -p snapdir-benches --bench iai_hot -- --baseline main
```

Because the baseline lives under `target/`, nothing about it is committed to the
repo; CI owns persisting/restoring it. A deliberate baseline change is therefore a
CI/operational action, not a tracked-file edit.
