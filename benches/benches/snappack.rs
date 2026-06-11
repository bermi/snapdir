//! SNAPPACK send/receive wall-clock benchmarks (v1 vs zstd, fsync off vs batch).
//!
//! These criterion benches drive the just-landed SNAPPACK pack wire
//! (`snapdir-stores`) end to end over two deterministic corpora:
//!
//! - **(a)** 5k × 4KiB *text-like* objects — compressible, the realistic
//!   "small-text snapshot" the zstd transport targets.
//! - **(b)** 256 × 1MiB *incompressible* objects — the worst case for
//!   compression (proves zstd never materially expands an unhelpable payload).
//!
//! Two families, mirroring `pipeline.rs`'s `BatchSize::PerIteration` pattern:
//!
//! 1. `pack/send/{a,b}/{v1,zstd}` — [`write_pack_with_format`] into a `Vec`
//!    (in-memory sink), `Throughput::Bytes` over the source object bytes.
//! 2. `pack/receive/{a,b}/{v1,zstd} × {off,batch}` — [`read_pack`] a
//!    pre-encoded pack into a FRESH `FileStore` dir per iteration via a
//!    [`FileSink`] with [`Durability::Off`] / [`Durability::Batch`].
//!
//! Bytes-on-wire is pinned by a `#[test]` (`zstd < v1` for scenario (a), prints
//! the ratio). These benches **measure only** — they never touch `crates/**`
//! and change no output bytes; they exercise the shipped `snapdir-stores`
//! public pack API exactly as `send-pack` / `receive-pack` will.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use snapdir_benches::{incompressible_bytes, text_like_bytes};
use snapdir_core::merkle::{Blake3Hasher, Hasher};
use snapdir_stores::{
    read_pack, write_pack_with_format, Durability, FileSink, FileStore, PackFormat, StreamStore,
};
use std::hint::black_box;
use tempfile::TempDir;

/// Stable per-scenario seed (any fixed value; the corpora must be
/// deterministic, not random).
const SEED: u64 = 0x5311_AC3D_0BAD_F00D;

/// Scenario (a): 5k × 4KiB text-like (compressible) objects.
const A_OBJECTS: usize = 5_000;
const A_BYTES: usize = 4 * 1024;

/// Scenario (b): 256 × 1MiB incompressible objects.
const B_OBJECTS: usize = 256;
const B_BYTES: usize = 1024 * 1024;

/// A built corpus: distinct object payloads + their content-addresses, plus a
/// seeded `FileStore` (in its own long-lived `TempDir`) holding them. The store
/// is the send source; the ids drive both send and receive.
struct Corpus {
    _src_dir: TempDir,
    store: FileStore,
    ids: Vec<String>,
    total_bytes: u64,
}

/// Distinct deterministic payloads via `gen(len, seed + i)` so every object has
/// a unique content-address (a content-addressed store would otherwise dedup
/// identical bodies down to one).
fn build_corpus(count: usize, len: usize, gen: fn(usize, u64) -> Vec<u8>) -> Corpus {
    let hasher = Blake3Hasher::new();
    let src_dir = TempDir::new().expect("create source store dir");
    let store = FileStore::from_root(src_dir.path());
    let mut ids = Vec::with_capacity(count);
    let mut total_bytes = 0u64;
    for i in 0..count {
        let bytes = gen(len, SEED.wrapping_add(i as u64));
        let checksum = hasher.hash_hex(&bytes);
        total_bytes += bytes.len() as u64;
        store
            .put_object(&checksum, bytes)
            .expect("seed source object");
        ids.push(checksum);
    }
    Corpus {
        _src_dir: src_dir,
        store,
        ids,
        total_bytes,
    }
}

/// Encodes the whole corpus into one in-memory pack in `format` (no manifest —
/// the bench measures raw object transport).
fn encode_pack(corpus: &Corpus, format: PackFormat) -> Vec<u8> {
    let mut out = Vec::new();
    write_pack_with_format(&corpus.store, &corpus.ids, None, format, &mut out)
        .expect("write_pack into Vec");
    out
}

/// A fresh, empty scratch directory (its own `TempDir`).
fn fresh_dir() -> TempDir {
    TempDir::new().expect("create scratch dir")
}

/// 1. `pack/send/{a,b}/{v1,zstd}`: write the whole corpus to an in-memory pack.
fn bench_send(c: &mut Criterion) {
    let mut group = c.benchmark_group("pack/send");
    for (name, corpus) in [
        ("a", build_corpus(A_OBJECTS, A_BYTES, text_like_bytes)),
        ("b", build_corpus(B_OBJECTS, B_BYTES, incompressible_bytes)),
    ] {
        group.throughput(Throughput::Bytes(corpus.total_bytes));
        for (fmt_name, format) in [("v1", PackFormat::V1), ("zstd", PackFormat::zstd_default())] {
            group.bench_function(format!("{name}/{fmt_name}"), |b| {
                b.iter(|| black_box(encode_pack(black_box(&corpus), format)));
            });
        }
    }
    group.finish();
}

/// 2. `pack/receive/{a,b}/{v1,zstd} × {off,batch}`: pre-encode the pack ONCE,
///    then each iteration reads it into a FRESH `FileStore` dir so real filing
///    (and, under `batch`, fsync) work is timed.
fn bench_receive(c: &mut Criterion) {
    let mut group = c.benchmark_group("pack/receive");
    for (name, corpus) in [
        ("a", build_corpus(A_OBJECTS, A_BYTES, text_like_bytes)),
        ("b", build_corpus(B_OBJECTS, B_BYTES, incompressible_bytes)),
    ] {
        group.throughput(Throughput::Bytes(corpus.total_bytes));
        for (fmt_name, format) in [("v1", PackFormat::V1), ("zstd", PackFormat::zstd_default())] {
            let pack = encode_pack(&corpus, format);
            for (dur_name, durability) in [("off", Durability::Off), ("batch", Durability::Batch)] {
                group.bench_function(format!("{name}/{fmt_name}/{dur_name}"), |b| {
                    b.iter_batched(
                        fresh_dir,
                        |dest_dir| {
                            let store = FileStore::from_root(dest_dir.path());
                            let mut sink = FileSink::new(&store).with_durability(durability);
                            read_pack(black_box(pack.as_slice()), &mut sink).expect("read_pack");
                            dest_dir
                        },
                        BatchSize::PerIteration,
                    );
                });
            }
        }
    }
    group.finish();
}

criterion_group!(snappack, bench_send, bench_receive);
criterion_main!(snappack);
