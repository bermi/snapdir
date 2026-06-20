//! snapdir full-pipeline wall-clock benchmarks.
//!
//! Criterion **wall-clock** benches over the BENCH-tier [`bench_scenarios`]
//! corpora, covering the whole local pipeline a `snapdir` run exercises:
//!
//! 1. `walk+hash`     — [`walk`] (BLAKE3) over each materialized BENCH scenario.
//! 2. `snapshot_id`   — [`snapshot_id`](snapdir_core::merkle::snapshot_id) over a
//!    pre-walked manifest.
//! 3. `stage/push`    — [`Store::push`] into a FRESH store dir per iteration.
//! 4. `checkout/fetch`— [`Store::fetch_files`] into a FRESH dest dir per iteration.
//! 5. `sync`          — [`sync_snapshot`] A→B into a FRESH `to` store per iteration.
//!
//! These benches **measure only** — they never touch `crates/**`, and nothing
//! here changes output bytes. They exercise the shipped `snapdir-core` /
//! `snapdir-stores` public API.
//!
//! ## Measuring real work, not skip-if-present
//!
//! snapdir's store is content-addressed: `push` skips already-present objects,
//! `fetch_files` skips present-and-verified dest files, and `sync_snapshot`
//! early-returns when the dest manifest already exists. A naive `b.iter(|| push)`
//! would therefore time a no-op after the first run. So push/fetch/sync use
//! [`Bencher::iter_batched`] with [`BatchSize::PerIteration`]: the SOURCE tree
//! and its manifest are built ONCE (long-lived [`TempDir`] + a single `walk`),
//! but the setup closure hands each timed iteration a FRESH empty store/dest/`to`
//! so every iteration does the full copy. Only the push/fetch/sync call itself is
//! timed.
//!
//! Wall-clock benches are noisy; for the deterministic, machine-independent perf
//! GATE (instruction counts) see the upcoming iai-callgrind bench. This suite is
//! the "make an informed decision" tool, NOT a hard gate.

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use snapdir_benches::{bench_scenarios, deterministic_bytes, Scenario};
use snapdir_core::merkle::snapshot_id;
use snapdir_core::mirror::{prune_set, DestEntry};
use snapdir_core::store::Store;
use snapdir_core::{walk, Blake3Hasher, Manifest, PathType, WalkOptions};
use snapdir_stores::{sync_snapshot, FileStore, MaterializeMode, TransferConfig};
use std::hint::black_box;
use std::path::Path;
use tempfile::TempDir;

/// Materializes a scenario into a fresh long-lived `TempDir` (cleaned on `Drop`).
fn materialize(scenario: &Scenario) -> TempDir {
    let tmp = TempDir::new().expect("create temp dir");
    scenario
        .materialize(tmp.path())
        .expect("materialize scenario");
    tmp
}

/// Walks `root` with default options + BLAKE3, returning the manifest.
fn walk_manifest(root: &Path) -> Manifest {
    walk(root, &WalkOptions::default(), &Blake3Hasher::new()).expect("walk scenario")
}

/// Sum of File-entry sizes in a manifest — the meaningful byte count for
/// throughput (dir entries carry no object).
fn manifest_bytes(manifest: &Manifest) -> u64 {
    manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .map(|e| e.size)
        .sum()
}

/// A fresh, empty scratch directory (its own `TempDir`).
fn fresh_dir() -> TempDir {
    TempDir::new().expect("create scratch dir")
}

/// 1. `walk+hash`: build each corpus once, then time `walk()` (BLAKE3) over it.
fn bench_walk_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("walk+hash");
    for scenario in bench_scenarios() {
        let tree = materialize(&scenario);
        let manifest = walk_manifest(tree.path());
        group.throughput(Throughput::Bytes(manifest_bytes(&manifest)));
        group.bench_with_input(
            BenchmarkId::from_parameter(scenario.name),
            tree.path(),
            |b, root| {
                b.iter(|| black_box(walk_manifest(black_box(root))));
            },
        );
    }
    group.finish();
}

/// 2. `snapshot_id`: pre-walk each corpus once, then time `snapshot_id` over the
///    in-memory manifest.
fn bench_snapshot_id(c: &mut Criterion) {
    let hasher = Blake3Hasher::new();
    let mut group = c.benchmark_group("snapshot_id");
    for scenario in bench_scenarios() {
        let tree = materialize(&scenario);
        let manifest = walk_manifest(tree.path());
        group.bench_with_input(
            BenchmarkId::from_parameter(scenario.name),
            &manifest,
            |b, m| {
                b.iter(|| black_box(snapshot_id(black_box(m), &hasher)));
            },
        );
    }
    group.finish();
}

