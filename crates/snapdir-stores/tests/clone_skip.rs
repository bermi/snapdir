//! Adversarial integration suite for the "skip the redundant post-copy
//! re-hash on the clone path" optimization (phase 29, gate
//! `clone-skip-spec-tests`).
//!
//! BLACK-BOX: authored from the gate SPEC alone, with NO visibility into the
//! private `copy_file` / `persist` internals being changed (the skip path does
//! not exist yet). It will NOT compile/pass until the stores-impl teammate
//! moves this file into `crates/snapdir-stores/tests/clone_skip.rs`, adds the
//! `CopyGuard` walk side-channel + `CopyMethod` trust-enum + the
//! `SNAPDIR_VERIFY_COPIES` knob, and lands the feature. Do NOT weaken any
//! assertion to make it green — if a behavior here fails against the landed
//! impl, that is a real bug in the impl, not in this test.
//!
//! ## SPEC under test
//!
//! Today `persist()` ALWAYS re-hashes the temp after `copy_file`, so the macOS
//! APFS `clonefile` fast-path buys ~0 wall-clock. The impl SKIPS that re-hash
//! when the copy was a CoW clone:
//!   * FETCH/checkout (source = an immutable, read-verified store object): skip
//!     whenever cloned — provably safe.
//!   * STAGE/push (source = a mutable user file): skip only under
//!     "stat-validated trust" — the walk records each file's
//!     `(size, mtime, ctime, ino)` into a `CopyGuard`; `persist` re-stats the
//!     source at clone time and skips the re-hash IFF unchanged, else falls
//!     back to the re-hash (-> Integrity error if the changed source != the
//!     expected checksum).
//!   * The `fs::copy` (non-clone) path ALWAYS re-hashes — unchanged.
//!
//! The skip MUST NOT introduce a silently mis-addressed object: an object
//! filed under checksum(A) that actually contains bytes(B) and is ACCEPTED on
//! read is the single forbidden outcome.
//!
//! ## Contracted symbols / env knobs (impl lane MUST match these names)
//!
//! - **`SNAPDIR_CLONEFILE=0`** (EXISTS) — disables clone entirely -> the
//!   `fs::copy` path, which always re-hashes. Used as the "always-rehash"
//!   control arm.
//! - **`SNAPDIR_VERIFY_COPIES=1`** (NEW) — forces the write-time re-hash even
//!   on the clone path (strict mode). Unset / `0` = the optimization is live
//!   (skip the re-hash on a trusted clone). When set to `1`, a clone still
//!   FIRES (so `clonefile_hits()` still advances) but `persist` re-hashes the
//!   cloned temp exactly as the `fs::copy` path does — proving the override is
//!   write-time strict, not a clone-disable.
//! - **`snapdir_stores::clonefile_hits() -> u64`** (EXISTS) — process-global
//!   clone-fire counter; used only to prove the clone still fired under the
//!   skip + under VERIFY_COPIES (cfg macos).
//!
//! The internal `CopyMethod` / trust-enum / `CopyGuard` are impl details; this
//! suite pins their EFFECT through the public `FileStore` API + env only (it
//! does not import them — they may be `pub(crate)`).
//!
//! ## How the RACE KEYSTONE is asserted (never silent)
//!
//! `push` reads its object bytes from the live source files; `fetch_files` /
//! `get_object` BLAKE3-verify on read. So to force the dangerous case through
//! the PUBLIC API we build a manifest whose entry records checksum(A) while the
//! on-disk source file actually holds bytes(B) at `push` time. Whichever way
//! the impl's stat-guard goes, the safe outcomes are EXACTLY:
//!   (1) `push` returns `StoreError::Integrity` (guard re-stats, sees the
//!       change vs the recorded `(size,mtime,ctime,ino)` -> re-hashes -> the B
//!       bytes != checksum(A)), OR
//!   (2) `push` succeeds BUT a later `get_object(checksum(A))` /
//!       `fetch_files` rejects with `StoreError::Integrity` (read-time BLAKE3
//!       verify is the backstop).
//! The FORBIDDEN outcome the test fails on: an object readable at
//! checksum(A) whose bytes are B. The assertion holds whether or not the
//! stat-guard fires, so it is robust to the impl's trust decision.
//!
//! ## Env / parallelism note
//!
//! `SNAPDIR_CLONEFILE` and `SNAPDIR_VERIFY_COPIES` are process-global and Rust
//! runs `#[test]`s multithreaded in one binary, so every test that touches a
//! knob (or compares a `clonefile_hits()` delta) holds a single process-wide
//! `ENV_LOCK` for its whole body and RESTORES the prior values on drop, so a
//! parallel test never observes a half-set knob.

// Wiring (shape only, no assertion change): silence workspace `-D warnings`
// clippy lints on this adversary-authored suite.
#![allow(
    unused_imports,
    clippy::type_complexity,
    clippy::single_match_else,
    clippy::single_match,
    clippy::manual_let_else,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::similar_names,
    dead_code
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};
use snapdir_core::CopyGuard;

use std::collections::HashMap;

use snapdir_stores::{FileStore, StreamStore};

// ---------------------------------------------------------------------------
// Test scaffolding (no dev-dependencies; mirrors apfs_clone.rs).
// ---------------------------------------------------------------------------

/// A unique temp dir removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "snapdir-clone-skip-test-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Process-global lock guarding `SNAPDIR_CLONEFILE` + `SNAPDIR_VERIFY_COPIES`
/// (both process-global) and the process-global `clonefile_hits()` counter.
fn env_lock() -> MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII guard that sets/clears `SNAPDIR_CLONEFILE` and `SNAPDIR_VERIFY_COPIES`
/// and restores the prior values on drop. Caller must already hold `env_lock()`.
struct CopyModeEnv {
    prev_clone: Option<String>,
    prev_verify: Option<String>,
}

impl CopyModeEnv {
    /// `clonefile`: `Some("0")` disables the clone fast-path entirely (forces
    /// `fs::copy` + always-rehash); `None` leaves it default-enabled.
    /// `verify`: `Some("1")` forces the write-time re-hash even on the clone
    /// path (strict); `None` leaves the skip optimization live.
    fn set(clonefile: Option<&str>, verify: Option<&str>) -> Self {
        let prev_clone = std::env::var("SNAPDIR_CLONEFILE").ok();
        let prev_verify = std::env::var("SNAPDIR_VERIFY_COPIES").ok();
        match clonefile {
            Some(v) => std::env::set_var("SNAPDIR_CLONEFILE", v),
            None => std::env::remove_var("SNAPDIR_CLONEFILE"),
        }
        match verify {
            Some(v) => std::env::set_var("SNAPDIR_VERIFY_COPIES", v),
            None => std::env::remove_var("SNAPDIR_VERIFY_COPIES"),
        }
        Self {
            prev_clone,
            prev_verify,
        }
    }
}

