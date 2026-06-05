//! Determinism gate for the synthetic-scenario generator.
//!
//! Every test fn name contains `scenario` so `cargo test -p snapdir-benches
//! scenario` selects exactly this suite. The gate proves the generator is
//! deterministic and cross-platform-stable by walking the materialized trees
//! with the SHIPPED `snapdir-core` public API and asserting:
//!
//! 1. **Twice-stable** — every GATE-tier scenario materialized into two separate
//!    temp dirs produces the SAME snapshot id (regular files + dirs only,
//!    explicit perms, fixed bytes -> identical ids).
//! 2. **Byte-determinism** — a materialized file's bytes equal
//!    `deterministic_bytes(n)`.
//! 3. **dedup invariant** — the `dedup` scenario's unique file checksums are
//!    fewer than its file count (identical content -> BLAKE3 collision).

use std::path::Path;

use snapdir_benches::{deterministic_bytes, gate_scenarios, Scenario};
use snapdir_core::{merkle::snapshot_id, walk, Blake3Hasher, PathType, WalkOptions};
use tempfile::TempDir;

/// Walks `dir` with default options + BLAKE3 and returns its snapshot id. The
/// manifest from `walk` is already sorted by path (see `compat_golden.rs` /
/// `e2e.rs`, which feed it straight into `snapshot_id`), so no extra sort.
fn snapshot_id_of(dir: &Path) -> String {
    let hasher = Blake3Hasher::new();
    let manifest = walk(dir, &WalkOptions::default(), &hasher).expect("walk scenario tree");
    snapshot_id(&manifest, &hasher)
}

/// Materializes `scenario` into a fresh temp dir and returns (id, owning dir).
fn materialize_and_id(scenario: &Scenario) -> (String, TempDir) {
    let tmp = TempDir::new().expect("create temp dir");
    scenario
        .materialize(tmp.path())
        .expect("materialize scenario");
    let id = snapshot_id_of(tmp.path());
    (id, tmp)
}

/// 1. Twice-stable: every gate scenario yields the SAME id from two independent
///    materializations (the cross-platform determinism contract).
#[test]
fn scenario_twice_stable_snapshot_ids_match() {
    let scenarios = gate_scenarios();
    assert!(!scenarios.is_empty(), "gate catalog must not be empty");
    for scenario in scenarios {
        let (id_a, _a) = materialize_and_id(&scenario);
        let (id_b, _b) = materialize_and_id(&scenario);
        assert_eq!(id_a.len(), 64, "snapshot id must be 64 hex chars: {id_a:?}");
        assert_eq!(
            id_a, id_b,
            "scenario {:?} must be twice-stable (got {id_a} vs {id_b})",
            scenario.name
        );
    }
}

/// 2. Byte-determinism: a materialized file's bytes equal `deterministic_bytes`.
///    The `dedup` scenario writes 32-byte files of identical content.
#[test]
fn scenario_file_bytes_are_deterministic() {
    let dedup = gate_scenarios()
        .into_iter()
        .find(|s| s.name == "dedup")
        .expect("catalog has a dedup scenario");
    let tmp = TempDir::new().expect("create temp dir");
    dedup.materialize(tmp.path()).expect("materialize dedup");

    let bytes = std::fs::read(tmp.path().join("dup000.bin")).expect("read dup000.bin");
    assert_eq!(
        bytes,
        deterministic_bytes(32),
        "materialized bytes must equal deterministic_bytes(32)"
    );
}

/// 3. dedup invariant: the `dedup` scenario's FILE entries share content, so the
///    count of UNIQUE checksums is strictly less than the file count.
#[test]
fn scenario_dedup_has_fewer_unique_checksums_than_files() {
    let dedup = gate_scenarios()
        .into_iter()
        .find(|s| s.name == "dedup")
        .expect("catalog has a dedup scenario");
    let tmp = TempDir::new().expect("create temp dir");
    dedup.materialize(tmp.path()).expect("materialize dedup");

    let hasher = Blake3Hasher::new();
    let manifest = walk(tmp.path(), &WalkOptions::default(), &hasher).expect("walk dedup");

    let file_checksums: Vec<&str> = manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .map(|e| e.checksum.as_str())
        .collect();
    let total_files = file_checksums.len();
    let unique: std::collections::HashSet<&str> = file_checksums.into_iter().collect();

    assert!(total_files > 1, "dedup must have multiple files");
    assert!(
        unique.len() < total_files,
        "dedup invariant: unique checksums ({}) must be < file count ({total_files})",
        unique.len()
    );
}
