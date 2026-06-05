//! Deterministic regression GATE + full-pipeline integration test.
//!
//! Every test fn name contains the substring `determinism`, so
//! `cargo test -p snapdir-benches determinism` selects exactly this suite. It is
//! a normal `cargo test` (it runs everywhere, including under
//! `cargo test --workspace`) and serves a dual role:
//!
//! 1. **Frozen-format regression gate.** For every GATE-tier scenario, the
//!    snapshot id and the structural invariants (unique object count, total
//!    bytes, manifest entry count) are pinned against the recorded
//!    [`GOLDENS`] table below. Because the corpora are regular files + dirs
//!    only, with explicit umask-independent perms, fixed deterministic bytes,
//!    and no symlinks / RNG / clock, the values are byte-for-byte reproducible
//!    and identical across platforms (a value recorded here on macOS equals the
//!    one CI computes on Linux). If the FROZEN manifest format ever deliberately
//!    changes, these goldens will (correctly) break — that is the point. See the
//!    regeneration affordance below.
//! 2. **Full local round-trip integration test.** For every GATE-tier scenario
//!    the materialized tree is pushed into a `file://` store, `sync_snapshot`ed
//!    A -> B, then fetched out of B, and the re-walked fetched tree must
//!    re-produce the SAME snapshot id == the golden. This proves the snapshot
//!    survives push -> sync -> fetch byte-identically.
//!
//! ## Intentional regeneration (when the frozen format deliberately changes)
//!
//! The [`GOLDENS`] table is a frozen contract anchor; do NOT edit it casually. A
//! change to the FROZEN manifest format (hash, line layout, merkle rule, …) will
//! deliberately break these goldens until they are regenerated. To regenerate:
//!
//! ```text
//! SNAPDIR_BENCH_REGEN=1 cargo test -p snapdir-benches determinism -- --nocapture
//! ```
//!
//! With `SNAPDIR_BENCH_REGEN=1` set, the regen test PRINTS each scenario's freshly
//! computed `(name, snapshot_id, unique_objects, total_bytes, entry_count)` to
//! stderr instead of asserting. Paste the printed rows into [`GOLDENS`], record a
//! `bump_reason` in the journal explaining why the frozen format changed, then
//! re-run WITHOUT the env var to confirm green.

use std::collections::HashSet;
use std::path::Path;

use snapdir_benches::{gate_scenarios, Scenario};
use snapdir_core::merkle::snapshot_id;
use snapdir_core::store::Store;
use snapdir_core::{walk, Blake3Hasher, ExcludeMatcher, Manifest, PathType, WalkOptions};
use snapdir_stores::{sync_snapshot, FileStore, StreamStore, TransferConfig};
use tempfile::TempDir;

/// One recorded golden row for a GATE-tier scenario. The values are produced by
/// the SHIPPED `snapdir-core` public API over the materialized tree (with the
/// scenario's `--exclude` applied when `Some`) and are deterministic +
/// cross-platform. Regenerate with `SNAPDIR_BENCH_REGEN=1` (see module docs).
struct Golden {
    /// The scenario name (matches [`Scenario::name`]).
    name: &'static str,
    /// `snapshot_id(&manifest, &hasher)` of the materialized tree.
    snapshot_id: &'static str,
    /// Distinct checksums among FILE entries (content-addressed object count).
    unique_objects: usize,
    /// Sum of FILE-entry `size` (bytes of real content).
    total_bytes: u64,
    /// `manifest.entries().len()` (files + dirs).
    entry_count: usize,
}

/// The frozen golden table — one row per GATE-tier scenario, in catalog order.
/// Recorded via `SNAPDIR_BENCH_REGEN=1` (see module docs). Cross-platform stable.
const GOLDENS: &[Golden] = &[
    Golden {
        name: "many_small",
        snapshot_id: "1a05825401b777f2c21baba7621c14387b60b059f59449b04d1cb86f3fefe2d1",
        unique_objects: 1,
        total_bytes: 384,
        entry_count: 29,
    },
    Golden {
        name: "few_large",
        snapshot_id: "5741158d9191ca1a78b2663568859b7a7b3cf9ebae81214a4e195973a2df5e9f",
        unique_objects: 1,
        total_bytes: 12288,
        entry_count: 4,
    },
    Golden {
        name: "deep_nest",
        snapshot_id: "82d66298dbd53ae435fdcf8d906b65ef22856ef0ea5a71cd0f00c705194512ad",
        unique_objects: 1,
        total_bytes: 16,
        entry_count: 14,
    },
    Golden {
        name: "wide_fanout",
        snapshot_id: "20765645a0af81bc50d6f8180792deacf88b0ef7f4a41868e410cb2d23c28c92",
        unique_objects: 1,
        total_bytes: 512,
        entry_count: 34,
    },
    Golden {
        name: "mixed",
        snapshot_id: "20e480591db13694baaad6559337f682b0e1a9e7e85ccbf25702ec612d5c2b4f",
        unique_objects: 7,
        total_bytes: 528,
        entry_count: 11,
    },
    Golden {
        name: "dedup",
        snapshot_id: "d5604b4f35fbc05f0e8604e747b1e471faa0c55fceaced01519c159b04fb1c14",
        unique_objects: 1,
        total_bytes: 256,
        entry_count: 9,
    },
    Golden {
        name: "with_excludes",
        snapshot_id: "664e00ce8d795f57f11dfd72c3f36c58d4bb07e6240a9ec70464f804b2d35667",
        unique_objects: 2,
        total_bytes: 72,
        entry_count: 5,
    },
    Golden {
        name: "edge",
        snapshot_id: "33ed645bf0884cbe82710b2837bfa156dfa9d15388f3c9407e81d6b5c330afdf",
        unique_objects: 1,
        total_bytes: 0,
        entry_count: 7,
    },
];