impl Drop for CopyModeEnv {
    fn drop(&mut self) {
        match &self.prev_clone {
            Some(v) => std::env::set_var("SNAPDIR_CLONEFILE", v),
            None => std::env::remove_var("SNAPDIR_CLONEFILE"),
        }
        match &self.prev_verify {
            Some(v) => std::env::set_var("SNAPDIR_VERIFY_COPIES", v),
            None => std::env::remove_var("SNAPDIR_VERIFY_COPIES"),
        }
    }
}

/// The three observable copy modes the SPEC compares. All three MUST produce
/// identical object pools + restored content + snapshot id for honest input.
#[derive(Clone, Copy)]
enum Mode {
    /// Default: clone fast-path enabled, skip optimization live.
    Default,
    /// `SNAPDIR_CLONEFILE=0`: `fs::copy` + always re-hash.
    CloneOff,
    /// `SNAPDIR_VERIFY_COPIES=1`: clone fires but the re-hash is forced.
    VerifyCopies,
}

impl Mode {
    fn env(self) -> CopyModeEnv {
        match self {
            Mode::Default => CopyModeEnv::set(None, None),
            Mode::CloneOff => CopyModeEnv::set(Some("0"), None),
            Mode::VerifyCopies => CopyModeEnv::set(None, Some("1")),
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::CloneOff => "cloneoff",
            Mode::VerifyCopies => "verify",
        }
    }
}

const ALL_MODES: [Mode; 3] = [Mode::Default, Mode::CloneOff, Mode::VerifyCopies];

/// Writes a real source tree under `src` and returns the matching `Manifest`
/// plus its snapshot id. `files` is `(relative path, content, octal-mode-str)`.
/// Object addressing uses the NON-keyed `Blake3Hasher` (the content-address
/// hasher the file store files objects under), as the shipped tests do.
fn build_tree(src: &Path, files: &[(&str, &[u8], &str)]) -> (Manifest, String) {
    let hasher = Blake3Hasher::new();
    let mut manifest = Manifest::new();

    let mut file_sums: Vec<String> = Vec::new();
    for (rel, content, mode) in files {
        let target = src.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&target, content).unwrap();
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(u32::from_str_radix(mode, 8).unwrap());
            fs::set_permissions(&target, perms).unwrap();
        }
        let sum = hasher.hash_hex(content);
        file_sums.push(sum.clone());
        manifest.push(ManifestEntry::new(
            PathType::File,
            *mode,
            sum,
            content.len() as u64,
            format!("./{rel}"),
        ));
    }

    let root_sum = directory_checksum(file_sums.iter().map(String::as_str), &hasher);
    let root_size: u64 = files.iter().map(|(_, c, _)| c.len() as u64).sum();
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        root_size,
        "./",
    ));

    manifest.sort();
    let id = snapshot_id(&manifest, &hasher);
    (manifest, id)
}

/// Counts the regular files under `<root>/.objects`.
fn count_objects(root: &Path) -> usize {
    fn walk(dir: &Path, acc: &mut usize) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, acc);
                } else if p.is_file() {
                    *acc += 1;
                }
            }
        }
    }
    let mut n = 0;
    walk(&root.join(".objects"), &mut n);
    n
}

/// Collects every `.objects` blob under `root` as `(sharded relative path,
/// bytes)`, sorted by path.
fn object_inventory(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, acc: &mut Vec<(String, Vec<u8>)>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(base, &p, acc);
                } else if p.is_file() {
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                    acc.push((rel, fs::read(&p).unwrap()));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &root.join(".objects"), &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// On-disk sharded path of an object blob under `root`.
fn object_disk(root: &Path, checksum: &str) -> PathBuf {
    root.join(object_path(checksum))
}

/// On-disk sharded path of a manifest under `root`.
fn manifest_disk(root: &Path, id: &str) -> PathBuf {
    root.join(manifest_path(id))
}

/// The unix mode bits (permission portion) of `path`, or `None` off-unix.
#[cfg(unix)]
fn mode_bits(path: &Path) -> Option<u32> {
    Some(fs::metadata(path).ok()?.permissions().mode() & 0o7777)
}
#[cfg(not(unix))]
fn mode_bits(_path: &Path) -> Option<u32> {
    None
}

/// `true` iff `e` is a `StoreError::Integrity`.
fn is_integrity(e: &StoreError) -> bool {
    matches!(e, StoreError::Integrity { .. })
}

/// A representative tree exercising the boundary sizes the SPEC calls out: a
/// file LARGER than 256 KiB, a tiny file, a 0-byte file, a nested deep path,
/// and a unicode/space path. Contents are deterministic so checksums are stable.
fn mixed_size_files() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let big: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    vec![
        ("big.bin", big, "644"),
        ("tiny.txt", b"x".to_vec(), "644"),
        ("empty", Vec::new(), "644"),
        (
            "nested/deep/leaf.bin",
            vec![0u8, 1, 2, 3, 255, 254, 0],
            "600",
        ),
        (
            "uni \u{2728}/space name.txt",
            "snowman \u{2603}\n".as_bytes().to_vec(),
            "644",
        ),
    ]
}

// ===========================================================================
// CASE 1 — FETCH skip: observable identity across the three modes.
// Spec clause: "FETCH/checkout skip whenever cloned — provably safe. Assert all
// three produce IDENTICAL restored content and the SAME object pool + SAME
// snapshot id."
// ===========================================================================

