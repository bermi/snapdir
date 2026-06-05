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
use snapdir_benches::{bench_scenarios, Scenario};
use snapdir_core::merkle::snapshot_id;
use snapdir_core::store::Store;
use snapdir_core::{walk, Blake3Hasher, Manifest, PathType, WalkOptions};
use snapdir_stores::{sync_snapshot, FileStore, TransferConfig};
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

criterion_group!(
    pipeline,
    bench_walk_hash,
    bench_snapshot_id,
    bench_push,
    bench_fetch,
    bench_sync,
);
criterion_main!(pipeline);
