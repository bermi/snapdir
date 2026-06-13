//! Fast-walk contract tests (snapdir-core integration) — ADVERSARY, black-box.
//!
//! Authored from the gate SPEC ONLY, with zero visibility into the not-yet-written
//! implementation. These tests pin the "fast walk" contract: making the directory
//! walk + file hashing parallel and memory-friendly while keeping snapshot ids
//! **BYTE-IDENTICAL**.
//!
//! Contracted symbols referenced here (so the suite cannot pass until the impl
//! lands):
//!   - `snapdir_core::hash_file::{HashFile, MMAP_THRESHOLD}` — a `HashFile`
//!     extension trait `fn hash_file_hex(&self, path: &Path) -> io::Result<(String, u64)>`
//!     with concrete impls for Blake3Hasher / Blake3KeyedHasher / Md5Hasher /
//!     Sha256Hasher, returning `(lowercase_hex, byte_len)`.
//!   - `WalkOptions.walk_jobs: Option<usize>` — cross-file hashing parallelism.
//!
//! GOLDENS: captured by running the CURRENT (pre-change) `snapdir` binary on the
//! SAME deterministic fixtures these tests materialize. The fixtures pin every
//! file mode to 0o644 and every dir mode to 0o755 (umask-independent), and the
//! snapshot id is computed over `./`-relative paths, so the golden is independent
//! of the tempdir location (verified: two different parent dirs yield the same id).
//! The fixture byte content is the same deterministic ramp the `benches` crate's
//! `deterministic_bytes` uses — `(i*31 + 7) & 0xff` — so the trees, and therefore
//! the ids, are reproducible across runs and machines.
//!
//! NOTE for the impl lane: this file is self-contained (no `tempfile`, no
//! `snapdir-benches` dev-dep). It uses a deterministic, hand-rolled scratch dir
//! under `std::env::temp_dir()` that mirrors the bench `Shape` materializers, so
//! it can land at `crates/snapdir-core/tests/fast_walk.rs` with only the `proptest`
//! dev-dep that already exists.

// Test-shape allows (impl-lane wiring, NOT assertion changes): this adversary
// suite deliberately casts across the MMAP_THRESHOLD boundary (u64/i64/usize),
// asserts on the contract const, keeps the full contracted import set, and
// writes prose doc-comments — all of which trip the workspace's pedantic-clippy
// gate on the test crate. None of these affect any assertion or golden.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::assertions_on_constants,
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    unused_imports
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;

use snapdir_core::hash_file::{HashFile, MMAP_THRESHOLD};
use snapdir_core::{
    snapshot_id, walk, Blake3Hasher, Blake3KeyedHasher, Hasher, Manifest, Md5Hasher, Sha256Hasher,
    WalkOptions,
};

// ---------------------------------------------------------------------------
// Deterministic fixtures — std-only, mirroring benches/src/lib.rs Shape logic.
// ---------------------------------------------------------------------------

/// Umask-independent, explicit modes (match the bench generator + the goldens).
const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;

/// A process-unique, monotonically increasing counter so each scratch tree gets
/// a distinct directory (no `tempfile` dependency, no collisions across tests
/// run in parallel by the test harness).
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// An RAII scratch directory under `std::env::temp_dir()`, removed on drop.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "snapdir_fastwalk_{}_{}_{}_{}",
            tag,
            std::process::id(),
            seq,
            // A second disambiguator so re-running back-to-back never reuses a
            // stale dir left by a crashed prior run.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&path);
        mkdir(&path);
        Scratch { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The bench `deterministic_bytes` ramp: a cheap, fully deterministic, non-uniform
/// byte pattern. MUST stay byte-identical to `benches::deterministic_bytes` so the
/// captured goldens hold.
fn deterministic_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i.wrapping_mul(31).wrapping_add(7) & 0xff).expect("masked to u8"))
        .collect()
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, PermissionsExt::from_mode(mode)).expect("set_permissions");
}

#[cfg(not(unix))]
fn chmod(_path: &Path, _mode: u32) {}

fn mkdir(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create_dir_all");
    chmod(dir, DIR_MODE);
}

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, content).expect("write file");
    chmod(path, FILE_MODE);
}

