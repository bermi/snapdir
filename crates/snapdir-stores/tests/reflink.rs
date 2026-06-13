//! Adversarial integration suite for the Linux `FICLONE` (reflink) copy-on-write
//! fast-path in the `FileStore` file-copy primitive (phase 29, gate
//! `reflink-spec-tests`).
//!
//! BLACK-BOX: authored from the gate SPEC alone, with NO visibility into the
//! `copy_file` / `try_reflink` internals being added (the Linux clone branch
//! does NOT exist yet — `try_reflink` is unwritten). It will NOT pass until the
//! stores-impl teammate moves this file into
//! `crates/snapdir-stores/tests/reflink.rs`, lands the `#[cfg(target_os="linux")]`
//! FICLONE branch, and the Btrfs CI leg provides a real reflink filesystem. Do
//! NOT weaken any assertion to make it green — if a behavior here fails against
//! the landed impl on a reflink FS, that is a real bug in the impl.
//!
//! ## SPEC under test (Linux reflink / FICLONE)
//!
//! The impl adds a `#[cfg(target_os = "linux")]` branch to the `FileStore`'s
//! internal copy primitive: when the source and the store/cache share a
//! reflink-capable filesystem (Btrfs / XFS reflink=1 / `OpenZFS` 2.2+ / OCFS2 /
//! bcachefs), `stage`/`push`/`checkout` clone objects copy-on-write via the
//! `FICLONE` ioctl instead of a byte-copy; it falls back to `fs::copy` on
//! ext4/F2FS/tmpfs, across filesystems (`EXDEV`), or on unsupported kernels.
//! It REUSES the existing macOS knobs/counter — NO new symbol:
//!   * `SNAPDIR_CLONEFILE=0` disables the clone fast-path (-> `fs::copy`),
//!     unified across macOS + Linux.
//!   * `snapdir_stores::clonefile_hits() -> u64` — the shared CoW-clone-fire
//!     counter; the Linux FICLONE path bumps it too.
//!   * `SNAPDIR_VERIFY_COPIES=1` — strict write-time re-hash (already exists).
//!
//! ## Env-gating contract (set by the Btrfs CI leg)
//!
//! Standard CI / dev hosts are ext4 (NO reflink), so FICLONE would silently
//! fall back to `fs::copy` and the "clone actually fired" assertions could never
//! pass. The suite is therefore gated on a reflink-capable directory:
//!   * `SNAPDIR_REFLINK_TEST_DIR` — path to a mounted reflink FS (the Btrfs leg
//!     sets it, e.g. to `/mnt/reflink`). When set, BOTH the source tree AND the
//!     `FileStore`/cache are placed UNDER it (FICLONE returns `EXDEV`
//!     cross-filesystem, so co-location is mandatory or the clone never fires).
//!   * `SNAPDIR_REFLINK_TEST_REQUIRE=1` — on the Btrfs leg, a MISSING
//!     `SNAPDIR_REFLINK_TEST_DIR` is a hard `panic!` (enforce — do NOT let the
//!     required leg silently pass without exercising real FICLONE).
//!   * Neither set (the ext4 matrix): `eprintln!` a skip note and `return`, so
//!     the ext4 legs stay green without a reflink FS.
//!
//! The whole file is `#[cfg(target_os = "linux")]`, so it compiles to NOTHING on
//! the macOS dev host (correct — structural verification only checks the file
//! exists and contains `#[test]`).
//!
//! ## Env / parallelism note
//!
//! `SNAPDIR_CLONEFILE` / `SNAPDIR_VERIFY_COPIES` are process-global and Rust runs
//! `#[test]`s multithreaded in one binary, so every test that touches a knob (or
//! compares a `clonefile_hits()` delta) holds a single process-wide `ENV_LOCK`
//! for its whole body and RESTORES the prior values on drop, so a parallel test
//! never observes a half-set knob. This mirrors `apfs_clone.rs`.

