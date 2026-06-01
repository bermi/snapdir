//! snapdir hot-path microbenchmarks.
//!
//! Three criterion groups covering the perf-critical paths of the port. These
//! benches **measure only** — they never touch `crates/**`, and nothing here
//! changes output bytes. They simply exercise the shipped `snapdir-core` public
//! API so the perf gate / `CodSpeed` can track regressions:
//!
//! 1. `hash`     — `Blake3Hasher::hash_hex` over a range of buffer sizes, with
//!    `Throughput::Bytes` so MB/s is reported. (The mmap+rayon vs streamed
//!    switch lives in core/walk; this is the in-process hash hot path.)
//! 2. `walk`     — `walk()` over two deterministic corpora built once outside
//!    the timed loop: a *many-small-files* tree and a *few-large-files* tree.
//!    They behave very differently (per-file syscall overhead vs raw hashing
//!    throughput), so they get separate IDs.
//! 3. `manifest` — emit (`Display` → `String`) and parse
//!    (`ManifestEntry::parse_line`) of a Manifest fixture built once outside
//!    the loop — a round-trip group.
//!
//! All inputs/outputs are `black_box`ed so the optimizer can't elide the work.
//! The walk corpora use deterministic bytes (no RNG) and self-clean via
//! `tempfile`'s `Drop`.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use snapdir_core::{walk, Blake3Hasher, Hasher, Manifest, ManifestEntry, PathType, WalkOptions};
use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use tempfile::TempDir;

/// Buffer sizes for the hash hot path (64 B, 4 KiB, 64 KiB, 1 MiB).
const HASH_SIZES: &[usize] = &[64, 4 * 1024, 64 * 1024, 1024 * 1024];

/// Fills a buffer of `len` bytes with a deterministic, non-trivial pattern (no
/// RNG, so corpora are reproducible across runs and machines).
fn deterministic_bytes(len: usize) -> Vec<u8> {
    // A simple byte ramp; cheap and fully deterministic. Masking to the low 8
    // bits gives a repeating-but-non-uniform pattern so the hasher can't
    // shortcut on a constant page; `try_from` after the mask is infallible.
    (0..len)
        .map(|i| u8::try_from(i.wrapping_mul(31).wrapping_add(7) & 0xff).expect("masked to u8"))
        .collect()
}

/// 1. Hash hot path: `Blake3Hasher::hash_hex` across buffer sizes, reporting
///    throughput in bytes so criterion prints MB/s.
fn bench_hash(c: &mut Criterion) {
    let hasher = Blake3Hasher::new();
    let mut group = c.benchmark_group("hash/blake3");
    for &size in HASH_SIZES {
        let buf = deterministic_bytes(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("{size}"), |b| {
            b.iter(|| black_box(hasher.hash_hex(black_box(&buf))));
        });
    }
    group.finish();
}

/// Writes `count` tiny files of `bytes_each` across `dirs` nested subdirectories
/// (round-robin), deterministically. Returns the owning `TempDir` (cleaned up on
/// `Drop`).
fn build_many_small(count: usize, bytes_each: usize, dirs: usize) -> TempDir {
    let tmp = TempDir::new().expect("create temp dir");
    let root = tmp.path();
    for d in 0..dirs {
        fs::create_dir_all(root.join(format!("d{d:03}"))).expect("mkdir");
    }
    let content = deterministic_bytes(bytes_each);
    for i in 0..count {
        let dir = root.join(format!("d{:03}", i % dirs));
        fs::write(dir.join(format!("f{i:05}.bin")), &content).expect("write file");
    }
    tmp
}

/// Writes `count` multi-MB files of `bytes_each`, deterministically. Returns the
/// owning `TempDir` (cleaned up on `Drop`).
fn build_few_large(count: usize, bytes_each: usize) -> TempDir {
    let tmp = TempDir::new().expect("create temp dir");
    let root = tmp.path();
    let content = deterministic_bytes(bytes_each);
    for i in 0..count {
        fs::write(root.join(format!("big{i:02}.bin")), &content).expect("write file");
    }
    tmp
}

/// Runs a default-options BLAKE3 walk over `root`, black-boxing the result.
fn run_walk(root: &Path) {
    let manifest = walk(
        black_box(root),
        black_box(&WalkOptions::default()),
        black_box(&Blake3Hasher::new()),
    )
    .expect("walk corpus");
    black_box(manifest);
}

/// 2. Walk hot path: build the two corpora ONCE (outside the timed loop), then
///    bench `walk()` over each.
fn bench_walk(c: &mut Criterion) {
    // many-small: a few thousand tiny files across nested dirs — dominated by
    // per-file syscall/metadata overhead.
    let many = build_many_small(4096, 64, 32);
    // few-large: a handful of multi-MB files — dominated by raw hashing
    // throughput.
    let large = build_few_large(8, 4 * 1024 * 1024);

    let mut group = c.benchmark_group("walk");
    group.bench_function("many_small", |b| {
        b.iter(|| run_walk(many.path()));
    });
    group.bench_function("few_large", |b| {
        b.iter(|| run_walk(large.path()));
    });
    group.finish();
    // `many` / `large` drop here, removing the scratch trees.
}

/// Builds a deterministic `Manifest` of `n` file entries (built once, outside
/// the timed loop) and its rendered lines for the parse direction.
fn build_manifest_fixture(n: usize) -> (Manifest, Vec<String>) {
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        // Deterministic 64-hex-char blake3-shaped checksum and a stable path.
        let checksum = format!("{i:064x}");
        entries.push(ManifestEntry::new(
            PathType::File,
            "644",
            checksum,
            (i as u64).wrapping_mul(7),
            format!("./dir{:03}/file{:05}.bin", i % 100, i),
        ));
    }
    let manifest = Manifest::from_entries(entries);
    let lines: Vec<String> = manifest.entries().iter().map(ToString::to_string).collect();
    (manifest, lines)
}

/// 3. Manifest hot path: emit (`Display` → `String`) and parse round-trip.
fn bench_manifest(c: &mut Criterion) {
    let mut group = c.benchmark_group("manifest");
    for &n in &[1_000usize, 10_000usize] {
        let (manifest, lines) = build_manifest_fixture(n);

        group.bench_function(format!("emit/{n}"), |b| {
            b.iter(|| {
                // Collect the whole manifest's Display into one String, mirroring
                // how the CLI emits a manifest.
                let mut out = String::with_capacity(n * 80);
                for entry in black_box(&manifest).entries() {
                    writeln!(out, "{entry}").expect("write to String");
                }
                black_box(out)
            });
        });

        group.bench_function(format!("parse/{n}"), |b| {
            b.iter(|| {
                for line in black_box(&lines) {
                    let entry = ManifestEntry::parse_line(black_box(line)).expect("parse line");
                    black_box(entry);
                }
            });
        });
    }
    group.finish();
}

criterion_group!(hot_paths, bench_hash, bench_walk, bench_manifest);
criterion_main!(hot_paths);
