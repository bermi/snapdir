//! SNAPPACK bytes-on-wire contract test.
//!
//! Pins the property the `snappack` criterion bench's zstd path exists to
//! exploit: for the **text-like (compressible)** corpus — scenario (a) of
//! `benches/benches/snappack.rs` — a `PackFormat::Zstd` pack is strictly
//! SMALLER on the wire than the plain `PackFormat::V1` pack, and the ratio is
//! printed for the evidence record. A companion check pins that the
//! **incompressible** corpus (scenario (b)) does not materially expand under
//! zstd (the worst case stays bounded).
//!
//! This lives in `benches/tests/` (not inside the `harness = false` bench
//! target, whose `criterion_main!` would shadow any `#[test]`), so the libtest
//! harness actually runs it under `cargo test -p snapdir-benches`. It drives
//! the shipped `snapdir-stores` pack API exactly as the bench does and changes
//! no output bytes.

use snapdir_benches::{incompressible_bytes, text_like_bytes};
use snapdir_core::merkle::{Blake3Hasher, Hasher};
use snapdir_stores::{write_pack_with_format, FileStore, PackFormat, StreamStore};
use tempfile::TempDir;

/// Stable per-object seed base — matches `snappack.rs` so the test exercises the
/// same corpora the bench measures.
const SEED: u64 = 0x5311_AC3D_0BAD_F00D;

/// Scenario (a) object size (text-like, compressible).
const A_BYTES: usize = 4 * 1024;
/// Scenario (b) object size (incompressible).
const B_BYTES: usize = 1024 * 1024;

/// Seeds `count` distinct deterministic objects (via `gen(len, SEED+i)`) into a
/// fresh `FileStore`, returning the store (+ its tempdir guard) and the ids in
/// order.
fn seed_corpus(
    count: usize,
    len: usize,
    gen: fn(usize, u64) -> Vec<u8>,
) -> (TempDir, FileStore, Vec<String>) {
    let hasher = Blake3Hasher::new();
    let dir = TempDir::new().expect("create source store dir");
    let store = FileStore::from_root(dir.path());
    let ids = (0..count)
        .map(|i| {
            let bytes = gen(len, SEED.wrapping_add(i as u64));
            let checksum = hasher.hash_hex(&bytes);
            store
                .put_object(&checksum, bytes)
                .expect("seed source object");
            checksum
        })
        .collect();
    (dir, store, ids)
}

/// Encodes the whole id set into one in-memory pack in `format` (no manifest).
fn encode(store: &FileStore, ids: &[String], format: PackFormat) -> Vec<u8> {
    let mut out = Vec::new();
    write_pack_with_format(store, ids, None, format, &mut out).expect("write_pack into Vec");
    out
}

/// Bytes-on-wire contract: for text-like scenario (a), zstd < v1. Uses a small
/// slice (the ratio is content-driven, not count-driven) so the test is fast.
#[test]
fn zstd_pack_is_smaller_than_v1_for_text_scenario_a() {
    let (_dir, store, ids) = seed_corpus(256, A_BYTES, text_like_bytes);
    let v1 = encode(&store, &ids, PackFormat::V1);
    let zstd = encode(&store, &ids, PackFormat::zstd_default());
    // Integer permille ratio (zstd/v1 × 1000) — no float cast (clippy pedantic).
    let permille = zstd.len() as u128 * 1000 / v1.len() as u128;
    println!(
        "snappack bytes-on-wire (scenario a, text-like, {} obj × {A_BYTES} B): \
         v1 = {} B, zstd = {} B, ratio (zstd/v1) = {}.{:03}",
        ids.len(),
        v1.len(),
        zstd.len(),
        permille / 1000,
        permille % 1000,
    );
    assert!(
        zstd.len() < v1.len(),
        "zstd pack ({} B) must be smaller than v1 ({} B) for text-like scenario (a)",
        zstd.len(),
        v1.len(),
    );
}

/// Sanity: incompressible scenario (b) does NOT materially expand under zstd
/// (the worst case stays bounded — never a runaway blow-up).
#[test]
fn zstd_does_not_blow_up_incompressible_scenario_b() {
    let (_dir, store, ids) = seed_corpus(8, B_BYTES, incompressible_bytes);
    let v1 = encode(&store, &ids, PackFormat::V1);
    let zstd = encode(&store, &ids, PackFormat::zstd_default());
    let permille = zstd.len() as u128 * 1000 / v1.len() as u128;
    println!(
        "snappack bytes-on-wire (scenario b, incompressible, {} obj × {B_BYTES} B): \
         v1 = {} B, zstd = {} B, ratio (zstd/v1) = {}.{:03}",
        ids.len(),
        v1.len(),
        zstd.len(),
        permille / 1000,
        permille % 1000,
    );
    // zstd stores an incompressible frame nearly verbatim (+small framing): the
    // pack must not grow by more than 1% (integer ×100 comparison, no float).
    assert!(
        zstd.len() as u128 * 100 < v1.len() as u128 * 101,
        "zstd pack ({} B) blew up vs v1 ({} B) on incompressible data",
        zstd.len(),
        v1.len(),
    );
}
