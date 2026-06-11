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
use snapdir_stores::{
    read_pack, write_pack, Durability, FileSink, FileStore, PackReadReport, StreamStore,
};
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
// 4. SNAPPACK receive hot path (read_pack into a FileSink).
// ---------------------------------------------------------------------------

/// Fixed receive workload: 64 objects × 1KiB each (v1 wire). Small + constant
/// so callgrind's instruction counts stay byte-stable.
const PACK_OBJECTS: usize = 64;
const PACK_OBJECT_BYTES: usize = 1024;

/// A pre-encoded v1 pack + the fresh dest store dir it will be read into (NOT
/// counted — runs in `setup`). The source objects are distinct (`deterministic_bytes`
/// over a per-object length) so each has a unique content-address. The returned
/// `TempDir` owns the dest dir (kept alive so the filed objects aren't dropped
/// before the read completes); only `read_pack` is counted.
fn setup_read_pack() -> (TempDir, Vec<u8>) {
    let hasher = Blake3Hasher::new();

    // Source store (its own tempdir) seeded with PACK_OBJECTS distinct objects.
    let src_dir = TempDir::new().expect("create source store dir");
    let src = FileStore::from_root(src_dir.path());
    let mut ids = Vec::with_capacity(PACK_OBJECTS);
    for i in 0..PACK_OBJECTS {
        // Vary the length by a few bytes per object so addresses differ.
        let bytes = deterministic_bytes(PACK_OBJECT_BYTES + i);
        let checksum = hasher.hash_hex(&bytes);
        src.put_object(&checksum, bytes)
            .expect("seed source object");
        ids.push(checksum);
    }

    // Encode the whole set into one in-memory v1 pack (no manifest).
    let mut pack = Vec::new();
    write_pack(&src, &ids, None, &mut pack).expect("write_pack into Vec");

    // Fresh, empty dest dir for the read. `src_dir` can drop — the pack bytes
    // are already in memory.
    let dest_dir = TempDir::new().expect("create dest store dir");
    (dest_dir, pack)
}

// `read_pack` of a fixed 64×1KiB v1 pack into a fresh FileSink (durability Off).
// Only the receive (parse + verify + file) is counted; encoding + seeding ran
// in `setup`. The first run sets the baseline.
#[library_benchmark]
#[bench::fixed(setup = setup_read_pack)]
fn bench_read_pack(input: (TempDir, Vec<u8>)) -> PackReadReport {
    let (dest_dir, pack) = input;
    let store = FileStore::from_root(dest_dir.path());
    let mut sink = FileSink::new(&store).with_durability(Durability::Off);
    let report = read_pack(black_box(pack.as_slice()), black_box(&mut sink)).expect("read_pack");
    // Keep the dest dir alive until the read (and any filing) completes.
    drop(dest_dir);
    black_box(report)
}

// ---------------------------------------------------------------------------
// Groups + harness. The 5% Ir / EstimatedCycles soft limit is wired in via the
// group `config` so every benched fn inherits the regression gate.
// ---------------------------------------------------------------------------

library_benchmark_group!(
    name = hot;
    config = callgrind_5pct();
    benchmarks = bench_blake3, bench_walk, bench_snapshot_id, bench_read_pack
);

main!(library_benchmark_groups = hot);
