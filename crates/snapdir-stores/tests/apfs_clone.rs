//! Adversarial integration suite for the macOS APFS `clonefile` copy-on-write
//! fast-path in the `FileStore` file-copy primitive (phase 29, gate
//! `apfs-clone-spec-tests`).
//!
//! BLACK-BOX: authored from the gate SPEC alone, with NO visibility into the
//! `copy_file` / `persist` internals being tested (there is no clone path yet).
//! It will NOT compile/pass until the stores-impl teammate moves this file into
//! `crates/snapdir-stores/tests/apfs_clone.rs` and lands the feature. Do NOT
//! weaken any assertion to make it green — if a behavior here fails against the
//! landed impl, that is a real bug in the impl, not in this test.
//!
//! SPEC under test: on macOS, when the copy source and the store destination are
//! on the same APFS volume, `FileStore`'s internal copy primitive (used by
//! `push` (source → object) AND `fetch_files` (object → working file)) uses
//! `clonefile()` (copy-on-write) instead of a byte-copy; on non-APFS /
//! cross-volume / non-macOS it falls back to `std::fs::copy`. KEYSTONE: the
//! clone path must be OBSERVABLY IDENTICAL to the `fs::copy` path — same object
//! bytes, same sharded object filenames (checksums), same restored content and
//! permissions.
//!
//! ## Contracted new symbols (impl lane MUST match these names)
//!
//! - Env knob **`SNAPDIR_CLONEFILE=0`** forces the plain `fs::copy` path
//!   (disables the fast-path). `1` / unset = fast-path enabled where supported.
//! - A **public, test-visible clone-hit counter**:
//!   **`snapdir_stores::clonefile_hits() -> u64`** — a process-global
//!   `AtomicU64` incremented each time the clone fast-path actually fires. It
//!   MUST be `pub` (callable from this integration test); a `pub(crate)` hook is
//!   NOT visible from `tests/`.
//!
//! ## Env / parallelism note (read before touching the env knob)
//!
//! `SNAPDIR_CLONEFILE` is process-global, and Rust runs the `#[test]`s in one
//! binary across multiple threads. Tests that toggle the knob therefore take a
//! single process-wide mutex (`ENV_LOCK`) and RESTORE the prior value before
//! releasing it, so a parallel test never observes a half-set knob. The
//! clone-hit-counter delta cases additionally serialize under that lock because
//! `clonefile_hits()` is process-global. Cross-volume (case 4) is
//! skip-if-unavailable (detected at runtime, `return`s with an eprintln note)
//! because CI may have only one filesystem.

// Wiring (shape only, no assertion change): silence workspace `-D warnings`
// clippy lints on this adversary-authored suite. `StreamStore` is imported as a
// contracted-symbol presence check; the round-trip return tuple and the
// skip-if-unavailable match are intentional in the adversary's style.
#![allow(
    unused_imports,
    clippy::type_complexity,
    clippy::single_match_else,
    clippy::single_match,
    clippy::manual_let_else
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
use snapdir_core::store::{manifest_path, object_path, Store};

use snapdir_stores::{FileStore, StreamStore};

// ---------------------------------------------------------------------------
// Test scaffolding (no dev-dependencies; mirrors the existing split/shim tests).
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
            "snapdir-clone-test-{}-{tag}-{n}",
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
        // removed, then remove. (chflags on the file is cleared by the case
        // itself; this is belt-and-suspenders for the temp root.)
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Process-global lock guarding the `SNAPDIR_CLONEFILE` env var + the
/// process-global `clonefile_hits()` counter. Any test that reads/sets the knob
/// or compares counter deltas MUST hold this for its whole body.
fn env_lock() -> MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII guard that sets `SNAPDIR_CLONEFILE` to `value` and restores the prior
/// value (or unsets it) on drop. The caller must already hold `env_lock()`.
struct CloneEnv {
    prev: Option<String>,
}

impl CloneEnv {
    /// `Some("0")` disables the fast-path; `None` removes the var (default =
    /// fast-path enabled where supported).
    fn set(value: Option<&str>) -> Self {
        let prev = std::env::var("SNAPDIR_CLONEFILE").ok();
        match value {
            Some(v) => std::env::set_var("SNAPDIR_CLONEFILE", v),
            None => std::env::remove_var("SNAPDIR_CLONEFILE"),
        }
        Self { prev }
    }
}

impl Drop for CloneEnv {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("SNAPDIR_CLONEFILE", v),
            None => std::env::remove_var("SNAPDIR_CLONEFILE"),
        }
    }
}