#![cfg(target_os = "linux")]
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
    // These pedantic lints only ever compile on the Linux target (the whole file
    // is `#![cfg(target_os = "linux")]`), so they are invisible to the macOS dev
    // host's clippy but fire on CI's Linux `clippy --all-targets -D warnings`:
    //   * map_unwrap_or / manual_assert — the env-contract gate `reflink_root_or_skip`,
    //   * cast_possible_truncation — `len() as usize` / errno `as i32` in test asserts,
    //   * unnested_or_patterns — the skip-on-EPERM/EACCES match arm.
    // Pure shape; no assertion or test logic is affected.
    clippy::map_unwrap_or,
    clippy::manual_assert,
    clippy::cast_possible_truncation,
    clippy::unnested_or_patterns,
    dead_code
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path, Store};

use snapdir_stores::{FileStore, StreamStore};

// ---------------------------------------------------------------------------
// Reflink-FS gating: every test starts by resolving the reflink root, or
// skips/panics per the env contract above.
// ---------------------------------------------------------------------------

/// Resolves the reflink-capable root for this run, honoring the env contract:
///   * `SNAPDIR_REFLINK_TEST_DIR` set -> `Some(path)` (place src + store under it).
///   * unset + `SNAPDIR_REFLINK_TEST_REQUIRE=1` -> `panic!` (Btrfs leg enforce).
///   * unset + not required -> `None` (caller `eprintln!`s a skip note + returns).
///
/// Spec clause: "If UNSET and SNAPDIR_REFLINK_TEST_REQUIRE=1 -> panic; if UNSET
/// and not required -> skip + return; when set, co-locate src AND store under it."
fn reflink_root_or_skip(test_name: &str) -> Option<PathBuf> {
    match std::env::var("SNAPDIR_REFLINK_TEST_DIR") {
        Ok(dir) if !dir.is_empty() => {
            let p = PathBuf::from(dir);
            assert!(
                p.is_dir(),
                "SNAPDIR_REFLINK_TEST_DIR={} must be an existing mounted reflink directory",
                p.display()
            );
            Some(p)
        }
        _ => {
            let required = std::env::var("SNAPDIR_REFLINK_TEST_REQUIRE")
                .map(|v| v == "1")
                .unwrap_or(false);
            if required {
                panic!("reflink FS required but SNAPDIR_REFLINK_TEST_DIR unset");
            }
            eprintln!(
                "SKIP {test_name}: SNAPDIR_REFLINK_TEST_DIR unset and \
                 SNAPDIR_REFLINK_TEST_REQUIRE != 1 (no reflink FS on this host)"
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Test scaffolding (no dev-dependencies; mirrors apfs_clone.rs / clone_skip.rs).
// ---------------------------------------------------------------------------

/// A unique temp dir under `parent`, removed on drop. Used to keep src + store
/// UNDER the reflink root so FICLONE can actually fire (same-FS co-location).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a unique dir under `parent` (the reflink root for reflink cases).
    fn under(parent: &Path, tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            "snapdir-reflink-test-{}-{tag}-{n}",
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
        // Best-effort: clear any immutable flag a case set so the dir can be
        // removed, then remove. (The immutable case clears its own flag too;
        // this is belt-and-suspenders for the temp root.)
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Process-global lock guarding `SNAPDIR_CLONEFILE` + `SNAPDIR_VERIFY_COPIES`
/// and the process-global `clonefile_hits()` counter. Any test that reads/sets a
/// knob or compares a counter delta MUST hold this for its whole body.
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
    /// `clonefile`: `Some("0")` disables the clone fast-path (forces `fs::copy`);
    /// `None` leaves it default-enabled. `verify`: `Some("1")` forces the
    /// write-time re-hash even on the clone path; `None` leaves it default.
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

/// Writes a real source tree under `src` and returns the matching `Manifest`
/// plus its snapshot id. `files` is `(relative path, content, octal-mode-str)`.
/// A `D ./` root entry is synthesized so the manifest is a valid snapshot.
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
        let perms = fs::Permissions::from_mode(u32::from_str_radix(mode, 8).unwrap());
        fs::set_permissions(&target, perms).unwrap();
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
/// bytes)`, sorted by path — for the clone-vs-fs::copy pool comparisons.
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

/// The unix mode bits (permission portion) of `path`.
fn mode_bits(path: &Path) -> Option<u32> {
    Some(fs::metadata(path).ok()?.permissions().mode() & 0o7777)
}

/// A representative tree exercising the boundary sizes the SPEC calls out: a
/// file LARGER than 256 KiB (so FICLONE shares real extents), a tiny file, and a
/// 0-byte file. Contents are deterministic so checksums are stable.
fn mixed_size_files() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    // >256 KiB so it spans many extents — the regime where a CoW clone differs
    // from a byte-copy and a truncation/aliasing bug would surface.
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
    ]
}

/// One full stage->object->checkout cycle, with src/store/dest ALL under
/// `reflink_root`, returning (object-pool inventory, restored-content map,
/// restored-mode map, snapshot id). The env knob is set by the caller.
fn roundtrip_once(
    reflink_root: &Path,
    tag: &str,
    files: &[(&str, &[u8], &str)],
) -> (
    Vec<(String, Vec<u8>)>,
    Vec<(String, Vec<u8>)>,
    Vec<(String, Option<u32>)>,
    String,
) {
    let store_dir = TempDir::under(reflink_root, &format!("rt-store-{tag}"));
    let src = TempDir::under(reflink_root, &format!("rt-src-{tag}"));
    let dest = TempDir::under(reflink_root, &format!("rt-dest-{tag}"));

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

// ===========================================================================
// CASE 1 — FICLONE actually fires (anti-silent-fallback). On the reflink dir,
// a push (source -> object) clones >=1 object and a fetch (object -> working
// file) clones >=1 more, so clonefile_hits() advances by at least 2. This
// PROVES the reflink branch fired rather than silently byte-copying.
// Spec clause (case 1): "after - before >= 2 (push clones >=1 + fetch clones
// >=1) — proves the reflink fired, not a silent fallback."
// ===========================================================================

#[test]
fn ficlone_fires_on_reflink_fs_push_and_fetch_bump_counter() {
    // Spec clause (case 1): on a reflink FS, push + fetch each ride FICLONE, so
    // clonefile_hits() must increase by >= 2 across one stage+checkout cycle.
    let Some(root) =
        reflink_root_or_skip("ficlone_fires_on_reflink_fs_push_and_fetch_bump_counter")
    else {
        return;
    };

    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // fast-path on so FICLONE really fires

    let store_dir = TempDir::under(&root, "fires-store");
    let src = TempDir::under(&root, "fires-src");
    let dest = TempDir::under(&root, "fires-dest");
    let (manifest, _id) = build_tree(src.path(), &files);
    let store = FileStore::from_root(store_dir.path().to_path_buf());

    let before = snapdir_stores::clonefile_hits();
    store
        .push(&manifest, src.path())
        .expect("push on reflink FS");
    store
        .fetch_files(&manifest, dest.path())
        .expect("fetch_files on reflink FS");
    let after = snapdir_stores::clonefile_hits();

    assert!(
        after - before >= 2,
        "FICLONE must fire for BOTH push (source->object, >=1) and fetch \
         (object->working file, >=1) on a reflink FS — proving the reflink \
         fast-path fired and did not silently fall back to fs::copy: \
         {before} -> {after}"
    );
}

// ===========================================================================
// CASE 2 — Identical output: reflink ON (default) vs OFF (SNAPDIR_CLONEFILE=0,
// fs::copy) vs strict (SNAPDIR_VERIFY_COPIES=1) produce byte-identical object
// pools (sharded filenames + bytes), identical restored content + perms, and
// the identical snapshot id.
// Spec clause (case 2): "identical restored content, identical object pool
// (sharded filenames + bytes), identical snapshot id; also vs VERIFY_COPIES=1."
// ===========================================================================

#[test]
fn reflink_on_off_and_verify_produce_identical_objects_content_and_id() {
    // Spec clause (case 2): the reflink path must be OBSERVABLY IDENTICAL to the
    // fs::copy path and to the strict re-hash path — same object pool, restored
    // content, restored perms, and snapshot id.
    let Some(root) =
        reflink_root_or_skip("reflink_on_off_and_verify_produce_identical_objects_content_and_id")
    else {
        return;
    };

    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    // (a) reflink ON (default), (b) reflink OFF (fs::copy), (c) strict re-hash.
    let on = {
        let _e = CopyModeEnv::set(None, None);
        roundtrip_once(&root, "on", &files)
    };
    let off = {
        let _e = CopyModeEnv::set(Some("0"), None);
        roundtrip_once(&root, "off", &files)
    };
    let verify = {
        let _e = CopyModeEnv::set(None, Some("1"));
        roundtrip_once(&root, "verify", &files)
    };

    assert_eq!(
        on.0, off.0,
        "object pool (sharded filenames + bytes) must be byte-identical between \
         the FICLONE fast-path and the fs::copy path"
    );
    assert_eq!(
        on.0, verify.0,
        "object pool must be byte-identical between FICLONE and the strict \
         (VERIFY_COPIES=1) re-hash path"
    );
    assert_eq!(
        on.1, off.1,
        "restored file content must be identical between FICLONE and fs::copy"
    );
    assert_eq!(
        on.1, verify.1,
        "restored file content must be identical between FICLONE and strict mode"
    );
    assert_eq!(
        on.2, off.2,
        "restored permission bits must be identical between FICLONE and fs::copy"
    );
    assert_eq!(
        on.3, off.3,
        "snapshot id must round-trip identically regardless of the copy path"
    );
    assert_eq!(
        on.3, verify.3,
        "snapshot id must be identical between FICLONE and strict mode"
    );

    // Sanity: the >256 KiB object is present full-length so the equivalence is
    // not vacuously over only tiny inputs.
    let big_sum = Blake3Hasher::new().hash_hex(&files_owned[0].1);
    let big_rel = object_path(&big_sum);
    assert!(
        on.0.iter()
            .any(|(rel, bytes)| *rel == big_rel && bytes.len() == files_owned[0].1.len()),
        "the >256 KiB object must be present in the pool at full length"
    );
}

// ===========================================================================
// CASE 3 — Sizes: a >256 KiB file AND a 0-byte file both clone correctly
// (content + id correct). The big file proves real-extent CoW; the 0-byte file
// proves no empty-file choke on the FICLONE path.
// Spec clause (case 3): ">256 KiB file and a 0-byte file both clone correctly."
// ===========================================================================

#[test]
fn reflink_large_and_zero_byte_files_clone_with_correct_content_and_id() {
    // Spec clause (case 3): boundary sizes — a >256 KiB (multi-extent) file and a
    // 0-byte file both produce byte-correct objects with the clone fast-path on.
    let Some(root) =
        reflink_root_or_skip("reflink_large_and_zero_byte_files_clone_with_correct_content_and_id")
    else {
        return;
    };

    // 1 MiB so it spans many extents; deterministic so the checksum is stable.
    let big: Vec<u8> = (0..(1024 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let files_owned: Vec<(&str, Vec<u8>, &str)> = vec![
        ("big/payload.bin", big.clone(), "640"),
        ("empty", Vec::new(), "644"),
    ];
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // fast-path on

    let store_dir = TempDir::under(&root, "sizes-store");
    let src = TempDir::under(&root, "sizes-src");
    let dest = TempDir::under(&root, "sizes-dest");
    let (manifest, id) = build_tree(src.path(), &files);
    let store = FileStore::from_root(store_dir.path().to_path_buf());

    let before = snapdir_stores::clonefile_hits();
    store.push(&manifest, src.path()).expect("push sizes");
    store
        .fetch_files(&manifest, dest.path())
        .expect("fetch sizes");
    let after = snapdir_stores::clonefile_hits();

    // The big (non-empty) object travelled the clone path at least once on push.
    assert!(
        after - before >= 1,
        "the >256 KiB object must travel the FICLONE fast-path (so the byte \
         comparison is not vacuous over a silent fs::copy): {before} -> {after}"
    );

    // Big object: present, full length, byte-correct, and the clone shared a
    // real (non-empty) extent.
    let big_sum = Blake3Hasher::new().hash_hex(&big);
    let big_obj = object_disk(store_dir.path(), &big_sum);
    assert_eq!(
        fs::read(&big_obj).expect("big object bytes"),
        big,
        "the cloned >256 KiB object must be byte-for-byte the source (no truncated CoW)"
    );
    assert_eq!(
        fs::metadata(&big_obj).unwrap().len() as usize,
        big.len(),
        "cloned >256 KiB object must have the full source length"
    );

    // 0-byte object: present and exactly empty.
    let empty_sum = Blake3Hasher::new().hash_hex(b"");
    let empty_obj = object_disk(store_dir.path(), &empty_sum);
    assert!(empty_obj.is_file(), "0-byte object must exist");
    assert_eq!(
        fs::metadata(&empty_obj).unwrap().len(),
        0,
        "0-byte object must be exactly empty"
    );

    // Restored content correct for both, and the snapshot round-trips.
    assert_eq!(
        fs::read(dest.path().join("big/payload.bin")).expect("restored big"),
        big,
        "restored >256 KiB file must equal the source"
    );
    assert!(
        fs::read(dest.path().join("empty"))
            .expect("restored empty")
            .is_empty(),
        "restored 0-byte file must be empty"
    );
    assert_eq!(
        store.get_manifest(&id).expect("get_manifest").to_string(),
        manifest.to_string(),
        "the snapshot manifest must round-trip identically"
    );
}

// ===========================================================================
// CASE 4 — EXDEV cross-FS fallback. When the SOURCE is on a DIFFERENT
// filesystem than the store, FICLONE returns EXDEV and the impl must fall back
// to fs::copy; the push must still succeed with byte-correct object content and
// id. Skip if a second filesystem cannot be arranged.
// Spec clause (case 4): "source on a DIFFERENT FS than the store -> push still
// succeeds with correct bytes/id (graceful fs::copy; clone didn't fire)."
// ===========================================================================

/// A writable dir on a DIFFERENT device than `same_dev_path`, or `None`.
fn other_fs_dir(same_dev_path: &Path) -> Option<PathBuf> {
    let base_dev = fs::metadata(same_dev_path).ok()?.dev();
    // `/tmp`, `/dev/shm`, `/var/tmp` are usually tmpfs/ext4 — NOT the reflink FS.
    for cand in ["/dev/shm", "/tmp", "/var/tmp", "/run"] {
        let p = Path::new(cand);
        if let Ok(md) = fs::metadata(p) {
            if md.dev() != base_dev {
                let scratch = p.join(format!("snapdir-reflink-xdev-{}", std::process::id()));
                if fs::create_dir_all(&scratch).is_ok() {
                    return Some(scratch);
                }
            }
        }
    }
    None
}

#[test]
fn cross_fs_source_falls_back_to_fscopy_with_correct_bytes_and_id() {
    // Spec clause (case 4): cross-filesystem (EXDEV) must gracefully fall back to
    // fs::copy and still produce a byte-correct object + committed manifest.
    let Some(root) =
        reflink_root_or_skip("cross_fs_source_falls_back_to_fscopy_with_correct_bytes_and_id")
    else {
        return;
    };

    // The STORE lives on the reflink FS; the SOURCE lives on a different FS, so
    // source->object crosses a device boundary and FICLONE must return EXDEV.
    let store_dir = TempDir::under(&root, "xdev-store");

    let other = match other_fs_dir(store_dir.path()) {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP cross_fs_source_falls_back_to_fscopy_with_correct_bytes_and_id: \
                 no second writable filesystem distinct from the reflink FS detected"
            );
            return;
        }
    };

    let src = other.join(format!("src-{}", std::process::id()));
    fs::create_dir_all(&src).unwrap();

    let content: Vec<u8> = (0..(280 * 1024u32)).map(|i| (i % 197) as u8).collect();
    let files: Vec<(&str, &[u8], &str)> = vec![("payload.bin", content.as_slice(), "644")];
    let (manifest, id) = build_tree(&src, &files);

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // fast-path on; impl must fall back on EXDEV

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let res = store.push(&manifest, &src);

    let cleanup = |p: &Path| {
        let _ = fs::remove_dir_all(p);
    };

    match res {
        Ok(()) => {
            let sum = Blake3Hasher::new().hash_hex(&content);
            let blob = fs::read(object_disk(store_dir.path(), &sum));
            let manifest_ok = manifest_disk(store_dir.path(), &id).is_file();
            cleanup(&other);
            let blob = blob.expect("object blob must exist after cross-FS push");
            assert_eq!(
                blob, content,
                "cross-FS EXDEV fallback must still file byte-correct object content"
            );
            assert!(
                manifest_ok,
                "the manifest must commit (manifest-last) after the EXDEV fs::copy fallback"
            );
        }
        Err(e) => {
            cleanup(&other);
            panic!("cross-FS push must succeed via the fs::copy (EXDEV) fallback, got {e}");
        }
    }
}

// ===========================================================================
// CASE 5 — Perms parity. A source file with an unusual mode (0o600, 0o755)
// yields the SAME restored/object perms under reflink vs fs::copy.
// Spec clause (case 5): "unusual mode yields the same restored/object perms with
// reflink vs fs::copy."
// ===========================================================================

#[test]
fn reflink_and_fscopy_yield_identical_restored_permissions() {
    // Spec clause (case 5): identical restored-file mode between the FICLONE path
    // and the fs::copy path for unusual source modes (0o600, 0o755), and each
    // restored file honors its manifest-recorded mode (no silent perm widening).
    let Some(root) =
        reflink_root_or_skip("reflink_and_fscopy_yield_identical_restored_permissions")
    else {
        return;
    };

    let mut files_owned: Vec<(&str, Vec<u8>, &str)> = vec![
        ("secret", b"private\n".to_vec(), "600"),
        ("script.sh", b"#!/bin/sh\necho hi\n".to_vec(), "755"),
    ];
    files_owned.sort_by(|a, b| a.0.cmp(b.0));
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    let on = {
        let _e = CopyModeEnv::set(None, None);
        roundtrip_once(&root, "perm-on", &files)
    };
    let off = {
        let _e = CopyModeEnv::set(Some("0"), None);
        roundtrip_once(&root, "perm-off", &files)
    };

    assert_eq!(
        on.2, off.2,
        "restored-file permission bits must be identical between FICLONE and fs::copy"
    );
    for ((rel, mode), (_, _, want)) in on.2.iter().zip(files_owned.iter()) {
        let want_bits = u32::from_str_radix(want, 8).unwrap();
        assert_eq!(
            *mode,
            Some(want_bits),
            "restored {rel} must carry the manifest-recorded mode {want}"
        );
    }
}

// ===========================================================================
// CASE 6 — Immutable source (FS_IMMUTABLE_FL via FS_IOC_SETFLAGS) does NOT
// yield an immutable object. FICLONE is a DATA-ONLY clone, so the inode
// immutable flag must NOT propagate to the object (unlike macOS uchg, Linux has
// no un-GC-able-object risk — but PIN it). Needs privilege: skip-if-EPERM.
// Spec clause (case 6): "set FS_IMMUTABLE_FL on a source via libc; if EPERM ->
// skip; else assert the resulting OBJECT is removable (fs::remove_file ok)."
// ===========================================================================

// FS_IOC_SETFLAGS / FS_IOC_GETFLAGS ioctl request numbers (long-arg, asm-generic
// values used by Btrfs/ext*/XFS on Linux). FS_IMMUTABLE_FL is the immutable bit.
const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;
const FS_IMMUTABLE_FL: libc::c_long = 0x0000_0010;

/// Sets/clears FS_IMMUTABLE_FL on `path`. Returns `Ok(())`, or `Err(errno)` —
/// `EPERM` (1) signals "no privilege" so the caller can skip.
fn set_immutable(path: &Path, immutable: bool) -> Result<(), i32> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    // O_RDONLY is sufficient for FS_IOC_*FLAGS.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
    }
    let result = (|| {
        let mut flags: libc::c_long = 0;
        let rc = unsafe { libc::ioctl(fd, FS_IOC_GETFLAGS as _, &mut flags) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
        }
        if immutable {
            flags |= FS_IMMUTABLE_FL;
        } else {
            flags &= !FS_IMMUTABLE_FL;
        }
        let rc = unsafe { libc::ioctl(fd, FS_IOC_SETFLAGS as _, &flags) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
        }
        Ok(())
    })();
    unsafe { libc::close(fd) };
    result
}

#[test]
fn immutable_source_does_not_produce_an_immutable_object() {
    // Spec clause (case 6): FICLONE clones data only; the FS_IMMUTABLE_FL on the
    // SOURCE inode must NOT propagate to the cloned object — the object must be
    // removable/GC-able. Requires privilege to set the flag; skip on EPERM.
    let Some(root) = reflink_root_or_skip("immutable_source_does_not_produce_an_immutable_object")
    else {
        return;
    };

    let store_dir = TempDir::under(&root, "immut-store");
    let src = TempDir::under(&root, "immut-src");

    let content = b"immutable-source-bytes-cloned-data-only\n".to_vec();
    let rel = "locked.bin";
    let target = src.path().join(rel);
    fs::write(&target, &content).unwrap();

    // Try to set FS_IMMUTABLE_FL on the source. EPERM (or EACCES) => skip.
    match set_immutable(&target, true) {
        Ok(()) => {}
        Err(libc::EPERM) | Err(libc::EACCES) => {
            eprintln!(
                "SKIP immutable_source_does_not_produce_an_immutable_object: \
                 setting FS_IMMUTABLE_FL needs privilege (EPERM/EACCES)"
            );
            return;
        }
        Err(errno) if errno == libc::ENOTTY || errno == libc::EOPNOTSUPP => {
            eprintln!(
                "SKIP immutable_source_does_not_produce_an_immutable_object: \
                 filesystem does not support FS_IOC_SETFLAGS (errno {errno})"
            );
            return;
        }
        Err(errno) => panic!("unexpected errno setting FS_IMMUTABLE_FL: {errno}"),
    }

    // Build the manifest AFTER setting the flag (content is unchanged).
    let hasher = Blake3Hasher::new();
    let sum = hasher.hash_hex(&content);
    let mut manifest = Manifest::new();
    manifest.push(ManifestEntry::new(
        PathType::File,
        "644",
        sum.clone(),
        content.len() as u64,
        format!("./{rel}"),
    ));
    let root_sum = directory_checksum(std::iter::once(sum.as_str()), &hasher);
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        content.len() as u64,
        "./",
    ));
    manifest.sort();

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // fast-path on so a real FICLONE fires

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let push_res = store.push(&manifest, src.path());

    // Clear the SOURCE flag now so the src TempDir can be cleaned up regardless
    // of the assertion outcome below.
    let _ = set_immutable(&target, false);

    push_res.expect("push of an immutable source must succeed");

    // KEYSTONE: the resulting object must be REMOVABLE — FICLONE (data-only)
    // must NOT have propagated FS_IMMUTABLE_FL onto the object inode.
    let obj = object_disk(store_dir.path(), &sum);
    assert!(obj.is_file(), "object must have landed");
    fs::remove_file(&obj).expect(
        "the cloned object must NOT be immutable — FICLONE clones data only, so \
         FS_IMMUTABLE_FL must not propagate (Linux has no un-GC-able-object risk)",
    );
    assert!(!obj.exists(), "object must be gone after removal");
}