/// The computed structural facts of a materialized scenario tree.
struct Computed {
    snapshot_id: String,
    unique_objects: usize,
    total_bytes: u64,
    entry_count: usize,
}

/// Builds the `WalkOptions` for `scenario`, compiling its `--exclude` pattern
/// (via [`ExcludeMatcher::new`], exactly as the CLI does) when present.
fn walk_options_for(scenario: &Scenario) -> WalkOptions {
    let exclude = scenario
        .exclude
        .map(|pattern| ExcludeMatcher::new(pattern).expect("compile scenario exclude pattern"));
    WalkOptions {
        exclude,
        ..Default::default()
    }
}

/// Walks `root` with `options` + BLAKE3 and returns the manifest.
fn walk_with(root: &Path, options: &WalkOptions) -> Manifest {
    walk(root, options, &Blake3Hasher::new()).expect("walk scenario tree")
}

/// Distinct checksums among FILE entries (content-addressed object count).
fn unique_object_count(manifest: &Manifest) -> usize {
    manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .map(|e| e.checksum.as_str())
        .collect::<HashSet<_>>()
        .len()
}

/// Sum of FILE-entry sizes (bytes of real content; dir entries carry no object).
fn total_file_bytes(manifest: &Manifest) -> u64 {
    manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .map(|e| e.size)
        .sum()
}

/// Materializes `scenario` into a fresh temp dir, walks it (applying the
/// scenario's exclude), and returns the computed facts + the owning dir + the
/// manifest. The dir is returned so the caller can drive the round-trip from the
/// SAME source tree.
fn materialize_and_compute(scenario: &Scenario) -> (Computed, TempDir, Manifest) {
    let tmp = TempDir::new().expect("create temp dir");
    scenario
        .materialize(tmp.path())
        .expect("materialize scenario");
    let manifest = walk_with(tmp.path(), &walk_options_for(scenario));
    let computed = Computed {
        snapshot_id: snapshot_id(&manifest, &Blake3Hasher::new()),
        unique_objects: unique_object_count(&manifest),
        total_bytes: total_file_bytes(&manifest),
        entry_count: manifest.entries().len(),
    };
    (computed, tmp, manifest)
}

/// Looks up the recorded golden row for `name`.
fn golden_for(name: &str) -> &'static Golden {
    GOLDENS
        .iter()
        .find(|g| g.name == name)
        .unwrap_or_else(|| panic!("no golden row recorded for scenario {name:?}"))
}

/// Whether the regeneration affordance is active (`SNAPDIR_BENCH_REGEN=1`).
fn regen_mode() -> bool {
    std::env::var("SNAPDIR_BENCH_REGEN").as_deref() == Ok("1")
}

/// Regeneration affordance + golden assertion for the snapshot id and structural
/// invariants. With `SNAPDIR_BENCH_REGEN=1` set, PRINTS each scenario's freshly
/// computed row (run with `-- --nocapture`) instead of asserting, so the
/// [`GOLDENS`] table can be regenerated when the frozen format deliberately
/// changes (see module docs). Otherwise asserts every row against the table.
#[test]
fn determinism_golden_ids_and_invariants_match() {
    let scenarios = gate_scenarios();
    assert!(!scenarios.is_empty(), "gate catalog must not be empty");

    if regen_mode() {
        eprintln!("# SNAPDIR_BENCH_REGEN=1 — recorded golden rows (paste into GOLDENS):");
        for scenario in &scenarios {
            let (c, _dir, _m) = materialize_and_compute(scenario);
            eprintln!(
                "    Golden {{ name: {:?}, snapshot_id: {:?}, unique_objects: {}, total_bytes: {}, entry_count: {} }},",
                scenario.name, c.snapshot_id, c.unique_objects, c.total_bytes, c.entry_count
            );
        }
        return;
    }

    assert_eq!(
        scenarios.len(),
        GOLDENS.len(),
        "every gate scenario needs exactly one golden row (regen with SNAPDIR_BENCH_REGEN=1)"
    );

    for scenario in &scenarios {
        let (c, _dir, _m) = materialize_and_compute(scenario);
        let g = golden_for(scenario.name);

        assert_eq!(
            c.snapshot_id.len(),
            64,
            "snapshot id must be 64 hex chars for {:?}",
            scenario.name
        );
        assert_eq!(
            c.snapshot_id, g.snapshot_id,
            "snapshot id regressed for {:?} (regen with SNAPDIR_BENCH_REGEN=1)",
            scenario.name
        );
        assert_eq!(
            c.unique_objects, g.unique_objects,
            "unique object count regressed for {:?}",
            scenario.name
        );
        assert_eq!(
            c.total_bytes, g.total_bytes,
            "total bytes regressed for {:?}",
            scenario.name
        );
        assert_eq!(
            c.entry_count, g.entry_count,
            "manifest entry count regressed for {:?}",
            scenario.name
        );
    }
}