/// Writes a real source tree under `src` and returns the matching `Manifest`
/// plus its snapshot id. `files` is `(relative path, content, octal-mode-str)`.
/// A `D ./` root entry is synthesized so the manifest is a valid snapshot.
/// Object addressing uses the NON-keyed `Blake3Hasher` (the content-address
/// hasher the file store files objects under), exactly as the shipped
/// shim/file-store/split tests do.
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
/// bytes)`, sorted by path — the structure case 1 compares between the clone
/// and `fs::copy` pools (identical sharded filenames AND identical bytes).
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

/// A representative tree exercising the boundary sizes the SPEC calls out: a
/// file LARGER than 256 KiB (the mmap/clone-relevant size), a tiny file, and a
/// 0-byte file. Contents are deterministic so checksums are stable.
fn mixed_size_files() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    // >256 KiB so it straddles any size-based clone/byte-copy branch.
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

// ===========================================================================
// CASE 1 — Round-trip equivalence, clone ON (default) vs OFF (SNAPDIR_CLONEFILE=0).
// Spec: "the clone path must be OBSERVABLY IDENTICAL to the fs::copy path —
// same object bytes, same sharded object filenames, same restored content."
// ===========================================================================

/// Runs a full stage→object→checkout cycle for `files` against a fresh store,
/// returning (object-pool inventory, restored-content map, restored-mode map,
/// snapshot id). Holds NOTHING about the env — the caller sets the knob.
fn roundtrip_once(
    tag: &str,
    files: &[(&str, &[u8], &str)],
) -> (
    Vec<(String, Vec<u8>)>,
    Vec<(String, Vec<u8>)>,
    Vec<(String, Option<u32>)>,
    String,
) {
    let store_dir = TempDir::new(&format!("rt-store-{tag}"));
    let src = TempDir::new(&format!("rt-src-{tag}"));
    let dest = TempDir::new(&format!("rt-dest-{tag}"));

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

    // The store_dir / src / dest TempDirs drop here; we already snapshotted
    // every byte we need above.
    (inv, restored, modes, id)
}

#[test]
fn clone_on_and_off_produce_identical_objects_filenames_and_restored_content() {
    // Spec clause: clone path OBSERVABLY IDENTICAL to fs::copy — identical
    // object bytes AND identical sharded filenames AND identical restored
    // content; snapshot id round-trips unchanged across both paths.
    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    // Fast-path enabled (default / unset).
    let on = {
        let _e = CloneEnv::set(None);
        roundtrip_once("on", &files)
    };
    // Fast-path force-disabled.
    let off = {
        let _e = CloneEnv::set(Some("0"));
        roundtrip_once("off", &files)
    };

    assert_eq!(
        on.0, off.0,
        "object pool inventory (sharded filenames + bytes) must be byte-identical \
         between the clone fast-path and the fs::copy path"
    );
    assert_eq!(
        on.1, off.1,
        "restored file content must be identical between clone and fs::copy"
    );
    assert_eq!(
        on.3, off.3,
        "snapshot id must round-trip identically regardless of the copy path"
    );

    // Sanity: the >256 KiB file actually produced a distinct large blob, so the
    // equivalence above is not vacuously over only tiny inputs.
    let big_sum = Blake3Hasher::new().hash_hex(&files_owned[0].1);
    // The inventory `rel` is the object's sharded relative path under the store
    // root (`object_path` inserts `/` separators between the shard segments), so
    // match the full sharded path rather than a raw checksum substring (wiring).
    let big_rel = object_path(&big_sum);
    assert!(
        on.0.iter()
            .any(|(rel, bytes)| *rel == big_rel && bytes.len() == files_owned[0].1.len()),
        "the >256 KiB object must be present in the pool with its full length"
    );
}

// ===========================================================================
// CASE 2 — Permissions parity (clone copies all metadata; impl must match
// fs::copy's perms-only semantics).
// Spec clause: "A source file with an unusual mode yields the SAME
// object/restored permissions under clone vs fs::copy."
// ===========================================================================