// ===========================================================================
// CASE 7 (impl-revealed) — Knob OFF on the reflink FS: with `SNAPDIR_CLONEFILE=0`,
// `copy_file`'s `clonefile_enabled()` guard short-circuits BEFORE `try_reflink`
// is ever called, so the FICLONE ioctl never runs and the counter must NOT
// advance — EVEN when src + store are co-located on a genuinely reflink-capable
// filesystem (the one host where the clone WOULD otherwise fire). This pins that
// the disable knob is honored on the reflink path (read per-copy, not cached),
// while a byte-correct object is still produced via `fs::copy`. Complements
// case 2 (which compares pools) with a direct counter-stays-flat assertion on
// the reflink FS specifically.
// ===========================================================================

#[test]
fn clonefile_disabled_does_not_advance_counter_on_reflink_fs() {
    // Impl: `copy_file` only enters the Linux branch when `clonefile_enabled()`
    // is true; with SNAPDIR_CLONEFILE=0 it falls straight through to fs::copy, so
    // CLONEFILE_HITS must be untouched even on a reflink-capable FS.
    let Some(root) =
        reflink_root_or_skip("clonefile_disabled_does_not_advance_counter_on_reflink_fs")
    else {
        return;
    };

    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();
    let _e = CopyModeEnv::set(Some("0"), None); // clone fast-path DISABLED

    let store_dir = TempDir::under(&root, "off-counter-store");
    let src = TempDir::under(&root, "off-counter-src");
    let dest = TempDir::under(&root, "off-counter-dest");
    let (manifest, id) = build_tree(src.path(), &files);
    let store = FileStore::from_root(store_dir.path().to_path_buf());

    let before = snapdir_stores::clonefile_hits();
    store
        .push(&manifest, src.path())
        .expect("push with clone disabled on reflink FS");
    store
        .fetch_files(&manifest, dest.path())
        .expect("fetch_files with clone disabled on reflink FS");
    let after = snapdir_stores::clonefile_hits();

    assert_eq!(
        after, before,
        "SNAPDIR_CLONEFILE=0 must short-circuit the FICLONE fast-path even on a \
         reflink-capable FS, so clonefile_hits() must NOT advance: {before} -> {after}"
    );

    // The fs::copy fallback must still produce a byte-correct object pool + a
    // round-trippable snapshot (disabling the clone must not corrupt anything).
    let big_sum = Blake3Hasher::new().hash_hex(&files_owned[0].1);
    let big_obj = object_disk(store_dir.path(), &big_sum);
    assert_eq!(
        fs::read(&big_obj).expect("big object via fs::copy"),
        files_owned[0].1,
        "the >256 KiB object must be byte-correct on the fs::copy (clone-off) path"
    );
    assert_eq!(
        store.get_manifest(&id).expect("get_manifest").to_string(),
        manifest.to_string(),
        "the snapshot manifest must round-trip on the clone-off path"
    );
}