/// The bench `Shape` catalog this gate's KEYSTONE pins. Materializes each tree
/// byte-identically to `benches/src/lib.rs` (gate-tier sizes), so the goldens
/// captured from the current binary hold.
#[derive(Clone, Copy)]
enum Shape {
    ManySmall,
    FewLarge,
    WideDir,
    Mixed,
    DupContent,
    EmptyFilesAndDirs,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::ManySmall => "many_small",
            Shape::FewLarge => "few_large",
            Shape::WideDir => "wide_dir",
            Shape::Mixed => "mixed",
            Shape::DupContent => "dup_content",
            Shape::EmptyFilesAndDirs => "empty_files_and_dirs",
        }
    }

    fn materialize(self, dir: &Path) {
        match self {
            // ManySmall { files: 24, bytes: 16, dirs: 4 }
            Shape::ManySmall => {
                for d in 0..4 {
                    mkdir(&dir.join(format!("d{d:03}")));
                }
                let content = deterministic_bytes(16);
                for i in 0..24 {
                    let sub = dir.join(format!("d{:03}", i % 4));
                    write_file(&sub.join(format!("f{i:05}.bin")), &content);
                }
            }
            // FewLarge { files: 3, bytes: 4096 }
            Shape::FewLarge => {
                let content = deterministic_bytes(4 * 1024);
                for i in 0..3 {
                    write_file(&dir.join(format!("big{i:02}.bin")), &content);
                }
            }
            // WideFanout { children: 32, bytes: 16 } -> "wide_dir"
            Shape::WideDir => {
                let fan = dir.join("fan");
                mkdir(&fan);
                let content = deterministic_bytes(16);
                for i in 0..32 {
                    write_file(&fan.join(format!("c{i:04}.bin")), &content);
                }
            }
            // Mixed (exact bench recipe).
            Shape::Mixed => {
                for (name, n) in [
                    ("root_a.bin", 16usize),
                    ("root_b.bin", 256),
                    ("root_c.bin", 0),
                ] {
                    write_file(&dir.join(name), &deterministic_bytes(n));
                }
                let nested = dir.join("dir_a").join("nested");
                mkdir(&nested);
                write_file(&dir.join("dir_a").join("a1.bin"), &deterministic_bytes(48));
                write_file(&nested.join("deep.bin"), &deterministic_bytes(72));
                let dir_b = dir.join("dir_b");
                mkdir(&dir_b);
                write_file(&dir_b.join("b1.bin"), &deterministic_bytes(128));
                write_file(&dir_b.join("b2.bin"), &deterministic_bytes(8));
            }
            // Dedup { copies: 8, bytes: 32 } -> "dup_content"
            Shape::DupContent => {
                let content = deterministic_bytes(32);
                for i in 0..8 {
                    write_file(&dir.join(format!("dup{i:03}.bin")), &content);
                }
            }
            // Edge: empty files AND empty dirs -> "empty_files_and_dirs"
            Shape::EmptyFilesAndDirs => {
                write_file(&dir.join("empty_a.bin"), &[]);
                write_file(&dir.join("empty_b.bin"), &[]);
                mkdir(&dir.join("empty_dir_a"));
                mkdir(&dir.join("empty_dir_b"));
                let sub = dir.join("sub");
                mkdir(&sub);
                write_file(&sub.join("empty_c.bin"), &[]);
            }
        }
    }
}

/// The KEYSTONE goldens, captured by running the CURRENT (pre-change) `snapdir`
/// binary on the SAME `Shape`-materialized fixtures (modes pinned, `./`-relative
/// id, location-independent). Every later gate must preserve these byte-for-byte.
fn golden_id(shape: Shape) -> &'static str {
    match shape {
        Shape::ManySmall => "1a05825401b777f2c21baba7621c14387b60b059f59449b04d1cb86f3fefe2d1",
        Shape::FewLarge => "5741158d9191ca1a78b2663568859b7a7b3cf9ebae81214a4e195973a2df5e9f",
        Shape::WideDir => "20765645a0af81bc50d6f8180792deacf88b0ef7f4a41868e410cb2d23c28c92",
        Shape::Mixed => "20e480591db13694baaad6559337f682b0e1a9e7e85ccbf25702ec612d5c2b4f",
        Shape::DupContent => "d5604b4f35fbc05f0e8604e747b1e471faa0c55fceaced01519c159b04fb1c14",
        Shape::EmptyFilesAndDirs => {
            "33ed645bf0884cbe82710b2837bfa156dfa9d15388f3c9407e81d6b5c330afdf"
        }
    }
}

const ALL_SHAPES: [Shape; 6] = [
    Shape::ManySmall,
    Shape::FewLarge,
    Shape::WideDir,
    Shape::Mixed,
    Shape::DupContent,
    Shape::EmptyFilesAndDirs,
];