/// One full stage->object->checkout cycle under `mode`, returning
/// (object-pool inventory, restored-content map, restored-mode map, id).
fn roundtrip_once(
    mode: Mode,
    files: &[(&str, &[u8], &str)],
) -> (
    Vec<(String, Vec<u8>)>,
    Vec<(String, Vec<u8>)>,
    Vec<(String, Option<u32>)>,
    String,
) {
    let store_dir = TempDir::new(&format!("rt-store-{}", mode.tag()));
    let src = TempDir::new(&format!("rt-src-{}", mode.tag()));
    let dest = TempDir::new(&format!("rt-dest-{}", mode.tag()));

    let (manifest, id) = build_tree(src.path(), files);
    let store = FileStore::from_root(store_dir.path().to_path_buf());
    store.push(&manifest, src.path()).expect("push");
    store
        .fetch_files(&manifest, dest.path())
        .expect("fetch_files");

    let inv = object_inventory(store_dir.path());

    let mut restored: Vec<(String, Vec<u8>)> = Vec::new();
    let mut modes: Vec<(String, Option<u32>)> = Vec::new();
    for (rel, _, _) in files {
        let p = dest.path().join(rel);
        restored.push(((*rel).to_string(), fs::read(&p).expect("restored file")));
        modes.push(((*rel).to_string(), mode_bits(&p)));
    }
    restored.sort_by(|a, b| a.0.cmp(&b.0));
    modes.sort_by(|a, b| a.0.cmp(&b.0));

    (inv, restored, modes, id)
}

#[test]
fn fetch_skip_identical_objects_content_and_id_across_three_modes() {
    // Spec clause (FETCH skip — observable identity): default (clone+skip),
    // SNAPDIR_CLONEFILE=0 (fs::copy+rehash), and SNAPDIR_VERIFY_COPIES=1
    // (clone+forced rehash) must produce IDENTICAL restored content, the SAME
    // object pool (sharded filenames + bytes), and the SAME snapshot id.
    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    let runs: Vec<_> = ALL_MODES
        .iter()
        .map(|m| {
            let _e = m.env();
            roundtrip_once(*m, &files)
        })
        .collect();

    let base = &runs[0];
    for (i, run) in runs.iter().enumerate().skip(1) {
        assert_eq!(
            run.0, base.0,
            "mode {i}: object pool (sharded filenames + bytes) must be byte-identical \
             to the default/skip mode across all three copy modes"
        );
        assert_eq!(
            run.1, base.1,
            "mode {i}: restored file content must be identical across all three modes"
        );
        assert_eq!(
            run.2, base.2,
            "mode {i}: restored permission bits must be identical across all three modes"
        );
        assert_eq!(
            run.3, base.3,
            "mode {i}: snapshot id must round-trip identically across all three modes"
        );
    }

    // Sanity: the >256 KiB object is really present full-length (so identity is
    // not vacuously over only tiny inputs).
    let big_sum = Blake3Hasher::new().hash_hex(&files_owned[0].1);
    let big_rel = object_path(&big_sum);
    assert!(
        base.0
            .iter()
            .any(|(rel, bytes)| *rel == big_rel && bytes.len() == files_owned[0].1.len()),
        "the >256 KiB object must be present in the pool at full length"
    );
}

// ===========================================================================
// CASE 2 — STAGE skip: observable identity of the PUSH object pool across the
// three modes (no fetch — isolates the stage/push copy path the skip changes).
// Spec clause: "STAGE skip — same three-way comparison for the object pool
// produced by push/stage: identical sharded object files + bytes + id."
// ===========================================================================

#[test]
fn stage_skip_identical_object_pool_and_id_across_three_modes() {
    // Spec clause (STAGE skip — observable identity): the object pool produced
    // by push (stage) must be byte-identical (sharded filenames + bytes) and
    // carry the same snapshot id across default / CLONEFILE=0 / VERIFY_COPIES=1.
    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    let runs: Vec<(Vec<(String, Vec<u8>)>, String, bool)> = ALL_MODES
        .iter()
        .map(|m| {
            let _e = m.env();
            let store_dir = TempDir::new(&format!("stage-store-{}", m.tag()));
            let src = TempDir::new(&format!("stage-src-{}", m.tag()));
            let (manifest, id) = build_tree(src.path(), &files);
            let store = FileStore::from_root(store_dir.path().to_path_buf());
            store.push(&manifest, src.path()).expect("push");
            let inv = object_inventory(store_dir.path());
            let manifest_committed = manifest_disk(store_dir.path(), &id).is_file();
            (inv, id, manifest_committed)
        })
        .collect();

    let base = &runs[0];
    for (i, run) in runs.iter().enumerate().skip(1) {
        assert_eq!(
            run.0, base.0,
            "mode {i}: stage object pool must be byte-identical across all three copy modes"
        );
        assert_eq!(
            run.1, base.1,
            "mode {i}: stage snapshot id must be identical across all three copy modes"
        );
    }
    for (i, run) in runs.iter().enumerate() {
        assert!(
            run.2,
            "mode {i}: the manifest must be committed (manifest-last) after a clean stage"
        );
    }
}

// ===========================================================================
// CASE 3 — RACE KEYSTONE (the dangerous one). A manifest entry records
// checksum(A); the on-disk source actually holds bytes(B) at push time. The
// skip must NEVER yield an object readable at checksum(A) whose bytes are B.
// Spec clause: "Assert NO silently mis-addressed object is ever accepted:
// EITHER push fails with StoreError::Integrity, OR if an object lands a
// subsequent get_object(checksum_of_A)/fetch REJECTS it with Integrity."
// ===========================================================================