#[cfg(unix)]
#[test]
fn clone_and_fscopy_yield_identical_restored_permissions() {
    // Spec clause: identical resulting restored-file mode between the clone path
    // and the fs::copy path for unusual source modes (0o600, 0o755).
    // Sorted by path so this vec lines up with `on.2`/`off.2` (which
    // `roundtrip_once` sorts by rel) when zipped below — wiring only; every
    // file's recorded mode is still asserted against its own restored mode.
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
        let _e = CloneEnv::set(None);
        roundtrip_once("perm-on", &files)
    };
    let off = {
        let _e = CloneEnv::set(Some("0"));
        roundtrip_once("perm-off", &files)
    };

    assert_eq!(
        on.2, off.2,
        "restored-file permission bits must be identical between clone and fs::copy"
    );
    // And both must honor the manifest's recorded mode (the restored file's
    // perm bits equal the manifest mode), so neither path silently widens perms.
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
// CASE 3 — BSD-flags safety (the dangerous one): a `uchg` (UF_IMMUTABLE)
// source must NOT yield an immutable object — the impl must clear cloned BSD
// flags so the object stays removable/GC-able.
// Spec clause: "the resulting object file is removable (can be fs::remove_file'd
// / GC'd)."
// ===========================================================================

#[cfg(target_os = "macos")]
#[test]
fn immutable_uchg_source_does_not_produce_an_immutable_object() {
    // Spec clause: clone must clear cloned BSD flags; the object must be GC-able.
    let store_dir = TempDir::new("uchg-store");
    let src = TempDir::new("uchg-src");

    let content = b"immutable-source-bytes\n".to_vec();
    let rel = "locked.bin";
    let target = src.path().join(rel);
    fs::write(&target, &content).unwrap();

    // Set the user-immutable flag on the SOURCE via `chflags uchg`.
    let set = std::process::Command::new("chflags")
        .arg("uchg")
        .arg(&target)
        .status()
        .expect("spawn chflags");
    assert!(set.success(), "chflags uchg must succeed on the source");

    // Build the manifest AFTER setting the flag (the file content is unchanged).
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
    let _e = CloneEnv::set(None); // fast-path on, so a clone really fires

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let push_res = store.push(&manifest, src.path());

    // Clear the source flag now so the src TempDir can be cleaned up regardless
    // of the assertion outcome below.
    let _ = std::process::Command::new("chflags")
        .arg("nouchg")
        .arg(&target)
        .status();

    push_res.expect("push of a uchg source must succeed");

    // The KEYSTONE assertion: the resulting object blob must be REMOVABLE — the
    // immutable flag must NOT have been cloned onto it.
    let obj = object_disk(store_dir.path(), &sum);
    assert!(obj.is_file(), "object must have landed");
    fs::remove_file(&obj)
        .expect("the cloned object must NOT be immutable — it must be removable/GC-able");
    assert!(!obj.exists(), "object must be gone after removal");
}

// ===========================================================================
// CASE 4 — Cross-volume fallback: when source and store are on DIFFERENT
// filesystems, clonefile returns EXDEV/ENOTSUP and the impl must fall back to
// fs::copy — the copy still succeeds with correct bytes. Skip-if-unavailable.
// Spec clause: "the clone path must fall back to fs::copy on EXDEV/ENOTSUP."
// ===========================================================================

/// Finds a writable directory on a DIFFERENT device than `same_dev_path`, or
/// `None` if no distinct volume is exercisable (CI single-FS → skip).
#[cfg(unix)]
fn other_volume_dir(same_dev_path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let base_dev = fs::metadata(same_dev_path).ok()?.dev();
    // Common candidates for a second filesystem (macOS RAM disk / Linux tmpfs).
    for cand in ["/dev/shm", "/tmp", "/private/tmp", "/var/tmp"] {
        let p = Path::new(cand);
        if let Ok(md) = fs::metadata(p) {
            if md.dev() != base_dev {
                // Confirm it is actually writable by creating a scratch subdir.
                let scratch = p.join(format!("snapdir-xdev-{}", std::process::id()));
                if fs::create_dir_all(&scratch).is_ok() {
                    return Some(scratch);
                }
            }
        }
    }
    None
}

