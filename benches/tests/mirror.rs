//! Sanity gate for the `pipeline` bench's `mirror` (`checkout --delete`) arms.
//!
//! The `mirror` criterion bench's headline is **zero-extraneous mirror ≈ plain
//! checkout**: when nothing in the destination is extraneous, the exact-mirror
//! prune (dest walk + `prune_set` + deletions) adds only negligible cost on top
//! of `fetch_files`. The wall-clock bench *quantifies* that; this test pins it
//! *qualitatively* using the SHIPPED `snapdir-core` public API:
//!
//! 1. A zero-extraneous prune set is EMPTY — the prune walk deletes nothing.
//! 2. After the (no-op) prune, the materialized dest is byte-for-byte equal to a
//!    plain checkout.
//! 3. With planted extraneous files, the prune set is exactly those files (so
//!    the `with_extraneous` arm measures real deletion work) and the dest again
//!    equals a plain checkout afterward.
//!
//! Every test fn name contains `mirror` so `cargo test -p snapdir-benches mirror`
//! selects exactly this suite. Runs natively — no valgrind.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use snapdir_benches::{bench_scenarios, deterministic_bytes, Scenario};
use snapdir_core::mirror::{prune_set, DestEntry};
use snapdir_core::store::Store;
use snapdir_core::{walk, Blake3Hasher, Manifest, PathType, WalkOptions};
use snapdir_stores::{FileStore, MaterializeMode};
use tempfile::TempDir;

fn materialize(scenario: &Scenario) -> TempDir {
    let tmp = TempDir::new().expect("create temp dir");
    scenario
        .materialize(tmp.path())
        .expect("materialize scenario");
    tmp
}

fn walk_manifest(root: &Path) -> Manifest {
    walk(root, &WalkOptions::default(), &Blake3Hasher::new()).expect("walk scenario")
}

fn fresh_dir() -> TempDir {
    TempDir::new().expect("create scratch dir")
}

/// Walks `dir` into the `./`-prefixed [`DestEntry`] listing `prune_set` expects
/// (dirs end `/`), relative to `root`. Mirrors the bench's `collect_dest_entries`.
fn collect_dest_entries(root: &Path, dir: &Path, out: &mut Vec<DestEntry>) {
    for entry in std::fs::read_dir(dir).expect("read dest dir") {
        let entry = entry.expect("dest dir entry");
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).expect("stat dest entry");
        let rel = path
            .strip_prefix(root)
            .expect("dest entry under root")
            .to_string_lossy()
            .into_owned();
        if meta.file_type().is_dir() {
            out.push(DestEntry::new(format!("./{rel}/"), PathType::Directory));
            collect_dest_entries(root, &path, out);
        } else {
            out.push(DestEntry::new(format!("./{rel}"), PathType::File));
        }
    }
}

/// The exact-mirror prune step (faithful copy of the bench's `mirror_prune`).
fn mirror_prune(manifest: &Manifest, dest: &Path) {
    let mut entries = Vec::new();
    collect_dest_entries(dest, dest, &mut entries);
    let prune = prune_set(manifest, &entries, &[]);
    for path in &prune {
        let rel = path.strip_prefix("./").unwrap_or(path);
        let rel = rel.strip_suffix('/').unwrap_or(rel);
        let target = dest.join(rel);
        let meta = match std::fs::symlink_metadata(&target) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => panic!("stat {}: {e}", target.display()),
        };
        if meta.file_type().is_dir() {
            std::fs::remove_dir(&target).expect("remove extraneous dir");
        } else {
            std::fs::remove_file(&target).expect("remove extraneous path");
        }
    }
}