/// The `dedup` scenario specifically: identical content -> BLAKE3 collision, so
/// the unique object count is strictly less than the FILE count.
#[test]
fn determinism_dedup_has_fewer_objects_than_files() {
    let dedup = gate_scenarios()
        .into_iter()
        .find(|s| s.name == "dedup")
        .expect("catalog has a dedup scenario");
    let (_c, _dir, manifest) = materialize_and_compute(&dedup);

    let file_count = manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .count();
    let unique = unique_object_count(&manifest);

    assert!(file_count > 1, "dedup must have multiple files");
    assert!(
        unique < file_count,
        "dedup invariant: unique objects ({unique}) must be < file count ({file_count})"
    );
}

/// The `with_excludes` scenario specifically: the `--exclude` pattern must
/// actually drop the `skip/` subtree, so the with-exclude entry count is
/// strictly less than the without-exclude count.
#[test]
fn determinism_excludes_actually_drop_a_subtree() {
    let scenario = gate_scenarios()
        .into_iter()
        .find(|s| s.name == "with_excludes")
        .expect("catalog has a with_excludes scenario");
    assert!(
        scenario.exclude.is_some(),
        "with_excludes must carry an exclude pattern"
    );

    let tmp = TempDir::new().expect("create temp dir");
    scenario
        .materialize(tmp.path())
        .expect("materialize with_excludes");

    let with = walk_with(tmp.path(), &walk_options_for(&scenario));
    let without = walk_with(tmp.path(), &WalkOptions::default());

    assert!(
        with.entries().len() < without.entries().len(),
        "exclude must drop a subtree: with-exclude entries ({}) must be < without ({})",
        with.entries().len(),
        without.entries().len()
    );
}

/// Full local round-trip: for every GATE-tier scenario, push the materialized
/// tree into store A, `sync_snapshot` A -> B, fetch out of B, then re-walk the
/// fetched tree (NO exclude — the excluded paths were never in the manifest, so
/// the fetched tree already omits them) and assert the re-id == the same id ==
/// the recorded golden. This proves the snapshot survives push -> sync -> fetch
/// byte-identically across the store pipeline.
#[test]
fn determinism_full_local_round_trip_preserves_snapshot_id() {
    if regen_mode() {
        // No goldens to assert against during regeneration.
        return;
    }
    let config = TransferConfig::new(4, None);

    for scenario in gate_scenarios() {
        let (computed, src_dir, manifest) = materialize_and_compute(&scenario);
        let id = computed.snapshot_id.clone();
        assert_eq!(
            id,
            golden_for(scenario.name).snapshot_id,
            "precondition: source id must equal the golden for {:?}",
            scenario.name
        );

        let from_store_dir = TempDir::new().expect("create source store dir");
        let to_store_dir = TempDir::new().expect("create destination store dir");
        let dest_dir = TempDir::new().expect("create fetch dest dir");

        let a = FileStore::from_root(from_store_dir.path());
        let b = FileStore::from_root(to_store_dir.path());

        a.push(&manifest, src_dir.path()).expect("push into A");
        let report = sync_snapshot(
            &a as &(dyn StreamStore + Sync),
            &b as &(dyn StreamStore + Sync),
            &id,
            &config,
            false,
            None,
        )
        .expect("sync A -> B");
        assert!(!report.dry_run, "round-trip sync must be a real copy");

        let m2 = b.get_manifest(&id).expect("B has the synced manifest");
        b.fetch_files(&m2, dest_dir.path())
            .expect("fetch files out of B");

        // Re-walk the fetched tree WITHOUT an exclude: excluded paths were never
        // in the manifest, so the fetched tree already omits that subtree.
        let round_tripped = walk_with(dest_dir.path(), &WalkOptions::default());
        let round_trip_id = snapshot_id(&round_tripped, &Blake3Hasher::new());

        assert_eq!(
            round_trip_id, id,
            "round-trip id must equal the source id == the golden for {:?}",
            scenario.name
        );
    }
}