// ===========================================================================
// CASE 8 (impl-revealed) — Fallback subdir under a NON-reflink mount still
// produces a correct object with the clone fast-path ENABLED. The impl's
// `try_reflink` returns `Ok(false)` on EXDEV/EOPNOTSUPP/ENOTTY (FS without
// reflink) and `copy_file` then falls through to `fs::copy`; this asserts that
// the fallback object is byte-correct, the manifest commits, AND the counter
// does NOT advance for that store (no false clone-fire credited to a byte copy).
// This is the EXDEV/non-reflink path of case 4, but additionally pins the
// counter-does-not-move invariant on the fallback. Skip if no second FS exists.
// ===========================================================================

#[test]
fn non_reflink_fallback_produces_correct_object_without_counter_bump() {
    // Impl: a store whose objects live on a NON-reflink FS forces try_reflink to
    // return Ok(false) (EXDEV/EOPNOTSUPP/ENOTTY), so fs::copy runs and the
    // counter is never bumped for that copy — yet the object is byte-correct.
    let Some(root) =
        reflink_root_or_skip("non_reflink_fallback_produces_correct_object_without_counter_bump")
    else {
        return;
    };

    // Put the STORE on a non-reflink FS (e.g. tmpfs /dev/shm or ext4 /tmp) while
    // the SOURCE is on the reflink FS, so source->object crosses the device
    // boundary (EXDEV) — FICLONE cannot fire and the impl must fall back.
    let src = TempDir::under(&root, "fallback-src");

    let other = match other_fs_dir(src.path()) {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP non_reflink_fallback_produces_correct_object_without_counter_bump: \
                 no second writable filesystem distinct from the reflink FS detected"
            );
            return;
        }
    };
    let store_root = other.join(format!("store-{}", std::process::id()));
    fs::create_dir_all(&store_root).unwrap();

    let content: Vec<u8> = (0..(290 * 1024u32)).map(|i| (i % 193) as u8).collect();
    let files: Vec<(&str, &[u8], &str)> = vec![("payload.bin", content.as_slice(), "640")];
    let (manifest, id) = build_tree(src.path(), &files);

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // clone ENABLED; impl must still fall back

    let store = FileStore::from_root(store_root.clone());

    let before = snapdir_stores::clonefile_hits();
    let push_res = store.push(&manifest, src.path());
    let after = snapdir_stores::clonefile_hits();

    let sum = Blake3Hasher::new().hash_hex(&content);
    let blob = fs::read(object_disk(&store_root, &sum));
    let manifest_ok = manifest_disk(&store_root, &id).is_file();
    let _ = fs::remove_dir_all(&other);

    push_res.expect("push must succeed via the fs::copy fallback on a non-reflink store FS");
    assert_eq!(
        after, before,
        "the EXDEV/non-reflink fallback uses fs::copy, so no clone may be credited \
         to it — clonefile_hits() must NOT advance: {before} -> {after}"
    );
    let blob = blob.expect("object blob must exist after the fallback push");
    assert_eq!(
        blob, content,
        "the fs::copy fallback object must be byte-for-byte the source content"
    );
    assert!(
        manifest_ok,
        "the manifest must commit (manifest-last) after the non-reflink fs::copy fallback"
    );
}
