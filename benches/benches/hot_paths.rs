//! snapdir hot-path microbenchmarks.
//!
//! This is the minimal scaffold target wired by the `bench-scaffold` gate so
//! that `cargo build --benches` builds a real bench (not a vacuous 0-target
//! pass). It exercises one trivial `snapdir-core` hot path — BLAKE3 hashing of
//! a tiny in-memory buffer via the core [`Hasher`] abstraction.
//!
//! The `bench` lane (gate `bench-compile`) will expand this into the real
//! hash / walk / manifest hot-path benchmarks.

use criterion::{criterion_group, criterion_main, Criterion};
use snapdir_core::{Blake3Hasher, Hasher};
use std::hint::black_box;

fn bench_blake3_hash_hex(c: &mut Criterion) {
    let hasher = Blake3Hasher::new();
    let buf = b"snapdir hot-path scaffold buffer";
    c.bench_function("blake3_hash_hex/tiny", |b| {
        b.iter(|| hasher.hash_hex(black_box(buf)));
    });
}

criterion_group!(hot_paths, bench_blake3_hash_hex);
criterion_main!(hot_paths);