#[cfg(unix)]
#[test]
fn cross_volume_source_falls_back_to_fscopy_with_correct_bytes() {
    // Spec clause: cross-volume must fall back to fs::copy (EXDEV/ENOTSUP) and
    // still produce correct bytes. Skip-if-unavailable on single-FS CI.
    let store_dir = TempDir::new("xdev-store");

    let other = match other_volume_dir(store_dir.path()) {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP cross_volume_source_falls_back_to_fscopy_with_correct_bytes: \
                 no second writable volume detected on this host"
            );
            return;
        }
    };

    // Put the SOURCE tree on the other volume; the store stays on `store_dir`'s
    // volume — so source→object crosses a device boundary.
    let src = other.join(format!("src-{}", std::process::id()));
    fs::create_dir_all(&src).unwrap();

    let content: Vec<u8> = (0..(280 * 1024u32)).map(|i| (i % 197) as u8).collect();
    let files: Vec<(&str, &[u8], &str)> = vec![("payload.bin", content.as_slice(), "644")];
    let (manifest, id) = build_tree(&src, &files);

    let _g = env_lock();
    let _e = CloneEnv::set(None); // fast-path on; impl must fall back on EXDEV

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let res = store.push(&manifest, &src);

    // Clean the cross-volume scratch regardless.
    let cleanup = |p: &Path| {
        let _ = fs::remove_dir_all(p);
    };

    match res {
        Ok(()) => {
            let sum = Blake3Hasher::new().hash_hex(&content);
            let blob = fs::read(object_disk(store_dir.path(), &sum));
            cleanup(&other);
            let blob = blob.expect("object blob must exist after cross-volume push");
            assert_eq!(
                blob, content,
                "cross-volume fallback must still file byte-correct object content"
            );
            assert!(
                manifest_disk(store_dir.path(), &id).is_file(),
                "manifest must commit after the fallback copy"
            );
        }
        Err(e) => {
            cleanup(&other);
            panic!("cross-volume push must succeed via fs::copy fallback, got {e}");
        }
    }
}

// ===========================================================================
// CASE 5 — Clone actually fires (anti-silent-fallback) + the OFF knob really
// disables it.
// Spec clause: on a same-volume APFS stage clonefile_hits() INCREASES;
// under SNAPDIR_CLONEFILE=0 the counter does NOT increase.
// ===========================================================================

#[cfg(target_os = "macos")]
#[test]
fn clone_fast_path_actually_fires_on_same_volume_and_off_knob_disables_it() {
    // Spec clause: anti-silent-fallback — the counter must increase on a
    // same-volume push with the fast-path on, and stay flat when forced off.
    let files_owned = mixed_size_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();

    // (a) Fast-path ON: the counter must strictly increase.
    let before_on = snapdir_stores::clonefile_hits();
    {
        let _e = CloneEnv::set(None);
        // src + store on the SAME temp volume (both under std::env::temp_dir()).
        let store_dir = TempDir::new("fires-store");
        let src = TempDir::new("fires-src");
        let dest = TempDir::new("fires-dest");
        let (manifest, _id) = build_tree(src.path(), &files);
        let store = FileStore::from_root(store_dir.path().to_path_buf());
        store.push(&manifest, src.path()).expect("push");
        // checkout too, so BOTH copy directions (push + fetch) exercise clone.
        store
            .fetch_files(&manifest, dest.path())
            .expect("fetch_files");
    }
    let after_on = snapdir_stores::clonefile_hits();
    assert!(
        after_on > before_on,
        "clonefile_hits() must INCREASE on a same-volume APFS stage \
         (impl must not be silently always-falling-back): {before_on} -> {after_on}"
    );

    // (b) Fast-path forced OFF: the counter must NOT move.
    let before_off = snapdir_stores::clonefile_hits();
    {
        let _e = CloneEnv::set(Some("0"));
        let store_dir = TempDir::new("off-store");
        let src = TempDir::new("off-src");
        let dest = TempDir::new("off-dest");
        let (manifest, _id) = build_tree(src.path(), &files);
        let store = FileStore::from_root(store_dir.path().to_path_buf());
        store.push(&manifest, src.path()).expect("push");
        store
            .fetch_files(&manifest, dest.path())
            .expect("fetch_files");
    }
    let after_off = snapdir_stores::clonefile_hits();
    assert_eq!(
        after_off, before_off,
        "with SNAPDIR_CLONEFILE=0 the clone fast-path must NOT fire: {before_off} -> {after_off}"
    );
}

// ===========================================================================
// CASE 6 — Degenerate inputs: a 0-byte file clones correctly, and a read-only
// source still produces a correct, usable object.
// Spec clause: "0-byte file clones correctly; a read-only source still produces
// a correct, writable-enough object."
// ===========================================================================