/// Walks `dir` with the default options + BLAKE3 and returns the snapshot id.
fn id_of(dir: &Path) -> String {
    let hasher = Blake3Hasher::new();
    let manifest = walk(dir, &WalkOptions::default(), &hasher).expect("walk succeeds");
    snapshot_id(&manifest, &hasher)
}

// ===========================================================================
// 1. KEYSTONE golden ids — every Shape's snapshot id matches the recorded golden.
// ===========================================================================

#[test]
fn keystone_golden_snapshot_ids_per_shape() {
    // Spec clause: KEYSTONE — for each bench Shape, walk + snapshot_id == the
    // GOLDEN captured from the current binary. These anchor every later gate;
    // parallelism / mmap MUST NOT change them.
    for shape in ALL_SHAPES {
        let scratch = Scratch::new(shape.name());
        shape.materialize(scratch.path());
        let got = id_of(scratch.path());
        assert_eq!(
            got,
            golden_id(shape),
            "snapshot id for shape {} drifted from the golden",
            shape.name()
        );
    }
}

#[test]
fn keystone_golden_id_is_location_independent() {
    // Spec clause: the snapshot id is over `./`-relative paths, so materializing
    // the SAME shape under two DIFFERENT parents yields the SAME golden id (this
    // is what makes the hardcoded cross-machine golden legitimate).
    let shape = Shape::Mixed;
    let a = Scratch::new("loc_a");
    let b = Scratch::new("loc_b_longer_name");
    shape.materialize(a.path());
    shape.materialize(b.path());
    assert_eq!(id_of(a.path()), golden_id(shape));
    assert_eq!(id_of(b.path()), golden_id(shape));
    assert_eq!(id_of(a.path()), id_of(b.path()));
}

// ===========================================================================
// 2. walk-twice determinism (deterministic + proptest).
// ===========================================================================

#[test]
fn walk_twice_yields_identical_manifest_and_id_per_shape() {
    // Spec clause: walk-twice determinism. The same tree, walked twice, yields
    // identical Manifest Display AND identical snapshot_id (a strong anchor even
    // where a hardcoded golden might be fragile).
    let hasher = Blake3Hasher::new();
    for shape in ALL_SHAPES {
        let scratch = Scratch::new(shape.name());
        shape.materialize(scratch.path());

        let m1 = walk(scratch.path(), &WalkOptions::default(), &hasher).expect("walk 1");
        let m2 = walk(scratch.path(), &WalkOptions::default(), &hasher).expect("walk 2");
        assert_eq!(
            m1.to_string(),
            m2.to_string(),
            "Manifest Display differs across walks for {}",
            shape.name()
        );
        assert_eq!(
            snapshot_id(&m1, &hasher),
            snapshot_id(&m2, &hasher),
            "snapshot_id differs across walks for {}",
            shape.name()
        );
    }
}