/// Drives the keystone for a given (B-size-vs-A) shape under `mode`, asserting
/// the never-silent invariant. `content_b_len_differs` toggles whether B has a
/// different length than A (size-changes are the easiest for a stat-guard to
/// catch; equal-length B is the harder forge this test deliberately includes).
fn assert_no_silent_misaddress(mode: Mode, content_b_len_differs: bool) {
    let store_dir = TempDir::new(&format!("race-store-{}", mode.tag()));
    let src = TempDir::new(&format!("race-src-{}", mode.tag()));

    let content_a: Vec<u8> = (0..(64 * 1024u32)).map(|i| (i % 211) as u8).collect();
    let content_b: Vec<u8> = if content_b_len_differs {
        (0..(96 * 1024u32))
            .map(|i| (i % 199) as u8 ^ 0x5a)
            .collect()
    } else {
        // Same length as A, different bytes (the hard case for a size-only guard).
        content_a.iter().map(|b| b ^ 0xff).collect()
    };
    assert_ne!(content_a, content_b, "A and B must differ");

    let hasher = Blake3Hasher::new();
    let sum_a = hasher.hash_hex(&content_a);
    let sum_b = hasher.hash_hex(&content_b);
    assert_ne!(sum_a, sum_b, "checksum(A) must differ from checksum(B)");

    let rel = "racey.bin";
    let target = src.path().join(rel);

    // Build the manifest entry over content A (records checksum(A) + len(A))...
    fs::write(&target, &content_a).unwrap();
    let mut manifest = Manifest::new();
    manifest.push(ManifestEntry::new(
        PathType::File,
        "644",
        sum_a.clone(),
        content_a.len() as u64,
        format!("./{rel}"),
    ));
    let root_sum = directory_checksum(std::iter::once(sum_a.as_str()), &hasher);
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        content_a.len() as u64,
        "./",
    ));
    manifest.sort();

    // ...then, BEFORE the store copy, overwrite the source with bytes B (the
    // mid-stage race the stat-guard is supposed to defend against).
    fs::write(&target, &content_b).unwrap();

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let push_res = store.push(&manifest, src.path());

    match push_res {
        Err(ref e) => {
            // Outcome (1): the guard saw the change -> re-hashed -> Integrity.
            assert!(
                is_integrity(e),
                "a mid-stage source change must surface as StoreError::Integrity, got {e:?}"
            );
            // And no usable object may be readable at checksum(A) holding B.
            match store.get_object(&sum_a) {
                Ok(bytes) => assert_ne!(
                    bytes, content_b,
                    "FORBIDDEN: object readable at checksum(A) but holding bytes(B) \
                     after a push that reported Integrity"
                ),
                Err(_) => {} // absent / integrity-rejected on read — both fine.
            }
        }
        Ok(()) => {
            // Outcome (2): an object landed; the read-time BLAKE3 backstop MUST
            // reject any attempt to read it as checksum(A) when it holds B.
            let read = store.get_object(&sum_a);
            match read {
                Ok(bytes) => {
                    assert_ne!(
                        bytes, content_b,
                        "FORBIDDEN: get_object(checksum(A)) returned bytes(B) — a silently \
                         mis-addressed object was accepted (the whole point of the test)"
                    );
                    // If it returned bytes at all they MUST hash to checksum(A).
                    assert_eq!(
                        Blake3Hasher::new().hash_hex(&bytes),
                        sum_a,
                        "an object returned at checksum(A) must actually hash to checksum(A)"
                    );
                }
                Err(e) => assert!(
                    is_integrity(&e) || matches!(e, StoreError::ObjectNotFound { .. }),
                    "reading checksum(A) of a raced stage must be Integrity-rejected or absent, \
                     got {e:?}"
                ),
            }
            // The backstop must also fire through fetch_files (the checkout path).
            let dest = TempDir::new(&format!("race-dest-{}", mode.tag()));
            if let Ok(()) = store.fetch_files(&manifest, dest.path()) {
                // If the checkout "succeeded", the restored file MUST be A, never B.
                let restored = fs::read(dest.path().join(rel)).unwrap_or_default();
                assert_ne!(
                    restored, content_b,
                    "FORBIDDEN: checkout restored bytes(B) under an entry addressed as A"
                );
            }
        }
    }
}

#[test]
fn race_keystone_no_silent_misaddress_size_change_default() {
    // Spec clause (RACE KEYSTONE): mid-stage source overwrite (B has a
    // DIFFERENT size than A) under the default skip mode must never yield a
    // silently mis-addressed object.
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None);
    assert_no_silent_misaddress(Mode::Default, true);
}

#[test]
fn race_keystone_no_silent_misaddress_same_size_default() {
    // Spec clause (RACE KEYSTONE, hard case): mid-stage overwrite where B has
    // the SAME size as A (a size-only guard cannot catch this) must still never
    // yield a silently mis-addressed object — the read-time BLAKE3 backstop
    // covers it.
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None);
    assert_no_silent_misaddress(Mode::Default, false);
}

#[test]
fn race_keystone_no_silent_misaddress_under_clone_off() {
    // Spec clause (RACE KEYSTONE): the fs::copy path ALWAYS re-hashes, so the
    // raced push MUST fail at write time with Integrity (no object lands).
    let _g = env_lock();
    let _e = CopyModeEnv::set(Some("0"), None);
    // CLONEFILE=0 forces fs::copy + re-hash on every object, so a same-size or
    // different-size B is caught at write time. Exercise both shapes.
    assert_no_silent_misaddress(Mode::CloneOff, true);
    assert_no_silent_misaddress(Mode::CloneOff, false);
}

// ===========================================================================
// CASE 4 — VERIFY_COPIES strict catches at WRITE time. With
// SNAPDIR_VERIFY_COPIES=1, the raced/mutated source stage MUST fail at write
// time with Integrity (the strict override re-hashes on the clone path).
// Spec clause: "With SNAPDIR_VERIFY_COPIES=1, the case-3 mutated-source stage
// MUST fail at write time (Integrity) — proving the strict override re-hashes
// on the clone path."
// ===========================================================================

#[test]
fn verify_copies_strict_catches_mutated_source_at_write_time() {
    // Spec clause (VERIFY_COPIES strict): a clone-path stage of a source that no
    // longer matches its recorded checksum MUST be rejected at WRITE time with
    // StoreError::Integrity when SNAPDIR_VERIFY_COPIES=1 — not deferred to read.
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, Some("1"));

    let store_dir = TempDir::new("verify-store");
    let src = TempDir::new("verify-src");

    let content_a: Vec<u8> = (0..(80 * 1024u32)).map(|i| (i % 223) as u8).collect();
    // Same length as A, different bytes — proves the strict re-hash is a true
    // content re-hash, not a size check.
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0xa5).collect();

    let hasher = Blake3Hasher::new();
    let sum_a = hasher.hash_hex(&content_a);

    let rel = "strict.bin";
    let target = src.path().join(rel);
    fs::write(&target, &content_a).unwrap();
    let mut manifest = Manifest::new();
    manifest.push(ManifestEntry::new(
        PathType::File,
        "644",
        sum_a.clone(),
        content_a.len() as u64,
        format!("./{rel}"),
    ));
    let root_sum = directory_checksum(std::iter::once(sum_a.as_str()), &hasher);
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        content_a.len() as u64,
        "./",
    ));
    manifest.sort();

    // Mutate the source before the copy.
    fs::write(&target, &content_b).unwrap();

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let res = store.push(&manifest, src.path());

    let err =
        res.expect_err("SNAPDIR_VERIFY_COPIES=1 must reject a mutated-source stage at write time");
    assert!(
        is_integrity(&err),
        "VERIFY_COPIES strict must fail with StoreError::Integrity, got {err:?}"
    );
    // No object addressed as A may be readable holding B.
    match store.get_object(&sum_a) {
        Ok(bytes) => assert_ne!(
            bytes, content_b,
            "FORBIDDEN: strict mode left a mis-addressed object readable at checksum(A)"
        ),
        Err(_) => {}
    }
}