/// 3. `stage/push`: build the source tree + manifest ONCE, then each iteration
///    pushes into a FRESH empty store dir so real copy work is timed.
fn bench_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("stage/push");
    for scenario in bench_scenarios() {
        let source = materialize(&scenario);
        let manifest = walk_manifest(source.path());
        group.throughput(Throughput::Bytes(manifest_bytes(&manifest)));
        group.bench_with_input(
            BenchmarkId::from_parameter(scenario.name),
            &manifest,
            |b, manifest| {
                b.iter_batched(
                    fresh_dir,
                    |store_dir| {
                        let store = FileStore::from_root(store_dir.path());
                        store
                            .push(black_box(manifest), source.path())
                            .expect("push");
                        // Keep `store_dir` alive until the copy finishes.
                        store_dir
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

/// 4. `checkout/fetch`: push into a store ONCE, then each iteration fetches into
///    a FRESH empty dest dir so real copy work is timed.
fn bench_fetch(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkout/fetch");
    for scenario in bench_scenarios() {
        let source = materialize(&scenario);
        let manifest = walk_manifest(source.path());
        let store_dir = fresh_dir();
        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, source.path()).expect("seed push");
        group.throughput(Throughput::Bytes(manifest_bytes(&manifest)));
        group.bench_with_input(
            BenchmarkId::from_parameter(scenario.name),
            &manifest,
            |b, manifest| {
                b.iter_batched(
                    fresh_dir,
                    |dest_dir| {
                        store
                            .fetch_files(black_box(manifest), dest_dir.path())
                            .expect("fetch");
                        dest_dir
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

/// 5. `sync`: pre-populate the `from` store ONCE, then each iteration syncs the
///    snapshot into a FRESH empty `to` store so real copy work is timed.
fn bench_sync(c: &mut Criterion) {
    let hasher = Blake3Hasher::new();
    let config = TransferConfig::new(4, None);
    let mut group = c.benchmark_group("sync");
    for scenario in bench_scenarios() {
        let source = materialize(&scenario);
        let manifest = walk_manifest(source.path());
        let id = snapshot_id(&manifest, &hasher);

        let from_dir = fresh_dir();
        let from = FileStore::from_root(from_dir.path());
        from.push(&manifest, source.path()).expect("seed from push");

        group.throughput(Throughput::Bytes(manifest_bytes(&manifest)));
        group.bench_with_input(BenchmarkId::from_parameter(scenario.name), &id, |b, id| {
            b.iter_batched(
                fresh_dir,
                |to_dir| {
                    let to = FileStore::from_root(to_dir.path());
                    let report = sync_snapshot(&from, &to, black_box(id), &config, false, None)
                        .expect("sync");
                    black_box(report);
                    to_dir
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 6. `mirror`: checkout `--delete` exact-mirror cost (Phase 32).
//
// The CLI's `checkout --delete` = `fetch_files` (materialize) THEN an exact-mirror
// prune: walk the dest, compute `prune_set` (paths present in the dest but absent
// from the manifest), and unlink them deepest-first. The store's `fetch_files`
// is the only public copy-in; the prune is `snapdir_core::mirror::prune_set` +
// std-fs deletions, replicated here verbatim from the CLI's private `prune_dest`
// so the bench measures the SHIPPED mirror path without touching `crates/**`.
//
// Headline asserted below: a ZERO-extraneous mirror ≈ a plain checkout — when
// nothing in the dest is extraneous, the prune walk + set-difference add only
// negligible cost on top of `fetch_files` (no deletions happen).
//
// Phase-27 Darwin caveat: macOS fsync inflation is a Darwin artifact, NOT the
// acceptance number. The headline (mirror ≈ checkout) is validated on Linux/CI;
// the macOS wall-clock deltas here are indicative only.
// ---------------------------------------------------------------------------

/// How many extraneous files to plant in the dest for the `with_extraneous`
/// mirror arm (the `zero_extraneous` arm plants none).
const MIRROR_EXTRANEOUS: usize = 256;

/// Walks `dest` into the `./`-prefixed [`DestEntry`] listing `prune_set` expects
/// (dirs end `/`), skipping the root. Mirrors the CLI's private
/// `collect_dest_entries` so the bench drives the exact set-difference input.
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

/// Plants `n` extraneous files (absent from any manifest) into `dest`, split
/// across the root and two extra subdirs. Deterministic content (no RNG).
fn plant_extraneous(dest: &Path, n: usize) {
    use std::os::unix::fs::PermissionsExt;
    let extra_a = dest.join("extra_a");
    let extra_b = dest.join("extra_b");
    std::fs::create_dir_all(&extra_a).expect("mkdir extra_a");
    std::fs::create_dir_all(&extra_b).expect("mkdir extra_b");
    let content = deterministic_bytes(64);
    for i in 0..n {
        let target = match i % 3 {
            0 => dest.join(format!("stale{i:05}.bin")),
            1 => extra_a.join(format!("stale{i:05}.bin")),
            _ => extra_b.join(format!("stale{i:05}.bin")),
        };
        std::fs::write(&target, &content).expect("write extraneous file");
        std::fs::set_permissions(&target, PermissionsExt::from_mode(0o644))
            .expect("chmod extraneous file");
    }
}

/// The exact-mirror prune step: walk the dest, compute the prune set, and unlink
/// the extraneous paths deepest-first. A faithful copy of the CLI's `prune_dest`
/// body (no excludes, no dry-run) over the public `prune_set`.
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

/// 6. `mirror`: compares, per BENCH scenario, three timed arms into a FRESH dest
///    per iteration (so each does real copy + prune work):
///
///    * `checkout`         — plain `fetch_files` (the baseline, no prune).
///    * `zero_extraneous`  — `fetch_files` + mirror prune with NOTHING extraneous
///      (the headline: this should be ≈ `checkout`).
///    * `with_extraneous`  — `fetch_files`, plant `MIRROR_EXTRANEOUS` stale files,
///      then mirror prune (real deletions happen).
///
/// `iter_batched` hands each timed iteration a fresh empty dest. The store is
/// seeded ONCE; the apply strategy is `MaterializeMode::Auto` (reflink-or-copy
/// into real inodes — the default checkout). Linked (symlink) and atomic
/// (reflink-swap) apply strategies are exercised by the dedicated `fetch` arms
/// elsewhere; the mirror headline is strategy-independent (the prune walk runs
/// after whichever apply produced the dest).
fn bench_mirror(c: &mut Criterion) {
    let mut group = c.benchmark_group("mirror");
    for scenario in bench_scenarios() {
        let source = materialize(&scenario);
        let manifest = walk_manifest(source.path());
        let store_dir = fresh_dir();
        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, source.path()).expect("seed push");
        group.throughput(Throughput::Bytes(manifest_bytes(&manifest)));

        // Baseline: plain checkout (fetch_files, no prune).
        group.bench_with_input(
            BenchmarkId::new("checkout", scenario.name),
            &manifest,
            |b, manifest| {
                b.iter_batched(
                    fresh_dir,
                    |dest_dir| {
                        store
                            .fetch_files_with_mode(
                                black_box(manifest),
                                dest_dir.path(),
                                MaterializeMode::Auto,
                            )
                            .expect("checkout");
                        dest_dir
                    },
                    BatchSize::PerIteration,
                );
            },
        );

        // Headline: zero-extraneous mirror — fetch + prune walk with nothing to
        // delete. Should be ≈ the plain checkout above.
        group.bench_with_input(
            BenchmarkId::new("zero_extraneous", scenario.name),
            &manifest,
            |b, manifest| {
                b.iter_batched(
                    fresh_dir,
                    |dest_dir| {
                        store
                            .fetch_files_with_mode(
                                black_box(manifest),
                                dest_dir.path(),
                                MaterializeMode::Auto,
                            )
                            .expect("checkout");
                        mirror_prune(black_box(manifest), dest_dir.path());
                        dest_dir
                    },
                    BatchSize::PerIteration,
                );
            },
        );

        // With extraneous: fetch, plant N stale files, then mirror prune (real
        // deletions). The delta vs `checkout` is the cost of removing them.
        group.bench_with_input(
            BenchmarkId::new("with_extraneous", scenario.name),
            &manifest,
            |b, manifest| {
                b.iter_batched(
                    fresh_dir,
                    |dest_dir| {
                        store
                            .fetch_files_with_mode(
                                black_box(manifest),
                                dest_dir.path(),
                                MaterializeMode::Auto,
                            )
                            .expect("checkout");
                        plant_extraneous(dest_dir.path(), MIRROR_EXTRANEOUS);
                        mirror_prune(black_box(manifest), dest_dir.path());
                        dest_dir
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(
    pipeline,
    bench_walk_hash,
    bench_snapshot_id,
    bench_push,
    bench_fetch,
    bench_sync,
    bench_mirror,
);
criterion_main!(pipeline);