/// Materializes a random-but-DETERMINISTIC tree (driven by a proptest-supplied
/// shape selection + sizes) and returns the scratch dir.
fn materialize_random_tree(dir: &Path, dir_count: usize, files: &[(usize, usize)]) {
    // `files` is a list of (target_dir_index, byte_len). dir_count subdirs plus
    // the root; deterministic byte content from the ramp.
    for d in 0..dir_count {
        mkdir(&dir.join(format!("sub{d:03}")));
    }
    for (i, (which, len)) in files.iter().enumerate() {
        let target = if dir_count == 0 || *which == 0 {
            dir.to_path_buf()
        } else {
            dir.join(format!("sub{:03}", which % dir_count))
        };
        write_file(
            &target.join(format!("file{i:04}.bin")),
            &deterministic_bytes(*len),
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Spec clause: walk-twice determinism (proptest). A random deterministic
    /// tree, walked twice, yields identical Manifest Display + identical
    /// snapshot_id. Includes sizes straddling MMAP_THRESHOLD so both hashing
    /// branches are exercised under determinism.
    #[test]
    fn proptest_walk_twice_determinism(
        dir_count in 0usize..5,
        files in prop::collection::vec(
            (0usize..5, prop_oneof![0usize..2048, Just(256 * 1024 - 1), Just(256 * 1024), Just(256 * 1024 + 1)]),
            0..12,
        ),
    ) {
        let scratch = Scratch::new("prop");
        materialize_random_tree(scratch.path(), dir_count, &files);

        let hasher = Blake3Hasher::new();
        let m1 = walk(scratch.path(), &WalkOptions::default(), &hasher).expect("walk 1");
        let m2 = walk(scratch.path(), &WalkOptions::default(), &hasher).expect("walk 2");
        prop_assert_eq!(m1.to_string(), m2.to_string());
        prop_assert_eq!(snapshot_id(&m1, &hasher), snapshot_id(&m2, &hasher));
    }
}

// ===========================================================================
// 3. Per-hasher equivalence: hash_file_hex(path).0 == hash_hex(&fs::read(path)),
//    and .1 == file size, for every contracted hasher.
// ===========================================================================

/// Writes `content` to a fresh scratch file and returns (scratch, path).
fn scratch_file(tag: &str, content: &[u8]) -> (Scratch, PathBuf) {
    let scratch = Scratch::new(tag);
    let path = scratch.path().join("payload.bin");
    write_file(&path, content);
    (scratch, path)
}

/// For one hasher, assert hash_file_hex == hash_hex(read) and the byte len for an
/// empty file, a sub-threshold file, and a multi-MB (> MMAP_THRESHOLD) file.
fn assert_hasher_equivalence<H: Hasher + HashFile>(hasher: &H, tag: &str) {
    let cases: [(&str, Vec<u8>); 3] = [
        ("empty", Vec::new()),
        ("small_1k", deterministic_bytes(1024)),
        // > 256 KiB so the blake3 mmap+rayon path is exercised; multi-MB.
        ("multi_mb", deterministic_bytes(3 * 1024 * 1024 + 5)),
    ];
    for (label, content) in cases {
        let (_scratch, path) = scratch_file(&format!("{tag}_{label}"), &content);
        let (hex, len) = hasher
            .hash_file_hex(&path)
            .expect("hash_file_hex must succeed on a readable file");
        let expected_hex = hasher.hash_hex(&std::fs::read(&path).unwrap());
        assert_eq!(
            hex, expected_hex,
            "hash_file_hex hex must equal hash_hex(read) [{tag} {label}]"
        );
        assert_eq!(
            len,
            content.len() as u64,
            "hash_file_hex byte len must equal the file size [{tag} {label}]"
        );
        // Lowercase-hex contract.
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash_file_hex must return lowercase hex [{tag} {label}]: {hex}"
        );
    }
}

#[test]
fn hash_file_hex_blake3_equivalence() {
    // Spec clause: per-hasher equivalence (Blake3Hasher), incl. the mmap path.
    assert_hasher_equivalence(&Blake3Hasher::new(), "blake3");
}

#[test]
fn hash_file_hex_blake3_keyed_equivalence() {
    // Spec clause: per-hasher equivalence (Blake3KeyedHasher / derive-key).
    assert_hasher_equivalence(
        &Blake3KeyedHasher::new("snapdir fast-walk adversary context"),
        "blake3keyed",
    );
}

#[test]
fn hash_file_hex_md5_equivalence() {
    // Spec clause: per-hasher equivalence (Md5Hasher) — read whole file + hash_hex.
    assert_hasher_equivalence(&Md5Hasher::new(), "md5");
}

#[test]
fn hash_file_hex_sha256_equivalence() {
    // Spec clause: per-hasher equivalence (Sha256Hasher) — read whole file + hash_hex.
    assert_hasher_equivalence(&Sha256Hasher::new(), "sha256");
}

#[test]
fn hash_file_hex_keyed_differs_from_unkeyed() {
    // Spec corollary: the keyed blake3 file hash must NOT equal the unkeyed one
    // for the same bytes (proves keyed mode is actually keyed via the file path).
    let content = deterministic_bytes(512 * 1024); // exercise the mmap branch
    let (_s, path) = scratch_file("keyed_vs_unkeyed", &content);
    let unkeyed = Blake3Hasher::new().hash_file_hex(&path).unwrap().0;
    let keyed = Blake3KeyedHasher::new("snapdir ctx")
        .hash_file_hex(&path)
        .unwrap()
        .0;
    assert_ne!(unkeyed, keyed, "keyed file hash must differ from unkeyed");
}

// ===========================================================================
// 4. mmap-threshold boundary: THRESHOLD-1 / THRESHOLD / THRESHOLD+1 hash
//    identically whether the mmap branch or the plain-read branch is taken.
// ===========================================================================

#[test]
fn mmap_threshold_boundary_blake3_identical_both_branches() {
    // Spec clause: mmap-threshold BOUNDARY. Files of exactly MMAP_THRESHOLD-1,
    // MMAP_THRESHOLD, and MMAP_THRESHOLD+1 bytes must hash identically to the
    // blake3 one-shot of the same bytes (i.e. the mmap branch and the plain-read
    // branch agree across the const boundary). MMAP_THRESHOLD referenced via the
    // contracted path so this fails to compile until the impl lands.
    let threshold = MMAP_THRESHOLD;
    assert_eq!(threshold, 256 * 1024, "MMAP_THRESHOLD contract = 256 KiB");

    let blake3 = Blake3Hasher::new();
    for delta in [-1i64, 0, 1] {
        let len = (threshold as i64 + delta) as usize;
        let content = deterministic_bytes(len);
        let (_s, path) = scratch_file(&format!("boundary_{len}"), &content);

        let (hex, byte_len) = blake3.hash_file_hex(&path).unwrap();
        // Independent oracle: a single one-shot blake3 over the same bytes.
        let oneshot = ::blake3::hash(&content).to_hex().to_string();
        assert_eq!(
            hex, oneshot,
            "blake3 hash_file_hex must equal one-shot blake3 at len {len}"
        );
        // And it must equal hash_hex(read) too (the two engines must agree).
        assert_eq!(hex, blake3.hash_hex(&content));
        assert_eq!(byte_len, len as u64);
    }
}

// ===========================================================================
// 5. Empty file: blake3 hash_file_hex == known empty-input BLAKE3 and never
//    mmapped (must not panic / SIGBUS).
// ===========================================================================

#[test]
fn empty_file_blake3_hash_is_known_constant_and_never_mmapped() {
    // Spec clause: a 0-byte file's blake3 hash_file_hex == the known empty-input
    // BLAKE3, and must never be mmapped (mmap of 0 bytes can SIGBUS). The fact
    // that this returns cleanly (no SIGBUS) is itself the "never mmapped" proof.
    const EMPTY_BLAKE3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    let (_s, path) = scratch_file("empty", &[]);
    let (hex, len) = Blake3Hasher::new().hash_file_hex(&path).unwrap();
    assert_eq!(hex, EMPTY_BLAKE3, "empty blake3 must be the known constant");
    assert_eq!(len, 0, "empty file byte len must be 0");
    // Empty file is strictly below MMAP_THRESHOLD, so it takes the plain-read branch.
    assert!(0 < MMAP_THRESHOLD, "0 bytes is below the mmap threshold");
}

// ===========================================================================
// 6. walk_jobs determinism — walk_jobs = Some(1) vs Some(8) yield the identical
//    snapshot_id (and Manifest) for a tree mixing many small + a few large files.
// ===========================================================================

/// Builds a tree mixing many small files + a few large (> MMAP_THRESHOLD) files,
/// for the cross-job determinism check.
fn materialize_small_and_large(dir: &Path) {
    for i in 0..40 {
        write_file(
            &dir.join(format!("small_{i:03}.bin")),
            &deterministic_bytes(64),
        );
    }
    for i in 0..3 {
        write_file(
            &dir.join(format!("large_{i:02}.bin")),
            &deterministic_bytes(MMAP_THRESHOLD as usize + 4096),
        );
    }
    let sub = dir.join("sub");
    mkdir(&sub);
    write_file(&sub.join("nested small.bin"), &deterministic_bytes(10));
    write_file(
        &sub.join("nested large.bin"),
        &deterministic_bytes(MMAP_THRESHOLD as usize * 2),
    );
}

#[test]
fn walk_jobs_1_vs_8_yield_identical_id_and_manifest() {
    // Spec clause: walk_jobs determinism. WalkOptions.walk_jobs = Some(1) vs
    // Some(8) must produce identical snapshot_id AND identical Manifest Display
    // for a many-small + few-large tree. References WalkOptions.walk_jobs so it
    // cannot compile until the field lands.
    let scratch = Scratch::new("walk_jobs");
    materialize_small_and_large(scratch.path());

    let hasher = Blake3Hasher::new();
    let opts_1 = WalkOptions {
        walk_jobs: Some(1),
        ..WalkOptions::default()
    };
    let opts_8 = WalkOptions {
        walk_jobs: Some(8),
        ..WalkOptions::default()
    };

    let m1 = walk(scratch.path(), &opts_1, &hasher).expect("walk jobs=1");
    let m8 = walk(scratch.path(), &opts_8, &hasher).expect("walk jobs=8");

    assert_eq!(
        m1.to_string(),
        m8.to_string(),
        "Manifest Display must be identical across walk_jobs"
    );
    assert_eq!(
        snapshot_id(&m1, &hasher),
        snapshot_id(&m8, &hasher),
        "snapshot_id must be identical across walk_jobs"
    );

    // And both must match the default (None) walk over the same tree.
    let m_default = walk(scratch.path(), &WalkOptions::default(), &hasher).expect("walk default");
    assert_eq!(snapshot_id(&m1, &hasher), snapshot_id(&m_default, &hasher));
}

#[test]
fn walk_jobs_does_not_change_golden_shapes() {
    // Spec corollary: setting walk_jobs must NOT change the KEYSTONE goldens.
    // Re-run every Shape under walk_jobs = Some(4) and assert the golden holds.
    let hasher = Blake3Hasher::new();
    for shape in ALL_SHAPES {
        let scratch = Scratch::new(shape.name());
        shape.materialize(scratch.path());
        let opts = WalkOptions {
            walk_jobs: Some(4),
            ..WalkOptions::default()
        };
        let manifest = walk(scratch.path(), &opts, &hasher).expect("walk jobs=4");
        assert_eq!(
            snapshot_id(&manifest, &hasher),
            golden_id(shape),
            "walk_jobs=4 changed the golden for {}",
            shape.name()
        );
    }
}

// ===========================================================================
// 7. REVIEW-MODE additions (impl now visible). The landed walk has TWO distinct
//    hashing engines selected by `pending.len() >= jobs` (walk.rs hash_pending):
//      - fewer pending files than jobs -> `hash_file_hex` (blake3 `update_mmap_rayon`,
//        intra-file rayon fan-out on a lone big file);
//      - at least `jobs` pending files  -> `hash_file_hex_seq` (blake3 `update_mmap`,
//        single-threaded per file).
//    Both MUST yield byte-identical ids. These cases drive each branch on purpose
//    and pin the NEW `hash_file_hex_seq` symbol the impl revealed.
// ===========================================================================

/// One huge file (> MMAP_THRESHOLD), ALONE in a dir. With a high `walk_jobs`
/// (so `pending.len() (==1) < walk_jobs`) the walk takes the intra-file
/// `update_mmap_rayon` branch; with `walk_jobs = Some(1)` it takes the
/// single-threaded engine. Both ids must match each other AND the default walk.
#[test]
fn single_huge_file_intra_file_rayon_matches_seq() {
    // Spec clause (review): the intra-file `update_mmap_rayon` path (pending < jobs)
    // must produce the SAME id as the sequential/`walk_jobs=1` path for one big file.
    let scratch = Scratch::new("huge_alone");
    // Strictly above the threshold so the mmap branch (not plain-read) is taken,
    // and multi-MB so update_mmap_rayon genuinely has work to fan out.
    let content = deterministic_bytes(MMAP_THRESHOLD as usize * 12 + 123);
    write_file(&scratch.path().join("only_big.bin"), &content);

    let hasher = Blake3Hasher::new();
    // Many jobs, ONE pending file => pending.len() (1) < jobs => update_mmap_rayon.
    let opts_rayon = WalkOptions {
        walk_jobs: Some(8),
        ..WalkOptions::default()
    };
    // One job => the honest single-threaded engine (update_mmap, no pool).
    let opts_seq = WalkOptions {
        walk_jobs: Some(1),
        ..WalkOptions::default()
    };

    let m_rayon = walk(scratch.path(), &opts_rayon, &hasher).expect("walk rayon branch");
    let m_seq = walk(scratch.path(), &opts_seq, &hasher).expect("walk seq branch");
    let m_default = walk(scratch.path(), &WalkOptions::default(), &hasher).expect("walk default");

    assert_eq!(
        m_rayon.to_string(),
        m_seq.to_string(),
        "intra-file rayon manifest must equal the single-threaded manifest"
    );
    assert_eq!(
        snapshot_id(&m_rayon, &hasher),
        snapshot_id(&m_seq, &hasher),
        "intra-file rayon id must equal the single-threaded id"
    );
    assert_eq!(
        snapshot_id(&m_rayon, &hasher),
        snapshot_id(&m_default, &hasher),
        "intra-file rayon id must equal the default-walk id"
    );

    // And the lone big file's checksum must equal a one-shot blake3 of its bytes
    // (the manifest row carries the per-file content hash).
    let oneshot = ::blake3::hash(&content).to_hex().to_string();
    assert!(
        m_rayon.to_string().contains(&oneshot),
        "the big file's row must carry the one-shot blake3 of its bytes"
    );
}

/// A tree with FAR more files than worker threads (`pending.len() >= jobs`) so the
/// cross-file rayon pool + per-file `hash_file_hex_seq` (`update_mmap` for the big
/// ones) branch is exercised. Determinism + golden-stability across job counts.
#[test]
fn many_files_cross_file_seq_branch_is_deterministic() {
    // Spec clause (review): the cross-file rayon + per-file `hash_file_hex_seq`
    // (update_mmap) path (pending >= jobs) must be deterministic and id-stable
    // across job counts, including some large files forced through update_mmap.
    let scratch = Scratch::new("many_files_seq");
    // 50 small files (sub-threshold, plain-read) ...
    for i in 0..50 {
        write_file(
            &scratch.path().join(format!("s_{i:03}.bin")),
            &deterministic_bytes(48 + (i % 7)),
        );
    }
    // ... plus several > MMAP_THRESHOLD files so the per-file update_mmap engine
    // (the seq branch, NOT update_mmap_rayon) hashes a big file. With many pending
    // files and only a few jobs, pending.len() >= jobs holds for every count below.
    for i in 0..4 {
        write_file(
            &scratch.path().join(format!("big_{i:02}.bin")),
            &deterministic_bytes(MMAP_THRESHOLD as usize + 1024 * (i + 1)),
        );
    }

    let hasher = Blake3Hasher::new();
    // job counts well below the file count (54), so pending >= jobs => seq engine.
    let mut ids = Vec::new();
    for jobs in [1usize, 2, 4, 8] {
        let opts = WalkOptions {
            walk_jobs: Some(jobs),
            ..WalkOptions::default()
        };
        let m = walk(scratch.path(), &opts, &hasher).expect("walk seq branch");
        ids.push(snapshot_id(&m, &hasher));
    }
    for id in &ids {
        assert_eq!(
            id, &ids[0],
            "cross-file seq-branch id must be identical across job counts"
        );
    }
    // Stable across a re-run too (golden-stability without a hardcoded value:
    // the tree mixes sizes the bench Shapes don't, so the id is computed, not
    // pinned — but it MUST be reproducible).
    let opts1 = WalkOptions {
        walk_jobs: Some(1),
        ..WalkOptions::default()
    };
    let rerun = snapshot_id(
        &walk(scratch.path(), &opts1, &hasher).expect("rerun"),
        &hasher,
    );
    assert_eq!(
        rerun, ids[0],
        "seq-branch id must be reproducible across runs"
    );
}

/// Pins the NEW symbol the impl revealed: `HashFile::hash_file_hex_seq`. It must
/// produce hex byte-identical to `hash_file_hex` (the rayon path) AND to a one-shot
/// blake3, across the MMAP_THRESHOLD boundary (-1 / 0 / +1 and well above).
#[test]
fn hash_file_hex_seq_equals_rayon_and_oneshot_across_threshold() {
    // Spec clause (review): hash_file_hex_seq (update_mmap / non-rayon mmap) ==
    // hash_file_hex (update_mmap_rayon) == one-shot blake3, for every size around
    // and above the threshold. The seq engine is a perf variant ONLY; its bytes
    // must never diverge.
    let blake3 = Blake3Hasher::new();
    let threshold = MMAP_THRESHOLD as usize;
    let sizes = [
        0usize,
        1,
        threshold - 1,
        threshold,
        threshold + 1,
        threshold * 4 + 7,
    ];
    for len in sizes {
        let content = deterministic_bytes(len);
        let (_s, path) = scratch_file(&format!("seq_eq_{len}"), &content);

        let (rayon_hex, rayon_len) = blake3.hash_file_hex(&path).expect("hash_file_hex");
        let (seq_hex, seq_len) = blake3.hash_file_hex_seq(&path).expect("hash_file_hex_seq");
        let oneshot = ::blake3::hash(&content).to_hex().to_string();

        assert_eq!(
            seq_hex, rayon_hex,
            "hash_file_hex_seq must equal hash_file_hex at len {len}"
        );
        assert_eq!(
            seq_hex, oneshot,
            "hash_file_hex_seq must equal one-shot blake3 at len {len}"
        );
        assert_eq!(seq_len, len as u64, "seq byte len at {len}");
        assert_eq!(rayon_len, len as u64, "rayon byte len at {len}");
    }
}

// ===========================================================================
// 8. walk_jobs = Some(0) (auto) must be deterministic and == Some(1).
// ===========================================================================

#[test]
fn walk_jobs_auto_zero_matches_one_and_default() {
    // Spec clause (review): `Some(0)` resolves to the auto (available_parallelism,
    // capped) worker count — it must still be deterministic and produce the
    // identical id as `Some(1)` and the default walk. (walk.rs resolve_jobs treats
    // Some(0) like None.)
    let scratch = Scratch::new("auto_zero");
    materialize_small_and_large(scratch.path());

    let hasher = Blake3Hasher::new();
    let opts_auto = WalkOptions {
        walk_jobs: Some(0),
        ..WalkOptions::default()
    };
    let opts_one = WalkOptions {
        walk_jobs: Some(1),
        ..WalkOptions::default()
    };

    let m_auto = walk(scratch.path(), &opts_auto, &hasher).expect("walk jobs=0");
    let m_one = walk(scratch.path(), &opts_one, &hasher).expect("walk jobs=1");
    let m_default = walk(scratch.path(), &WalkOptions::default(), &hasher).expect("walk default");

    assert_eq!(
        m_auto.to_string(),
        m_one.to_string(),
        "walk_jobs=Some(0) (auto) manifest must equal Some(1)"
    );
    assert_eq!(
        snapshot_id(&m_auto, &hasher),
        snapshot_id(&m_one, &hasher),
        "walk_jobs=Some(0) id must equal Some(1)"
    );
    assert_eq!(
        snapshot_id(&m_auto, &hasher),
        snapshot_id(&m_default, &hasher),
        "walk_jobs=Some(0) id must equal the default (None) walk"
    );

    // Auto is itself stable run-over-run (no thread-count nondeterminism).
    let rerun = walk(scratch.path(), &opts_auto, &hasher).expect("rerun jobs=0");
    assert_eq!(snapshot_id(&m_auto, &hasher), snapshot_id(&rerun, &hasher));
}

// ===========================================================================
// 9. Symlink-followed file: the walk follows symlinks by DEFAULT (find -L). A
//    followed symlink-to-file hashes the TARGET's content (checksum read through
//    the link), while its SIZE column is the symlink's own lstat length. The
//    parallel hasher must hash through the link, not the symlink bytes. (Pins the
//    `content_path = entry_path` + `size = link_meta.len()` split in walk.rs.)
// ===========================================================================

#[cfg(unix)]
#[test]
fn followed_symlink_file_hashes_target_content() {
    // Spec clause (review): a symlink-to-file, followed by default, appears as an
    // `F` row whose CHECKSUM is the blake3 of the TARGET's bytes (hashed through
    // the link by the parallel hasher), proving the walk hashes the target, not
    // the symlink. Drive a > MMAP_THRESHOLD target so the followed link is hashed
    // through the mmap path.
    let scratch = Scratch::new("symlink_file");
    let root = scratch.path();

    // A real target file, large enough to take the mmap branch through the link.
    let target_content = deterministic_bytes(MMAP_THRESHOLD as usize + 4096);
    write_file(&root.join("real.bin"), &target_content);

    // A symlink to it (relative target name, same dir).
    std::os::unix::fs::symlink("real.bin", root.join("link.bin")).expect("symlink file");

    let hasher = Blake3Hasher::new();
    // Force the cross-file seq engine AND the intra-file rayon engine; both must
    // hash the target through the link to the same checksum.
    let target_hex = ::blake3::hash(&target_content).to_hex().to_string();
    for jobs in [Some(1usize), Some(8), Some(0), None] {
        let opts = WalkOptions {
            walk_jobs: jobs,
            ..WalkOptions::default()
        };
        let manifest = walk(root, &opts, &hasher).expect("walk follows symlinks by default");
        let text = manifest.to_string();
        // The real file row.
        assert!(
            text.lines().any(|l| l.starts_with("F ")
                && l.contains(&target_hex)
                && l.ends_with(" ./real.bin")),
            "real file row must carry the target's blake3 [{jobs:?}]: {text}"
        );
        // The followed symlink row: SAME content checksum (hashed through the link).
        assert!(
            text.lines().any(|l| l.starts_with("F ")
                && l.contains(&target_hex)
                && l.ends_with(" ./link.bin")),
            "followed symlink row must carry the TARGET's blake3 [{jobs:?}]: {text}"
        );
    }
}