// ===========================================================================
// CASE 5 — clonefile_hits still fires (cfg macos). The skip optimization must
// NOT disable cloning: a default stage AND a fetch on APFS still increment
// clonefile_hits(); VERIFY_COPIES=1 still clones (only the re-hash is forced).
// Spec clause: "clonefile_hits still fires: default stage on APFS still
// increments clonefile_hits() (skip doesn't disable cloning); a fetch likewise."
// ===========================================================================

#[cfg(target_os = "macos")]
#[test]
fn clonefile_still_fires_under_skip_and_under_verify_copies() {
    // Spec clause (clonefile_hits still fires): the skip + the strict override
    // must both leave the clone fast-path firing; only CLONEFILE=0 stops it.
    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    // Helper: push + fetch under `mode`, return the clone-hit delta.
    let run_delta = |mode: Mode| -> u64 {
        let _e = mode.env();
        let store_dir = TempDir::new(&format!("hits-store-{}", mode.tag()));
        let src = TempDir::new(&format!("hits-src-{}", mode.tag()));
        let dest = TempDir::new(&format!("hits-dest-{}", mode.tag()));
        let (manifest, _id) = build_tree(src.path(), &files);
        let store = FileStore::from_root(store_dir.path().to_path_buf());
        let before = snapdir_stores::clonefile_hits();
        store.push(&manifest, src.path()).expect("push");
        store
            .fetch_files(&manifest, dest.path())
            .expect("fetch_files");
        snapdir_stores::clonefile_hits() - before
    };

    // Default (skip live): clone still fires across push + fetch.
    let default_delta = run_delta(Mode::Default);
    assert!(
        default_delta >= 2,
        "default skip mode must STILL clone on both push and fetch \
         (skip does not disable cloning): delta={default_delta}"
    );

    // VERIFY_COPIES=1: clone still fires (only the re-hash is forced on).
    let verify_delta = run_delta(Mode::VerifyCopies);
    assert!(
        verify_delta >= 2,
        "SNAPDIR_VERIFY_COPIES=1 must still clone (it forces a re-hash, it does \
         NOT disable cloning): delta={verify_delta}"
    );

    // CLONEFILE=0: no clone fires (control arm).
    let off_delta = run_delta(Mode::CloneOff);
    assert_eq!(
        off_delta, 0,
        "SNAPDIR_CLONEFILE=0 must fire no clone: delta={off_delta}"
    );
}

// ===========================================================================
// CASE 6a — Degenerate: 0-byte file on the skip path. The skip branch must
// handle an empty source (no empty-file choke) and round-trip byte-correct
// under all three modes.
// Spec clause: "Degenerate: 0-byte file (skip path must handle it)."
// ===========================================================================

#[test]
fn zero_byte_file_skip_path_roundtrips_under_all_modes() {
    // Spec clause (degenerate 0-byte): the skip path must handle an empty
    // source; restored content + object pool + id identical across modes.
    let files: Vec<(&str, &[u8], &str)> = vec![("empty", b"".as_slice(), "644")];

    let _g = env_lock();

    let runs: Vec<_> = ALL_MODES
        .iter()
        .map(|m| {
            let _e = m.env();
            roundtrip_once(*m, &files)
        })
        .collect();

    let sum = Blake3Hasher::new().hash_hex(b"");
    let base = &runs[0];
    for (i, run) in runs.iter().enumerate() {
        assert_eq!(run.0, base.0, "mode {i}: 0-byte object pool must match");
        assert_eq!(
            run.1, base.1,
            "mode {i}: restored 0-byte content must match"
        );
        assert_eq!(run.3, base.3, "mode {i}: 0-byte snapshot id must match");
        // The empty object exists and is exactly empty.
        let empty_rel = object_path(&sum);
        assert!(
            run.0.iter().any(|(r, b)| *r == empty_rel && b.is_empty()),
            "mode {i}: the 0-byte object must be present and empty"
        );
        // Restored empty file is empty.
        assert!(
            run.1.iter().any(|(r, b)| r == "empty" && b.is_empty()),
            "mode {i}: restored 0-byte file must be empty"
        );
    }
}

// ===========================================================================
// CASE 6b — Degenerate: same-mtime, different-content. Two source files written
// "back to back" can share an mtime at coarse filesystem granularity. If the
// stage stat-guard trusts (size, mtime, ctime, ino) and skips the re-hash, an
// equal-size + same-mtime forge could in principle slip the guard — but the
// read-time BLAKE3 backstop must STILL prevent any silently mis-addressed
// object. (Deep TOCTOU/inode-reuse/`touch -t` fuzz is deferred to the later
// clone-skip-race-verify gate; this is the basic same-mtime case.)
// Spec clause: "a file whose mtime is ... equal across two writes
// (mtime-granularity) — assert no silent mis-address ... include a basic
// same-mtime-different-content case here."
// ===========================================================================

#[cfg(unix)]
#[test]
fn same_mtime_different_content_never_silently_misaddresses() {
    // Spec clause (degenerate same-mtime forge): record checksum(A) for a
    // source, then rewrite it with equal-length bytes(B) and FORCE the mtime
    // (and atime) back to A's recorded value, so a (size,mtime)-only guard would
    // be fooled. The push must still never produce an object readable at
    // checksum(A) holding B — caught at write time (full guard incl. ctime/ino)
    // OR at read time (BLAKE3 backstop).
    use std::os::unix::fs::MetadataExt;

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // default skip mode — the risky one

    let store_dir = TempDir::new("mtime-store");
    let src = TempDir::new("mtime-src");

    let content_a: Vec<u8> = (0..(40 * 1024u32)).map(|i| (i % 181) as u8).collect();
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0x3c).collect(); // same length
    assert_eq!(content_a.len(), content_b.len());

    let hasher = Blake3Hasher::new();
    let sum_a = hasher.hash_hex(&content_a);

    let rel = "twin.bin";
    let target = src.path().join(rel);

    // Write A, capture its mtime/atime, build the manifest over A.
    fs::write(&target, &content_a).unwrap();
    let md_a = fs::metadata(&target).unwrap();
    let a_mtime = filetime_pair(
        md_a.atime(),
        md_a.atime_nsec(),
        md_a.mtime(),
        md_a.mtime_nsec(),
    );

    let mut manifest = Manifest::new();
    manifest.push(ManifestEntry::new(
        PathType::File,
        "644",
        sum_a.clone(),
        content_a.len() as u64,
        format!("./{rel}"),
    ));
    let root_sum = directory_checksum(std::iter::once(sum_a.as_str()), &hasher);
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        content_a.len() as u64,
        "./",
    ));
    manifest.sort();

    // Overwrite with B (same length) and force the mtime/atime back to A's, so a
    // size+mtime-only check sees "unchanged".
    fs::write(&target, &content_b).unwrap();
    set_file_times(&target, a_mtime);

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let push_res = store.push(&manifest, src.path());

    // Never-silent invariant (same shape as the keystone): write-time Integrity,
    // or read-time rejection — never an object at checksum(A) holding B.
    match push_res {
        Err(ref e) => assert!(
            is_integrity(e),
            "a same-mtime same-size content forge that the full guard catches must \
             surface as Integrity, got {e:?}"
        ),
        Ok(()) => match store.get_object(&sum_a) {
            Ok(bytes) => {
                assert_ne!(
                    bytes, content_b,
                    "FORBIDDEN: get_object(checksum(A)) returned bytes(B) for a same-mtime forge"
                );
                assert_eq!(
                    Blake3Hasher::new().hash_hex(&bytes),
                    sum_a,
                    "any object returned at checksum(A) must hash to checksum(A)"
                );
            }
            Err(e) => assert!(
                is_integrity(&e) || matches!(e, StoreError::ObjectNotFound { .. }),
                "reading checksum(A) of a same-mtime forge must be Integrity-rejected or \
                 absent, got {e:?}"
            ),
        },
    }
}