/// A sorted snapshot of the dest's `(path, type)` keys.
fn dest_keys(root: &Path) -> Vec<DestEntry> {
    let mut v = Vec::new();
    collect_dest_entries(root, root, &mut v);
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

fn bench_scenario(name: &str) -> Scenario {
    bench_scenarios()
        .into_iter()
        .find(|s| s.name == name)
        .expect("named bench scenario exists")
}

/// A zero-extraneous mirror prunes NOTHING and leaves the dest identical to a
/// plain checkout — the `mirror` bench's `zero_extraneous ≈ checkout` headline.
#[test]
fn zero_extraneous_mirror_equals_plain_checkout() {
    let scenario = bench_scenario("many_small_bench");
    let source = materialize(&scenario);
    let manifest = walk_manifest(source.path());

    let store_dir = fresh_dir();
    let store = FileStore::from_root(store_dir.path());
    store.push(&manifest, source.path()).expect("seed push");

    // Plain checkout.
    let plain = fresh_dir();
    store
        .fetch_files_with_mode(&manifest, plain.path(), MaterializeMode::Auto)
        .expect("plain checkout");
    let plain_keys = dest_keys(plain.path());

    // Mirror checkout (fetch + prune) with NOTHING extraneous.
    let mirrored = fresh_dir();
    store
        .fetch_files_with_mode(&manifest, mirrored.path(), MaterializeMode::Auto)
        .expect("mirror checkout");

    // The prune set is empty: the prune walk does no deletion work.
    let mut entries = Vec::new();
    collect_dest_entries(mirrored.path(), mirrored.path(), &mut entries);
    let prune = prune_set(&manifest, &entries, &[]);
    assert!(
        prune.is_empty(),
        "zero-extraneous mirror must prune nothing, got {prune:?}"
    );

    mirror_prune(&manifest, mirrored.path());
    assert_eq!(
        plain_keys,
        dest_keys(mirrored.path()),
        "zero-extraneous mirror must equal a plain checkout"
    );
}

/// With planted extraneous files, the prune set is EXACTLY those files and the
/// dest equals a plain checkout afterward — the `with_extraneous` arm's contract.
#[test]
fn mirror_prunes_exactly_the_planted_extraneous() {
    // A small BENCH scenario for a fast, representative tree.
    let scenario = bench_scenario("deep_nest_bench");
    let source = materialize(&scenario);
    let manifest = walk_manifest(source.path());

    let store_dir = fresh_dir();
    let store = FileStore::from_root(store_dir.path());
    store.push(&manifest, source.path()).expect("seed push");

    let plain = fresh_dir();
    store
        .fetch_files_with_mode(&manifest, plain.path(), MaterializeMode::Auto)
        .expect("plain checkout");
    let plain_keys = dest_keys(plain.path());

    let mirrored = fresh_dir();
    store
        .fetch_files_with_mode(&manifest, mirrored.path(), MaterializeMode::Auto)
        .expect("mirror checkout");

    // Plant a handful of extraneous files in a fresh subdir.
    let extra = mirrored.path().join("extra");
    std::fs::create_dir_all(&extra).expect("mkdir extra");
    let content = deterministic_bytes(64);
    let mut planted = Vec::new();
    for i in 0..8u32 {
        let target = extra.join(format!("stale{i:03}.bin"));
        std::fs::write(&target, &content).expect("write extraneous");
        std::fs::set_permissions(&target, PermissionsExt::from_mode(0o644)).expect("chmod");
        planted.push(format!("./extra/stale{i:03}.bin"));
    }

    let mut entries = Vec::new();
    collect_dest_entries(mirrored.path(), mirrored.path(), &mut entries);
    let mut prune = prune_set(&manifest, &entries, &[]);
    prune.sort();
    let mut expected = planted.clone();
    expected.push("./extra/".to_owned());
    expected.sort();
    assert_eq!(
        prune, expected,
        "prune set must be exactly the planted extraneous paths (+ their dir)"
    );

    mirror_prune(&manifest, mirrored.path());
    assert_eq!(
        plain_keys,
        dest_keys(mirrored.path()),
        "after pruning the extraneous files the dest must equal a plain checkout"
    );
}
