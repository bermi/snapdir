//! snapdir deterministic INSTRUCTION-COUNT perf gate (iai-callgrind).
//!
//! Unlike the wall-clock criterion suites (`hot_paths`, `pipeline`), this bench
//! measures **CPU instruction counts** (`Ir`) and `EstimatedCycles` under
//! valgrind/callgrind. Those counts are *deterministic* — they don't depend on
//! the host's clock, load, or microarchitecture — so they make a reliable hard
//! perf GATE: a run FAILS when a metric regresses past the configured soft limit
//! (see [`callgrind_5pct`]) versus the saved baseline.
//!
//! It exercises the same shipped `snapdir-core` public API as the criterion
//! benches and **measures only** — it never touches `crates/**` and changes no
//! output bytes.
//!
//! ## Why FIXED TINY deterministic inputs
//!
//! callgrind re-runs each benched function under instrumentation, so for the
//! counts to be stable the *input* must be fixed and deterministic. Every group
//! here uses a small, constant input built in a `setup` (which is NOT counted —
//! iai-callgrind only counts the benched function body), drawn from the crate's
//! single source of truth ([`snapdir_benches`]): [`deterministic_bytes`] (a fixed
//! byte ramp, no RNG) and a single small GATE-tier [`Scenario`].
//!
//! Three groups, mirroring the perf-critical paths:
//!
//! 1. `blake3`      — `Blake3Hasher::hash_hex` over a fixed small buffer.
//! 2. `walk`        — `walk()` over a small fixed scenario materialized ONCE in
//!    `setup` (the materialization is not counted; only the walk is).
//! 3. `snapshot_id` — `snapshot_id()` over a small pre-built manifest.

use iai_callgrind::{
    library_benchmark, library_benchmark_group, main, Callgrind, EventKind, LibraryBenchmarkConfig,
};
use snapdir_benches::{deterministic_bytes, gate_scenarios, Scenario};
use snapdir_core::merkle::snapshot_id;
use snapdir_core::{walk, Blake3Hasher, Hasher, Manifest, WalkOptions};
use std::hint::black_box;
use std::path::PathBuf;
use tempfile::TempDir;

/// Fixed tiny hash-input size (bytes). Small + constant so callgrind's
/// instruction counts are byte-stable across runs and machines.
const HASH_INPUT_LEN: usize = 4 * 1024;

/// The small GATE-tier scenario used by the `walk` / `snapshot_id` groups,
/// looked up by name. `mixed` — a few files + a couple of nested subtrees — is
/// tiny, deterministic, and representative without making the counts depend on a
/// large corpus.
const WALK_SCENARIO: &str = "mixed";

/// Resolves a GATE-tier scenario by name (the single source of truth lives in
/// [`snapdir_benches::gate_scenarios`]).
fn scenario_by_name(name: &str) -> Scenario {
    gate_scenarios()
        .into_iter()
        .find(|s| s.name == name)
        .expect("named gate scenario exists")
}

/// Soft-limit config: FAIL the bench when `Ir` (instructions retired) or
/// `EstimatedCycles` regress by more than 5% versus the saved baseline. Both are
/// deterministic callgrind metrics, so a 5% headroom catches real algorithmic
/// regressions while tolerating tiny, unavoidable codegen jitter.
fn callgrind_5pct() -> LibraryBenchmarkConfig {
    LibraryBenchmarkConfig::default()
        .tool(
            Callgrind::default()
                .soft_limits([(EventKind::Ir, 5f64), (EventKind::EstimatedCycles, 5f64)]),
        )
        .clone()
}

// ---------------------------------------------------------------------------
// 1. blake3 hash hot path.
// ---------------------------------------------------------------------------

/// Builds the fixed tiny hash input (NOT counted — runs in `setup`). Takes the
/// length as an explicit arg so the `#[bench]` `args = (...)` form drives it.
fn setup_hash_input(len: usize) -> Vec<u8> {
    deterministic_bytes(len)
}

// `Blake3Hasher::hash_hex` over a fixed small buffer. Only this body is counted.
#[library_benchmark]
#[bench::fixed(args = (HASH_INPUT_LEN,), setup = setup_hash_input)]
fn bench_blake3(buf: Vec<u8>) -> String {
    let hasher = Blake3Hasher::new();
    black_box(hasher.hash_hex(black_box(&buf)))
}

// ---------------------------------------------------------------------------
// 2. walk + manifest build hot path.
// ---------------------------------------------------------------------------

/// Materializes the named fixed scenario into a fresh tempdir ONCE (NOT counted —
/// runs in `setup`). Returns the owning `TempDir` (kept alive so the tree isn't
/// dropped before the walk) and its root path.
fn setup_walk_tree(name: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("create temp dir");
    scenario_by_name(name)
        .materialize(tmp.path())
        .expect("materialize scenario");
    let root = tmp.path().to_path_buf();
    (tmp, root)
}

// `walk()` (BLAKE3, default options) over the pre-materialized fixed tree. Only
// the walk + manifest build is counted; materialization happened in `setup`.
#[library_benchmark]
#[bench::fixed(args = (WALK_SCENARIO,), setup = setup_walk_tree)]
fn bench_walk(tree: (TempDir, PathBuf)) -> Manifest {
    let (_tmp, root) = tree;
    let manifest = walk(
        black_box(&root),
        black_box(&WalkOptions::default()),
        black_box(&Blake3Hasher::new()),
    )
    .expect("walk fixed scenario");
    black_box(manifest)
}

// ---------------------------------------------------------------------------
// 3. snapshot_id hot path.
// ---------------------------------------------------------------------------

/// Builds a small pre-walked manifest ONCE (NOT counted — runs in `setup`). The
/// `TempDir` is dropped here (the manifest is in-memory and self-contained), so
/// only the manifest is handed to the benched fn.
fn setup_manifest(name: &str) -> Manifest {
    let tmp = TempDir::new().expect("create temp dir");
    scenario_by_name(name)
        .materialize(tmp.path())
        .expect("materialize scenario");
    walk(tmp.path(), &WalkOptions::default(), &Blake3Hasher::new()).expect("walk for manifest")
}

// `snapshot_id()` over a small pre-built manifest. Only this body is counted.
#[library_benchmark]
#[bench::fixed(args = (WALK_SCENARIO,), setup = setup_manifest)]
fn bench_snapshot_id(manifest: Manifest) -> String {
    let hasher = Blake3Hasher::new();
    black_box(snapshot_id(black_box(&manifest), &hasher))
}

// ---------------------------------------------------------------------------
// Groups + harness. The 5% Ir / EstimatedCycles soft limit is wired in via the
// group `config` so every benched fn inherits the regression gate.
// ---------------------------------------------------------------------------

library_benchmark_group!(
    name = hot;
    config = callgrind_5pct();
    benchmarks = bench_blake3, bench_walk, bench_snapshot_id
);

main!(library_benchmark_groups = hot);