// --- tiny in-test filetime helpers (no dev-dep; uses libc via raw syscall) ---
// We avoid pulling the `filetime` crate (the stores crate has no such dev-dep);
// the times are set via the libc `utimensat` syscall directly in the harness.
#[cfg(unix)]
#[derive(Clone, Copy)]
struct FileTimes {
    atime_sec: i64,
    atime_nsec: i64,
    mtime_sec: i64,
    mtime_nsec: i64,
}

#[cfg(unix)]
fn filetime_pair(atime_sec: i64, atime_nsec: i64, mtime_sec: i64, mtime_nsec: i64) -> FileTimes {
    FileTimes {
        atime_sec,
        atime_nsec,
        mtime_sec,
        mtime_nsec,
    }
}

#[cfg(unix)]
fn set_file_times(path: &Path, t: FileTimes) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    let times = [
        libc::timespec {
            tv_sec: t.atime_sec as libc::time_t,
            tv_nsec: t.atime_nsec as _,
        },
        libc::timespec {
            tv_sec: t.mtime_sec as libc::time_t,
            tv_nsec: t.mtime_nsec as _,
        },
    ];
    // AT_FDCWD = use the path relative to cwd (the path is absolute here anyway).
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(rc, 0, "utimensat must succeed to forge the mtime");
}

// ===========================================================================
// REVIEW-MODE ADDITIONS (gate `clone-skip-tests-review`). The StatGuarded skip
// is now live and `FileStore::with_copy_guards` is a public API; these cases
// exercise the now-visible branches the black-box authoring gate could not
// reach (it could not populate the guard map directly).
//
// Builds a `HashMap<PathBuf, CopyGuard>` from the LIVE metadata of each source
// file, keyed by the same absolute path `push` joins (`source.join(rel)`), so
// the StatGuarded SKIP genuinely engages.
// ===========================================================================

/// Builds the guard map `push` consumes (`source.join(rel)` keys) from the
/// CURRENT on-disk metadata of every regular file the manifest references.
#[cfg(unix)]
fn guards_from_sources(manifest: &Manifest, source: &Path) -> HashMap<PathBuf, CopyGuard> {
    let mut map = HashMap::new();
    for entry in manifest.entries() {
        if entry.path_type != PathType::File {
            continue;
        }
        let rel = entry.path.strip_prefix("./").unwrap_or(entry.path.as_str());
        let abs = source.join(rel);
        let meta = fs::metadata(&abs).expect("source file metadata");
        if let Some(g) = CopyGuard::from_metadata(&meta) {
            map.insert(abs, g);
        }
    }
    map
}

// ---------------------------------------------------------------------------
// REVIEW CASE A — TrustedObject DEVIATION ADJUDICATION: a CORRUPT store object
// must STILL be detected on the FETCH/checkout path on a clone-capable host.
//
// The stores coder did NOT implement the literal "zero-read fetch skip" the
// plan first described; instead `TrustedObject` re-hashes the SOURCE object
// once (catching on-disk corruption) and only skips the redundant TEMP re-hash.
// This case independently CONFIRMS that choice is *safer, not weaker*: it
// corrupts a committed store object in place, then fetches it on THIS APFS host
// (clone fast-path live, skip optimization on) and asserts the corruption is
// STILL rejected (Integrity) — i.e. clone-skip did NOT open a silent
// corrupt-checkout hole. If this ever FAILS, clone-skip opened a real hole and
// the impl gate `clone-skip-stores` must reopen.
// ---------------------------------------------------------------------------

#[test]
fn fetch_of_corrupted_store_object_still_detected_on_clone_path() {
    // Spec/adjudication clause: clone-skip's TrustedObject path must preserve
    // the store's verify-on-fetch corruption discipline. A store object whose
    // on-disk bytes no longer hash to its content address must NEVER be cloned
    // (skip) into a checkout — the source-verify must surface Integrity.
    let _g = env_lock();
    // Default mode: clone fast-path enabled + skip optimization LIVE (the
    // exact configuration under which a "zero-read fetch skip" would have been
    // dangerous).
    let _e = CopyModeEnv::set(None, None);

    let store_dir = TempDir::new("corrupt-fetch-store");
    let src = TempDir::new("corrupt-fetch-src");
    let dest = TempDir::new("corrupt-fetch-dest");

    // A >256 KiB file so the clone fast-path is firmly in play on APFS.
    let content: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let files: Vec<(&str, &[u8], &str)> = vec![("payload.bin", content.as_slice(), "644")];
    let (manifest, _id) = build_tree(src.path(), &files);

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    store
        .push(&manifest, src.path())
        .expect("push clean object");

    // Corrupt the committed store object IN PLACE (out-of-band on-disk rot):
    // same byte length, different content, so a size check alone cannot catch
    // it — only a real content re-hash can.
    let checksum = Blake3Hasher::new().hash_hex(&content);
    let obj = object_disk(store_dir.path(), &checksum);
    assert!(
        obj.is_file(),
        "the store object must exist before corruption"
    );
    let mut corrupt = content.clone();
    corrupt[0] ^= 0xff;
    corrupt[content.len() - 1] ^= 0xff;
    assert_eq!(corrupt.len(), content.len(), "corruption preserves length");
    assert_ne!(corrupt, content, "corruption changes content");
    fs::write(&obj, &corrupt).expect("corrupt the store object in place");

    // Fetch/checkout on the clone-capable host. The corruption MUST be detected
    // (Integrity), NOT silently cloned into the destination.
    let res = store.fetch_files(&manifest, dest.path());
    let err = res.expect_err(
        "fetching a corrupted store object on the clone path must be REJECTED, \
         not silently cloned — clone-skip must not open a silent-corrupt-checkout hole",
    );
    assert!(
        is_integrity(&err),
        "a corrupt store object must surface as StoreError::Integrity on the clone \
         fetch path, got {err:?}"
    );

    // And the destination must NOT hold the corrupt bytes (no partial silent
    // materialization of a mis-addressed object).
    let restored = fs::read(dest.path().join("payload.bin")).unwrap_or_default();
    assert_ne!(
        restored, corrupt,
        "FORBIDDEN: the corrupt store bytes were cloned into the checkout"
    );

    // Read-time backstop is also intact (get_object rejects the corrupt blob).
    match store.get_object(&checksum) {
        Err(e) => assert!(
            is_integrity(&e),
            "get_object of the corrupt blob must be Integrity-rejected, got {e:?}"
        ),
        Ok(_) => panic!("FORBIDDEN: get_object returned the corrupt blob as valid"),
    }
}