#[test]
fn zero_byte_file_clones_correctly() {
    // Spec clause: 0-byte file clones/copies correctly (no empty-file choke).
    let files: Vec<(&str, &[u8], &str)> = vec![("empty", b"".as_slice(), "644")];

    let _g = env_lock();
    let _e = CloneEnv::set(None); // fast-path on

    let store_dir = TempDir::new("zero-store");
    let src = TempDir::new("zero-src");
    let dest = TempDir::new("zero-dest");
    let (manifest, id) = build_tree(src.path(), &files);
    let store = FileStore::from_root(store_dir.path().to_path_buf());
    store.push(&manifest, src.path()).expect("push 0-byte");
    store
        .fetch_files(&manifest, dest.path())
        .expect("fetch 0-byte");

    // The empty object's checksum is the BLAKE3 of the empty input.
    let sum = Blake3Hasher::new().hash_hex(b"");
    let obj = object_disk(store_dir.path(), &sum);
    assert!(obj.is_file(), "0-byte object must exist");
    assert_eq!(
        fs::metadata(&obj).unwrap().len(),
        0,
        "0-byte object must be exactly empty"
    );
    let restored = fs::read(dest.path().join("empty")).expect("restored empty file");
    assert!(restored.is_empty(), "restored 0-byte file must be empty");
    // And the snapshot round-trips.
    assert_eq!(
        store.get_manifest(&id).expect("get_manifest").to_string(),
        manifest.to_string()
    );
}

#[test]
fn read_only_source_still_produces_a_correct_object() {
    // Spec clause: a read-only (0o444) source still produces a correct object
    // (the clone/copy must not require write perms on the source).
    let content = b"read-only source content\n".to_vec();
    let files: Vec<(&str, &[u8], &str)> = vec![("ro.txt", content.as_slice(), "444")];

    let _g = env_lock();
    let _e = CloneEnv::set(None); // fast-path on

    let store_dir = TempDir::new("ro-store");
    let src = TempDir::new("ro-src");
    let dest = TempDir::new("ro-dest");
    let (manifest, _id) = build_tree(src.path(), &files);

    let store = FileStore::from_root(store_dir.path().to_path_buf());
    store
        .push(&manifest, src.path())
        .expect("push read-only source");
    store
        .fetch_files(&manifest, dest.path())
        .expect("fetch read-only source");

    let sum = Blake3Hasher::new().hash_hex(&content);
    // The object exists, has the right bytes, AND is removable (GC-able) — i.e.
    // the read-only source mode did not leave the object un-deletable.
    let obj = object_disk(store_dir.path(), &sum);
    assert_eq!(fs::read(&obj).expect("object bytes"), content);
    assert_eq!(count_objects(store_dir.path()), 1);
    fs::remove_file(&obj).expect("object from a read-only source must be removable/GC-able");

    // Restored content is correct.
    let restored = fs::read(dest.path().join("ro.txt")).expect("restored file");
    assert_eq!(restored, content);
}

// ===========================================================================
// CASE 7 — Object/restored object COUNT + dedup parity between clone and
// fs::copy on a duplicate-content tree (clone path must not alter dedup: two
// files with identical content → ONE object under both paths).
// Spec clause: clone path OBSERVABLY IDENTICAL — same object SET (dedup intact).
// ===========================================================================

#[test]
fn duplicate_content_dedups_to_one_object_under_both_paths() {
    // Spec clause: identical object set/dedup between clone and fs::copy.
    let dup = b"identical-content-deduped\n";
    let files: Vec<(&str, &[u8], &str)> = vec![
        ("a/first", dup.as_slice(), "644"),
        ("b/second", dup.as_slice(), "644"),
        ("c/third", dup.as_slice(), "644"),
    ];

    let _g = env_lock();

    let on_count = {
        let _e = CloneEnv::set(None);
        let store_dir = TempDir::new("dedup-on-store");
        let src = TempDir::new("dedup-on-src");
        let (manifest, _id) = build_tree(src.path(), &files);
        FileStore::from_root(store_dir.path().to_path_buf())
            .push(&manifest, src.path())
            .expect("push on");
        count_objects(store_dir.path())
    };
    let off_count = {
        let _e = CloneEnv::set(Some("0"));
        let store_dir = TempDir::new("dedup-off-store");
        let src = TempDir::new("dedup-off-src");
        let (manifest, _id) = build_tree(src.path(), &files);
        FileStore::from_root(store_dir.path().to_path_buf())
            .push(&manifest, src.path())
            .expect("push off");
        count_objects(store_dir.path())
    };

    assert_eq!(
        on_count, 1,
        "three identical-content files must dedup to ONE object"
    );
    assert_eq!(
        on_count, off_count,
        "dedup behavior must be identical between the clone and fs::copy paths"
    );
}