// ---------------------------------------------------------------------------
// REVIEW CASE B — StatGuarded SKIP genuinely TAKEN. With a guard map populated
// from the source files' real metadata, the stage clone-skip engages: the
// object pool + id must be correct, equal to the SNAPDIR_VERIFY_COPIES=1 run
// (skip ≡ verify for honest input), and (cfg macos) the clone fast-path must
// have fired (clonefile_hits advanced) — proving the skip rode the clone path,
// not a silent fs::copy fallback.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn stage_statguarded_skip_taken_equals_verify_and_correct_pool() {
    // Spec clause (StatGuarded skip taken): a matching guard makes `persist`
    // skip the temp re-hash with NO read; the result must equal the strict
    // (VERIFY_COPIES=1, full re-hash) run object-for-object and id-for-id.
    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    // Strict reference run (forced re-hash, no skip) WITHOUT guards.
    let (verify_inv, verify_id) = {
        let _e = CopyModeEnv::set(None, Some("1"));
        let store_dir = TempDir::new("sg-verify-store");
        let src = TempDir::new("sg-verify-src");
        let (manifest, id) = build_tree(src.path(), &files);
        let store = FileStore::from_root(store_dir.path().to_path_buf());
        store.push(&manifest, src.path()).expect("strict push");
        (object_inventory(store_dir.path()), id)
    };

    // Skip run: guards captured from the live sources => StatGuarded SKIP.
    let (skip_inv, skip_id, clone_delta) = {
        let _e = CopyModeEnv::set(None, None);
        let store_dir = TempDir::new("sg-skip-store");
        let src = TempDir::new("sg-skip-src");
        let (manifest, id) = build_tree(src.path(), &files);
        let guards = guards_from_sources(&manifest, src.path());
        assert!(
            !guards.is_empty(),
            "the guard map must be populated so the StatGuarded skip can engage"
        );
        let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);
        let before = snapdir_stores::clonefile_hits();
        store.push(&manifest, src.path()).expect("skip push");
        let delta = snapdir_stores::clonefile_hits() - before;
        (object_inventory(store_dir.path()), id, delta)
    };

    assert_eq!(
        skip_inv, verify_inv,
        "StatGuarded skip object pool must be byte-identical to the strict verify run"
    );
    assert_eq!(
        skip_id, verify_id,
        "StatGuarded skip snapshot id must equal the strict verify run"
    );

    // The >256 KiB object is present at full length (skip is not vacuous).
    let big_sum = Blake3Hasher::new().hash_hex(&files_owned[0].1);
    let big_rel = object_path(&big_sum);
    assert!(
        skip_inv
            .iter()
            .any(|(rel, bytes)| *rel == big_rel && bytes.len() == files_owned[0].1.len()),
        "the >256 KiB object must be present at full length under the skip path"
    );

    // On macOS/APFS the skip must have RIDDEN the clone fast-path (>=1 regular
    // file cloned), not silently fallen back to fs::copy.
    #[cfg(target_os = "macos")]
    assert!(
        clone_delta >= 1,
        "StatGuarded skip must ride the clone fast-path on APFS (clonefile_hits \
         must advance): delta={clone_delta}"
    );
    #[cfg(not(target_os = "macos"))]
    let _ = clone_delta;
}

// ---------------------------------------------------------------------------
// REVIEW CASE C — StatGuarded MISMATCH falls back to the re-hash. A guard whose
// (size/mtime/ctime/ino) no longer matches the source (because the source was
// mutated after the guard was captured) must NOT skip: it falls through to the
// temp re-hash, which surfaces a true content change as Integrity. A stale
// guard must NEVER cause a silent mis-addressed object.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn stage_statguarded_stale_guard_never_silently_misaddresses() {
    // Spec clause (StatGuarded mismatch -> re-hash): a guard captured for
    // content A, with the source then overwritten by content B (records A's
    // checksum but holds B), must fall back to the re-hash and reject the
    // mismatch — never file B under checksum(A).
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // default skip mode

    let store_dir = TempDir::new("sg-stale-store");
    let src = TempDir::new("sg-stale-src");

    let content_a: Vec<u8> = (0..(64 * 1024u32)).map(|i| (i % 211) as u8).collect();
    // Same length, different bytes (size-only guard cannot catch this).
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0xff).collect();
    let hasher = Blake3Hasher::new();
    let sum_a = hasher.hash_hex(&content_a);

    let rel = "stale.bin";
    let target = src.path().join(rel);
    fs::write(&target, &content_a).unwrap();

    // Capture the guard for A (this is what the walk would have recorded).
    let guard_a = CopyGuard::from_metadata(&fs::metadata(&target).unwrap()).expect("guard for A");

    let mut manifest = Manifest::new();
    manifest.push(ManifestEntry::new(
        PathType::File,
        "644",
        sum_a.clone(),
        content_a.len() as u64,
        format!("./{rel}"),
    ));
    let root_sum = directory_checksum(std::iter::once(sum_a.as_str()), &hasher);
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        content_a.len() as u64,
        "./",
    ));
    manifest.sort();

    // Overwrite with B. The guard is now STALE (the live metadata no longer
    // matches guard_a: at minimum ctime advances on the rewrite).
    fs::write(&target, &content_b).unwrap();

    let mut guards = HashMap::new();
    guards.insert(target.clone(), guard_a);
    let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);

    let res = store.push(&manifest, src.path());

    // The stale guard must NOT cause a silent mis-address. Either the guard
    // mismatch is detected and the re-hash surfaces Integrity (the expected
    // outcome), or — never — an object readable at checksum(A) holding B.
    match res {
        Err(ref e) => assert!(
            is_integrity(e),
            "a stale guard must fall back to the re-hash and reject the mutated \
             source with Integrity, got {e:?}"
        ),
        Ok(()) => match store.get_object(&sum_a) {
            Ok(bytes) => {
                assert_ne!(
                    bytes, content_b,
                    "FORBIDDEN: a stale guard let bytes(B) be filed under checksum(A)"
                );
                assert_eq!(
                    Blake3Hasher::new().hash_hex(&bytes),
                    sum_a,
                    "any object readable at checksum(A) must hash to checksum(A)"
                );
            }
            Err(e) => assert!(
                is_integrity(&e) || matches!(e, StoreError::ObjectNotFound { .. }),
                "reading checksum(A) after a stale-guard stage must be rejected or \
                 absent, got {e:?}"
            ),
        },
    }
}

// ---------------------------------------------------------------------------
// REVIEW CASE D — SNAPDIR_VERIFY_COPIES=1 OVERRIDES a MATCHING guard. Even when
// the guard still matches the source's current metadata, strict mode must
// re-hash (and therefore catch a mutated source whose metadata was forged back
// to match). This proves the strict override is a true write-time re-hash, not
// a guard-driven skip. Contrast with the same setup under default mode, where
// the matching guard would skip (the read-time backstop is the safety net there
// — covered by case B's race-keystone family).
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn verify_copies_overrides_a_matching_guard() {
    // Spec clause (strict overrides guard): build a guard that MATCHES the
    // current source metadata, but whose content no longer matches the recorded
    // checksum (metadata forged back via utimensat); under VERIFY_COPIES=1 the
    // strict re-hash must catch it (Integrity) regardless of the matching guard.
    use std::os::unix::fs::MetadataExt;

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, Some("1")); // STRICT

    let store_dir = TempDir::new("strict-guard-store");
    let src = TempDir::new("strict-guard-src");

    let content_a: Vec<u8> = (0..(72 * 1024u32)).map(|i| (i % 197) as u8).collect();
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0x6b).collect(); // same length
    let hasher = Blake3Hasher::new();
    let sum_a = hasher.hash_hex(&content_a);

    let rel = "forged.bin";
    let target = src.path().join(rel);

    // Write A, capture A's mtime/atime for later forgery.
    fs::write(&target, &content_a).unwrap();
    let md_a = fs::metadata(&target).unwrap();
    let a_times = filetime_pair(
        md_a.atime(),
        md_a.atime_nsec(),
        md_a.mtime(),
        md_a.mtime_nsec(),
    );

    let mut manifest = Manifest::new();
    manifest.push(ManifestEntry::new(
        PathType::File,
        "644",
        sum_a.clone(),
        content_a.len() as u64,
        format!("./{rel}"),
    ));
    let root_sum = directory_checksum(std::iter::once(sum_a.as_str()), &hasher);
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        content_a.len() as u64,
        "./",
    ));
    manifest.sort();

    // Overwrite with B (same length) and force mtime/atime back to A's.
    fs::write(&target, &content_b).unwrap();
    set_file_times(&target, a_times);

    // Build a guard from the CURRENT (post-forge) metadata so it MATCHES what
    // `persist` will re-stat — proving strict mode ignores the matching guard.
    let matching_guard =
        CopyGuard::from_metadata(&fs::metadata(&target).unwrap()).expect("guard for forged");
    let mut guards = HashMap::new();
    guards.insert(target.clone(), matching_guard);

    let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);
    let res = store.push(&manifest, src.path());

    let err = res
        .expect_err("SNAPDIR_VERIFY_COPIES=1 must re-hash and reject even with a MATCHING guard");
    assert!(
        is_integrity(&err),
        "strict mode must surface Integrity despite the matching guard, got {err:?}"
    );
    match store.get_object(&sum_a) {
        Ok(bytes) => assert_ne!(
            bytes, content_b,
            "FORBIDDEN: strict mode with a matching guard left a mis-addressed object"
        ),
        Err(_) => {}
    }
}

// ---------------------------------------------------------------------------
// REVIEW CASE E — Untrusted (EMPTY guard map) is byte-for-byte today's
// behavior. A FileStore with no guards (the default) must produce an object
// pool + id identical to a CLONEFILE=0 (always-fs::copy+rehash) run for the
// SAME input — i.e. the optimization is inert without guards.
// ---------------------------------------------------------------------------

#[test]
fn stage_untrusted_empty_guards_equals_clone_off() {
    // Spec clause (Untrusted == today): empty `copy_guards` reproduces today's
    // always-rehash behavior; its object pool + id must equal the CLONEFILE=0
    // (fs::copy + rehash) reference run.
    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    // Reference: CLONEFILE=0, no guards.
    let (off_inv, off_id) = {
        let _e = CopyModeEnv::set(Some("0"), None);
        let store_dir = TempDir::new("untrusted-off-store");
        let src = TempDir::new("untrusted-off-src");
        let (manifest, id) = build_tree(src.path(), &files);
        let store = FileStore::from_root(store_dir.path().to_path_buf());
        store.push(&manifest, src.path()).expect("off push");
        (object_inventory(store_dir.path()), id)
    };

    // Default mode but an EXPLICIT empty guard map => every source Untrusted.
    let (untrusted_inv, untrusted_id) = {
        let _e = CopyModeEnv::set(None, None);
        let store_dir = TempDir::new("untrusted-def-store");
        let src = TempDir::new("untrusted-def-src");
        let (manifest, id) = build_tree(src.path(), &files);
        let store =
            FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(HashMap::new());
        store.push(&manifest, src.path()).expect("untrusted push");
        (object_inventory(store_dir.path()), id)
    };

    assert_eq!(
        untrusted_inv, off_inv,
        "Untrusted (empty guards) object pool must equal the CLONEFILE=0 reference"
    );
    assert_eq!(
        untrusted_id, off_id,
        "Untrusted (empty guards) snapshot id must equal the CLONEFILE=0 reference"
    );
}
