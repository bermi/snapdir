//! Adversarial integration suite for the **atomic staged swap** (phase 32, gate
//! `mirror-atomic-swap-spec-tests`).
//!
//! BLACK-BOX: authored from the gate SPEC + the operator-approved plan alone,
//! with NO visibility into the atomic-swap implementation (it does NOT exist yet
//! — `fetch_files` / `fetch_files_with_mode` materialize in place; there is no
//! staged-swap path). It will NOT compile/pass until the stores-impl teammate
//! moves this file into `crates/snapdir-stores/tests/mirror_atomic_swap.rs`,
//! lands the staged-swap API + the non-zero-copy hard-error guard, and wires the
//! staged cases. Do NOT weaken any assertion to make it green — if a behavior
//! here fails against the landed impl, that is a real bug in the impl, not in
//! this test.
//!
//! ## SPEC under test (Phase 32 atomic staged swap)
//!
//! Instead of materialize-in-place-then-prune, the store can build the COMPLETE
//! destination tree in a sibling **staging** directory (reflink each file from
//! `.objects/` on CoW, or symlink to objects in `--linked` mode), then
//! atomically swap it into place: `rename(dest -> dest.old)` +
//! `rename(staging -> dest)` + remove the old. The swapped-in dest is an EXACT
//! mirror (built fresh from the manifest, no extraneous files), and a held-open
//! fd on an old dest file keeps reading the OLD inode (POSIX) so long-running
//! processes are never disrupted.
//!
//! ## HARD INVARIANTS this suite pins (must be PREVENTED, not discouraged)
//!
//!   (1) ZERO-COPY ONLY:
//!       (1a) on a CoW/symlink path the staged swap copies ZERO bytes — the
//!            staged/swapped-in files are reflink CLONES (CopyMethod::Cloned, so
//!            clonefile_hits() advances) or SYMLINKS to objects, never fresh
//!            byte-copies.
//!       (1b) requesting an atomic swap on a plain-copy / non-CoW path (forced
//!            via SNAPDIR_CLONEFILE=0, which `cow_reflink_supported` reports
//!            false for) is a TYPED, non-panic HARD ERROR — it must NOT silently
//!            byte-copy, and must NOT partially apply (dest unchanged).
//!   (2) HELD-OPEN FD SURVIVES THE SWAP: a file opened in the dest BEFORE the
//!       swap keeps reading the ORIGINAL bytes across the swap (the old tree is
//!       renamed aside; the inode survives until the fd closes). The durability
//!       keystone.
//!   (3) SWAP-OR-NOTHING (atomicity): a failure injected MID-stage (BEFORE the
//!       final rename) — here, a manifest referencing an object MISSING from the
//!       pool, so staging fails partway — leaves the ORIGINAL dest FULLY INTACT
//!       (no partial mirror, no half-swapped tree, no torn state) and returns a
//!       typed error.
//!   (4) CoW capability probe consulted: when `cow_reflink_supported(dest)`
//!       reports false (SNAPDIR_CLONEFILE=0), an atomic request errors rather
//!       than copying.
//!
//! ## Assumed (contracted) public API — impl lane MUST honor or re-point
//!
//! The atomic-swap API does NOT exist yet. This suite is authored against a
//! PLAUSIBLE shape; the impl may RE-POINT the type/method names only (NOT weaken
//! the contract). See the handoff "Assumed stores API" + "Real-bug report".
//!
//!   * `FileStore::fetch_files_atomic(&self, manifest: &Manifest, dest: &Path,
//!        mode: MaterializeMode) -> Result<(), StoreError>` — builds the full
//!     dest tree in a sibling staging dir (reflink on CoW / symlink under
//!     `Linked`) and atomically swaps it into place. On a non-zero-copy target
//!     (plain copy / non-CoW where `cow_reflink_supported` is false and mode is
//!     NOT `Linked`) it returns a TYPED, non-panic `StoreError` (assumed a
//!     plausible variant; the suite tolerates ANY non-`Io`/non-panic typed error
//!     whose Display names the unsupported/atomic/copy condition via
//!     `is_atomic_unsupported_like`, exactly as the materialize-modes suite did —
//!     a bare `Io` or a panic is NOT acceptable).
//!   * `MaterializeMode::{Auto, Linked}` + `snapdir_stores::clonefile_hits()` +
//!     `snapdir_stores::cow_reflink_supported(dest)` already exist (materialize
//!     modes). The atomic path MUST consult the CoW probe.
//!
//! If the impl decides the swap belongs behind an existing entry point + a flag
//! instead of a new method, it must encode the SAME observable behavior and
//! re-point these tests; that API-shape question is flagged in the handoff.
//!
//! ## Env / parallelism note
//!
//! `SNAPDIR_CLONEFILE` is process-global and Rust runs `#[test]`s multithreaded
//! in one binary, so every test that toggles the knob or compares a
//! `clonefile_hits()` delta holds a single process-wide `ENV_LOCK` for its whole
//! body and RESTORES the prior value on drop. Mirrors `reflink.rs` /
//! `mirror_materialize_modes.rs`.
//!
//! ## CoW gating
//!
//! CI / dev hosts vary: macOS dev is APFS (clone fires), Linux CI runs both an
//! ext4 leg (NO reflink) and a Btrfs loopback leg (real FICLONE). So the
//! "swap actually reflinked / fired" assertions are GATED on a clone-capable
//! host (macOS always; Linux only when `SNAPDIR_REFLINK_TEST_DIR` is provided).
//! The HARD-ERROR half (atomic on a forced-copy path, SNAPDIR_CLONEFILE=0), the
//! held-fd survival, and the swap-or-nothing atomicity invariants are asserted
//! on EVERY host (no reflink FS required) — symlink (`Linked`) staging is
//! zero-copy on ANY filesystem, so the zero-copy + held-fd cases also run
//! unconditionally in `Linked` mode.

// Wiring (shape only, no assertion change): silence workspace `-D warnings`
// clippy lints on this adversary-authored suite. The new method is a
// contracted-symbol presence check; the skip matches + round-trip helpers are
// intentional in the adversary's style.
#![allow(
    unused_imports,
    clippy::type_complexity,
    clippy::single_match_else,
    clippy::single_match,
    clippy::manual_let_else,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::map_unwrap_or,
    clippy::manual_assert,
    clippy::cast_possible_truncation,
    clippy::unnested_or_patterns,
    dead_code
)]

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};

// CONTRACTED symbols. `MaterializeMode` / `clonefile_hits` / `cow_reflink_supported`
// exist (materialize-modes); `FileStore::fetch_files_atomic` is the ASSUMED
// staged-swap entry point this suite calls (impl may re-point the name only).
use snapdir_stores::{
    clonefile_hits, cow_reflink_supported, FileStore, MaterializeMode, StreamStore,
};

// ---------------------------------------------------------------------------
// Test scaffolding (no dev-dependencies; mirrors mirror_materialize_modes.rs /
// reflink.rs).
// ---------------------------------------------------------------------------

/// A unique temp dir removed on drop. `new` places it under the system temp dir;
/// `under` places it under an explicit parent (used to co-locate src + store +
/// dest on a reflink root so a clone can actually fire — same-FS co-location).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        Self::under(&std::env::temp_dir(), tag)
    }

    fn under(parent: &Path, tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            "snapdir-atomic-swap-test-{}-{tag}-{n}",
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
        // A linked/atomic checkout makes objects 0444 and dest entries symlinks;
        // restore writability defensively so a hardened tree (incl. any leftover
        // staging/`dest.old` siblings the impl may create) tears down cleanly.
        restore_writable(&self.path);
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Recursively restore u+w on every dir/file under `root` so a hardened
/// (`0444`) tree can be torn down by the TempDir drop without leaking temp dirs.
#[cfg(unix)]
fn restore_writable(root: &Path) {
    if let Ok(md) = fs::symlink_metadata(root) {
        if md.file_type().is_symlink() {
            return; // never chase a symlink during cleanup
        }
        let mut perms = md.permissions();
        let mode = perms.mode();
        perms.set_mode(mode | 0o700);
        let _ = fs::set_permissions(root, perms);
        if md.is_dir() {
            if let Ok(rd) = fs::read_dir(root) {
                for e in rd.flatten() {
                    restore_writable(&e.path());
                }
            }
        }
    }
}
#[cfg(not(unix))]
fn restore_writable(_root: &Path) {}

/// Process-global lock guarding `SNAPDIR_CLONEFILE` + the process-global
/// `clonefile_hits()` counter. Any test that reads/sets the knob or compares a
/// counter delta MUST hold this for its whole body.
fn env_lock() -> MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII guard that sets/clears `SNAPDIR_CLONEFILE` and restores the prior value
/// on drop. The caller must already hold `env_lock()`.
struct CloneEnv {
    prev: Option<String>,
}

impl CloneEnv {
    /// `Some("0")` disables the clone fast-path (forces `fs::copy` / non-CoW);
    /// `None` leaves it default-enabled.
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
/// hasher the file store files objects under), exactly as the shipped tests do.
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

/// On-disk sharded path of an object blob under `root`.
fn object_disk(root: &Path, checksum: &str) -> PathBuf {
    root.join(object_path(checksum))
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

/// `true` iff `e` is a typed, non-`Io` refusal of the kind the impl MUST use for
/// the "atomic on a non-zero-copy / non-CoW target" hard error. Tolerant of the
/// exact variant name so the impl may re-point to `Unsupported`/`Backend`/a new
/// variant — but a bare `Io` or a panic is NOT acceptable. The Display text must
/// name the unsupported/atomic/copy/CoW condition.
fn is_atomic_unsupported_like(e: &StoreError) -> bool {
    if matches!(e, StoreError::Io(_)) {
        return false;
    }
    if matches!(e, StoreError::Backend { .. }) {
        return true;
    }
    let s = e.to_string().to_lowercase();
    s.contains("unsupported")
        || s.contains("not supported")
        || s.contains("atomic")
        || s.contains("copy-on-write")
        || s.contains("copy on write")
        || s.contains("cow")
        || s.contains("reflink")
        || s.contains("clone")
        || s.contains("would copy")
        || s.contains("byte copy")
        || s.contains("byte-copy")
        || s.contains("zero-copy")
        || s.contains("zero copy")
}

/// `true` iff `e` is a typed, non-`Io`/non-panic error of the kind the impl MUST
/// use for a MID-STAGE staging failure (a manifest object missing from the
/// pool). `ObjectNotFound` is the natural shape; `Integrity` and `Backend` are
/// also acceptable; a bare `Io` is tolerated here ONLY because a missing-object
/// open can surface as `NotFound` Io — but the KEY assertion in that test is
/// that the dest is UNCHANGED, not the error variant.
fn is_typed_staging_failure(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::ObjectNotFound { .. }
            | StoreError::Integrity { .. }
            | StoreError::Backend { .. }
            | StoreError::Io(_)
    )
}

/// A representative tree exercising boundary sizes + path shapes: a file LARGER
/// than 256 KiB (so a reflink shares real extents and a byte-copy would be
/// detectable), a tiny file, a 0-byte file, and a nested deep path.
fn mixed_files() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let big: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    vec![
        ("big.bin", big, "644"),
        ("tiny.txt", b"hello\n".to_vec(), "644"),
        ("empty", Vec::new(), "644"),
        (
            "nested/deep/leaf.bin",
            vec![0u8, 1, 2, 3, 255, 254, 0],
            "600",
        ),
    ]
}

/// Pushes `files` into a fresh local store rooted under `parent` and returns
/// `(store, store_dir, manifest, id, src_dir)`. `src_dir`/`store_dir` are kept
/// alive by the caller (they own the temp roots).
fn staged_store(
    parent: &Path,
    tag: &str,
    files: &[(&str, &[u8], &str)],
) -> (FileStore, TempDir, Manifest, String, TempDir) {
    let store_dir = TempDir::under(parent, &format!("{tag}-store"));
    let src = TempDir::under(parent, &format!("{tag}-src"));
    let (manifest, id) = build_tree(src.path(), files);
    let store = FileStore::from_root(store_dir.path().to_path_buf());
    store
        .push(&manifest, src.path())
        .expect("push to local store");
    (store, store_dir, manifest, id, src)
}

/// Resolves a reflink-capable root for the Linux Btrfs CI leg, honoring the same
/// env contract `reflink.rs` uses: `SNAPDIR_REFLINK_TEST_DIR` set => `Some`,
/// unset => `None` (caller falls back to a plain temp root + copy path). macOS
/// needs no such root (APFS clones under the system temp dir).
fn reflink_root() -> Option<PathBuf> {
    match std::env::var("SNAPDIR_REFLINK_TEST_DIR") {
        Ok(dir) if !dir.is_empty() => {
            let p = PathBuf::from(dir);
            if p.is_dir() {
                Some(p)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether THIS run can actually exercise the reflink fast-path: macOS (APFS) is
/// always clone-capable; Linux only when a reflink root is provided.
fn reflink_capable() -> bool {
    cfg!(target_os = "macos") || reflink_root().is_some()
}

/// The parent dir under which to co-locate src+store+dest so a clone can fire.
fn coloc_parent() -> PathBuf {
    reflink_root().unwrap_or_else(std::env::temp_dir)
}

/// Recursively collect `(relative-path, kind, bytes-if-file)` for every entry
/// under `root` (kind: 'd' dir, 'f' regular file, 'l' symlink). Used to prove a
/// dest is UNCHANGED after a swap-or-nothing failure (exact-tree snapshot).
#[cfg(unix)]
fn tree_snapshot(root: &Path) -> Vec<(String, char, Option<Vec<u8>>)> {
    fn walk(base: &Path, dir: &Path, acc: &mut Vec<(String, char, Option<Vec<u8>>)>) {
        let rd = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let md = match fs::symlink_metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let ft = md.file_type();
            if ft.is_symlink() {
                // record the LINK TARGET text, not the (followed) content
                let tgt = fs::read_link(&p)
                    .ok()
                    .map(|t| t.into_os_string().into_vec_lossy());
                acc.push((rel, 'l', tgt));
            } else if ft.is_dir() {
                acc.push((rel.clone(), 'd', None));
                walk(base, &p, acc);
            } else if ft.is_file() {
                acc.push((rel, 'f', fs::read(&p).ok()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Tiny helper so `tree_snapshot` can stringify a symlink target portably.
#[cfg(unix)]
trait IntoVecLossy {
    fn into_vec_lossy(self) -> Vec<u8>;
}
#[cfg(unix)]
impl IntoVecLossy for std::ffi::OsString {
    fn into_vec_lossy(self) -> Vec<u8> {
        use std::os::unix::ffi::OsStringExt;
        self.into_vec()
    }
}

// ===========================================================================
// CASE 1a — ZERO-COPY on a CoW host: an atomic swap in Auto mode on a
// clone-capable filesystem clones (reflink) the staged files — clonefile_hits()
// advances — so ZERO bytes are duplicated. GATED on a clone-capable host (else
// the copy path can't be a clone and clause 1b covers the non-CoW refusal).
// Spec clause (1a): "on a CoW path the swap copies ZERO bytes (assert via the
// clonefile_hits() counter)".
// ===========================================================================

#[cfg(unix)]
#[test]
fn atomic_swap_on_cow_is_zero_copy_reflink_counter_advances() {
    // Spec clause (1a): an atomic staged swap on a CoW filesystem must build the
    // staging tree by REFLINK (CopyMethod::Cloned) — clonefile_hits() must
    // advance, proving no fresh byte-copy of the >256 KiB object occurred.
    if !reflink_capable() {
        eprintln!(
            "SKIP atomic_swap_on_cow_is_zero_copy_reflink_counter_advances: \
             no clone-capable FS on this host (set SNAPDIR_REFLINK_TEST_DIR on Linux)"
        );
        return;
    }

    let files_owned = mixed_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let parent = coloc_parent();

    let _g = env_lock();
    let _e = CloneEnv::set(None); // clone fast-path enabled (reflink where supported)

    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "zc-cow", &files);
    let dest = TempDir::under(&parent, "zc-cow-dest");

    let before = clonefile_hits();
    store
        .fetch_files_atomic(&manifest, dest.path(), MaterializeMode::Auto)
        .expect("atomic swap on a CoW host must succeed");
    let after = clonefile_hits();

    // The swap must have reflinked at least the >256 KiB object — clones fired,
    // not a silent byte-copy.
    assert!(
        after > before,
        "atomic swap on a CoW host must reflink the staged files (CopyMethod::Cloned) — \
         clonefile_hits() must advance, proving ZERO bytes were copied: {before} -> {after}"
    );

    // The swapped-in dest is an EXACT mirror: every manifest file is present with
    // correct content + mode, and the dest entries are independent regular files
    // (Auto mode is editable, never the thin link view).
    for (rel, content, mode) in &files_owned {
        let p = dest.path().join(rel);
        let md = fs::symlink_metadata(&p).unwrap_or_else(|e| panic!("dest {rel} must exist: {e}"));
        assert!(
            md.file_type().is_file(),
            "atomic Auto dest entry {rel} must be a regular file (editable), not a symlink"
        );
        assert_eq!(
            &fs::read(&p).unwrap(),
            content,
            "swapped-in dest content for {rel} must equal the source"
        );
        assert_eq!(
            mode_bits(&p),
            Some(u32::from_str_radix(mode, 8).unwrap()),
            "swapped-in dest {rel} must carry the manifest-recorded mode {mode}"
        );
    }

    // The store object pool was not bloated by the swap (no duplicate objects).
    let distinct: std::collections::HashSet<String> = files_owned
        .iter()
        .map(|(_, c, _)| Blake3Hasher::new().hash_hex(c))
        .collect();
    assert_eq!(
        count_objects(store_dir.path()),
        distinct.len(),
        "the atomic swap must not duplicate objects into the store"
    );
}

// ===========================================================================
// CASE 1a' — ZERO-COPY under --linked on ANY filesystem: an atomic swap in
// Linked mode stages SYMLINKS into the local objects, so it copies ZERO bytes on
// every host (no reflink FS required). Runs UNCONDITIONALLY.
// Spec clause (1a, symlink half): "on a symlink (--linked) path the swap copies
// ZERO bytes — staged files are symlinks, not fresh copies".
// ===========================================================================

#[cfg(unix)]
#[test]
fn atomic_swap_linked_mode_is_zero_copy_symlinks_on_any_fs() {
    // Spec clause (1a, symlink): atomic swap in Linked mode must produce SYMLINKS
    // into the local objects (zero bytes copied) on ANY filesystem, and the
    // swapped-in dest must be an exact mirror reading correct content per entry.
    let files_owned = mixed_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "zc-linked", &files);
    let dest = TempDir::under(&parent, "zc-linked-dest");

    store
        .fetch_files_atomic(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("atomic swap in Linked mode must succeed against a LOCAL store");

    let hasher = Blake3Hasher::new();
    for (rel, content, _mode) in &files_owned {
        let link = dest.path().join(rel);
        let md = fs::symlink_metadata(&link)
            .unwrap_or_else(|e| panic!("dest entry {rel} must exist after swap: {e}"));
        assert!(
            md.file_type().is_symlink(),
            "atomic Linked dest entry {rel} MUST be a symlink (zero-copy), not a fresh copy"
        );
        // The link resolves to the LOCAL store object for this checksum.
        let sum = hasher.hash_hex(content);
        let want = object_disk(store_dir.path(), &sum)
            .canonicalize()
            .expect("store object must exist to be linked");
        let got = fs::canonicalize(&link)
            .unwrap_or_else(|e| panic!("symlink {rel} must resolve to the object: {e}"));
        assert_eq!(
            got, want,
            "the swapped-in link for {rel} must point at the LOCAL store object, not a copy"
        );
        assert_eq!(
            &fs::read(&link).unwrap(),
            content,
            "reading through the swapped-in link for {rel} must return the source content"
        );
    }

    // KEYSTONE: zero-copy — no object was DUPLICATED into the store by the swap.
    let distinct: std::collections::HashSet<String> = files_owned
        .iter()
        .map(|(_, c, _)| Blake3Hasher::new().hash_hex(c))
        .collect();
    assert_eq!(
        count_objects(store_dir.path()),
        distinct.len(),
        "atomic Linked swap is zero-copy: it must NOT duplicate objects into the store"
    );
}

// ===========================================================================
// CASE 1b / 4 — ATOMIC ON NON-CoW IS A HARD ERROR. With SNAPDIR_CLONEFILE=0 the
// dest is non-CoW (cow_reflink_supported reports false), so an atomic swap in
// Auto mode would have to byte-copy the whole tree into staging — that is
// FORBIDDEN. The request must return a TYPED, non-panic error, must NOT silently
// byte-copy, and must NOT partially apply (dest unchanged). Runs on EVERY host.
// Spec clause (1b)+(4): "atomic requested where it would duplicate bytes (plain
// copy on non-CoW) is a HARD ERROR" + "when cow_reflink_supported reports false
// an atomic request errors rather than copying".
// ===========================================================================

#[cfg(unix)]
#[test]
fn atomic_swap_on_non_cow_is_typed_hard_error_no_byte_copy_no_partial_apply() {
    // Spec clause (1b)+(4): atomic Auto on a forced non-CoW path (SNAPDIR_CLONEFILE=0,
    // so cow_reflink_supported==false) must be a typed, non-panic HARD ERROR — it
    // must NOT byte-copy and must NOT partially apply (the dest is untouched).
    let files_owned = mixed_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();
    let _e = CloneEnv::set(Some("0")); // force the non-CoW (copy) path everywhere

    // Use the system temp dir: SNAPDIR_CLONEFILE=0 guarantees non-CoW regardless
    // of the underlying FS (the probe routes through the same copy_file machinery).
    let parent = std::env::temp_dir();
    let (store, _store_dir, manifest, _id, _src) = staged_store(&parent, "noncow", &files);
    let dest = TempDir::under(&parent, "noncow-dest");

    // Precondition: the CoW probe must report false under the forced-off knob.
    assert!(
        !cow_reflink_supported(dest.path()).expect("probe must not error on a writable dir"),
        "precondition: SNAPDIR_CLONEFILE=0 must make cow_reflink_supported report false"
    );

    // Plant a sentinel in the dest so we can prove the failed atomic request left
    // the ORIGINAL dest untouched (no partial apply, no clobber).
    let sentinel = dest.path().join("ORIGINAL_SENTINEL.txt");
    fs::write(&sentinel, b"original-dest-content").unwrap();
    let dest_before = tree_snapshot(dest.path());

    let before = clonefile_hits();
    let res = store.fetch_files_atomic(&manifest, dest.path(), MaterializeMode::Auto);
    let after = clonefile_hits();

    let err = res.expect_err(
        "atomic swap on a non-CoW (plain-copy) path MUST be a HARD ERROR — it would \
         duplicate every byte into staging, which is forbidden; got Ok(())",
    );
    assert!(
        is_atomic_unsupported_like(&err),
        "the non-CoW atomic refusal must be a typed, non-panic StoreError naming the \
         unsupported/atomic/copy condition (NOT a bare Io, NOT a panic), got {err:?}"
    );

    // No clone fired (the knob is off) AND — critically — no byte-copy staging
    // happened: the dest must be byte-for-byte what it was before the request.
    assert_eq!(
        after, before,
        "the refused atomic request must not have fired any clone: {before} -> {after}"
    );
    assert_eq!(
        tree_snapshot(dest.path()),
        dest_before,
        "a refused atomic-on-non-CoW request must NOT partially apply — the original \
         dest tree must be byte-for-byte unchanged (no half-staged copy swapped in)"
    );
    assert_eq!(
        fs::read(&sentinel).expect("sentinel must survive"),
        b"original-dest-content",
        "the original dest file must be untouched after the refused atomic request"
    );
    // No leftover staging / dest.old siblings from a partial attempt.
    let parent_of_dest = dest.path().parent().unwrap();
    for e in fs::read_dir(parent_of_dest).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let dest_name = dest
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if name == dest_name {
            continue;
        }
        assert!(
            !(name.starts_with(&dest_name) && (name.contains(".old") || name.contains("stag"))),
            "a refused atomic request must leave no staging/dest.old sibling: found {name}"
        );
    }
}

// ===========================================================================
// CASE 2 — HELD-OPEN FD SURVIVES THE SWAP (durability keystone). A process opens
// a dest file and reads PART of it; an atomic swap to a DIFFERENT snapshot then
// runs; the still-open fd must keep reading the ORIGINAL bytes (the old inode
// survives the rename-aside). Runs UNCONDITIONALLY in Linked mode (zero-copy on
// any FS) so it never false-skips, and ADDITIONALLY in Auto on a CoW host.
// Spec clause (2): "a held-open fd keeps reading old bytes across the swap".
// ===========================================================================

#[cfg(unix)]
#[test]
fn held_open_fd_keeps_reading_old_bytes_across_atomic_swap_linked() {
    // Spec clause (2): an fd opened on a dest file BEFORE an atomic swap keeps
    // reading the ORIGINAL inode's bytes across the swap (POSIX: the swap renames
    // the old tree aside; the inode survives until the fd closes). Linked mode so
    // it is zero-copy on ANY filesystem and never false-skips.
    let parent = coloc_parent();

    // V1 (the snapshot first checked out into the dest) and V2 (a DIFFERENT
    // snapshot swapped in). Same path "data.bin", different content/checksum.
    let v1_content: Vec<u8> = (0..(64 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let v1_files: Vec<(&str, &[u8], &str)> = vec![("data.bin", v1_content.as_slice(), "644")];
    let v2_content: Vec<u8> = (0..(64 * 1024u32))
        .map(|i| ((i * 7 + 3) % 251) as u8)
        .collect();
    let v2_files: Vec<(&str, &[u8], &str)> = vec![("data.bin", v2_content.as_slice(), "644")];
    assert_ne!(
        v1_content, v2_content,
        "V1 and V2 must differ for the test to be meaningful"
    );

    // A single store holding BOTH snapshots' objects.
    let store_dir = TempDir::under(&parent, "heldfd-store");
    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let src1 = TempDir::under(&parent, "heldfd-src1");
    let (m1, _id1) = build_tree(src1.path(), &v1_files);
    store.push(&m1, src1.path()).expect("push v1");
    let src2 = TempDir::under(&parent, "heldfd-src2");
    let (m2, _id2) = build_tree(src2.path(), &v2_files);
    store.push(&m2, src2.path()).expect("push v2");

    let dest = TempDir::under(&parent, "heldfd-dest");

    // First atomic swap: lay down V1.
    store
        .fetch_files_atomic(&m1, dest.path(), MaterializeMode::Linked)
        .expect("initial atomic swap (V1) must succeed");

    let data_path = dest.path().join("data.bin");
    // Open the V1 file and read the FIRST half BEFORE the swap (binding the fd to
    // the V1 inode / object). Follow the link explicitly: open() follows symlinks,
    // so the fd binds to the underlying V1 object inode.
    let mut fd = fs::File::open(&data_path).expect("open dest data.bin before swap");
    let mut head = vec![0u8; v1_content.len() / 2];
    fd.read_exact(&mut head)
        .expect("read first half before swap");
    assert_eq!(
        head,
        &v1_content[..v1_content.len() / 2],
        "the first half read before the swap must be V1 content"
    );

    // Now atomically swap V2 into the SAME dest.
    store
        .fetch_files_atomic(&m2, dest.path(), MaterializeMode::Linked)
        .expect("atomic swap (V2) over the held-open dest must succeed");

    // The path now reflects V2 ...
    assert_eq!(
        fs::read(&data_path).expect("read dest path after swap"),
        v2_content,
        "after the swap the dest PATH must resolve to V2 content"
    );

    // ... but the still-open fd keeps reading the ORIGINAL (V1) bytes: read the
    // REMAINING half from the same fd and assert it is V1's second half.
    let mut tail = vec![0u8; v1_content.len() - v1_content.len() / 2];
    fd.read_exact(&mut tail)
        .expect("the held-open fd must keep reading V1 bytes across the swap");
    assert_eq!(
        tail,
        &v1_content[v1_content.len() / 2..],
        "KEYSTONE: the fd opened before the swap MUST keep reading the ORIGINAL (V1) \
         inode bytes across the atomic swap — long-running processes are never disrupted"
    );
}

#[cfg(unix)]
#[test]
fn held_open_fd_keeps_reading_old_bytes_across_atomic_swap_auto_cow() {
    // Spec clause (2, Auto/reflink half): same held-fd durability on the editable
    // Auto (reflink) path. GATED on a clone-capable host (Auto on non-CoW is a
    // hard error per clause 1b, so the swap could not run there anyway).
    if !reflink_capable() {
        eprintln!(
            "SKIP held_open_fd_keeps_reading_old_bytes_across_atomic_swap_auto_cow: \
             no clone-capable FS (Auto atomic requires zero-copy/CoW)"
        );
        return;
    }

    let parent = coloc_parent();

    let v1_content: Vec<u8> = (0..(80 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let v1_files: Vec<(&str, &[u8], &str)> = vec![("data.bin", v1_content.as_slice(), "644")];
    let v2_content: Vec<u8> = (0..(80 * 1024u32))
        .map(|i| ((i * 11 + 5) % 251) as u8)
        .collect();
    let v2_files: Vec<(&str, &[u8], &str)> = vec![("data.bin", v2_content.as_slice(), "644")];
    assert_ne!(v1_content, v2_content);

    let _g = env_lock();
    let _e = CloneEnv::set(None); // reflink enabled

    let store_dir = TempDir::under(&parent, "heldfd-auto-store");
    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let src1 = TempDir::under(&parent, "heldfd-auto-src1");
    let (m1, _id1) = build_tree(src1.path(), &v1_files);
    store.push(&m1, src1.path()).expect("push v1");
    let src2 = TempDir::under(&parent, "heldfd-auto-src2");
    let (m2, _id2) = build_tree(src2.path(), &v2_files);
    store.push(&m2, src2.path()).expect("push v2");

    let dest = TempDir::under(&parent, "heldfd-auto-dest");
    store
        .fetch_files_atomic(&m1, dest.path(), MaterializeMode::Auto)
        .expect("initial atomic swap (V1, Auto) must succeed");

    let data_path = dest.path().join("data.bin");
    let mut fd = fs::File::open(&data_path).expect("open dest before swap");
    let mut head = vec![0u8; v1_content.len() / 2];
    fd.read_exact(&mut head)
        .expect("read first half before swap");
    assert_eq!(head, &v1_content[..v1_content.len() / 2]);

    store
        .fetch_files_atomic(&m2, dest.path(), MaterializeMode::Auto)
        .expect("atomic swap (V2, Auto) over the held-open dest must succeed");

    assert_eq!(
        fs::read(&data_path).expect("read after swap"),
        v2_content,
        "after the swap the dest PATH must resolve to V2 content"
    );
    let mut tail = vec![0u8; v1_content.len() - v1_content.len() / 2];
    fd.read_exact(&mut tail)
        .expect("the held-open fd must keep reading V1 bytes across the Auto swap");
    assert_eq!(
        tail,
        &v1_content[v1_content.len() / 2..],
        "KEYSTONE (Auto/reflink): the fd opened before the swap MUST keep reading the \
         ORIGINAL (V1) bytes across the atomic swap"
    );
}

// ===========================================================================
// CASE 3 — SWAP-OR-NOTHING (atomicity). A mid-stage failure (a manifest
// referencing an object MISSING from the pool, so staging fails partway BEFORE
// the final rename) must leave the ORIGINAL dest FULLY INTACT — no partial
// mirror, no half-swapped tree, no torn state — and return a typed error. Built
// from an OBSERVABLE failure (missing object), not a private fault-injection
// hook. Runs UNCONDITIONALLY in Linked mode (zero-copy on any FS).
// Spec clause (3): "a simulated mid-stage failure leaves the original dest
// intact (swap-or-nothing)".
// ===========================================================================

#[cfg(unix)]
#[test]
fn mid_stage_failure_leaves_original_dest_intact_swap_or_nothing() {
    // Spec clause (3): a staging failure injected MID-stage (a manifest object
    // missing from the pool) must leave the ORIGINAL dest byte-for-byte intact
    // (no partial/half-swapped tree) and return a typed error — swap-or-nothing.
    let parent = coloc_parent();

    // V1 is a healthy, fully-present snapshot we first lay into the dest.
    let v1_files: Vec<(&str, &[u8], &str)> = vec![
        ("keep.txt", b"V1-keep-content\n".as_slice(), "644"),
        ("sub/inner.bin", b"V1-inner\n".as_slice(), "600"),
    ];

    // V2 is the snapshot we ATTEMPT to swap in — but one of its objects will be
    // MISSING from the pool, so staging must fail partway.
    let present_content = b"V2-present-object\n".to_vec();
    let missing_content = b"V2-OBJECT-THAT-WILL-BE-REMOVED\n".to_vec();
    let v2_files: Vec<(&str, &[u8], &str)> = vec![
        ("keep.txt", present_content.as_slice(), "644"),
        ("ghost.bin", missing_content.as_slice(), "644"),
        ("sub/inner.bin", present_content.as_slice(), "600"),
    ];

    let store_dir = TempDir::under(&parent, "son-store");
    let store = FileStore::from_root(store_dir.path().to_path_buf());

    let src1 = TempDir::under(&parent, "son-src1");
    let (m1, _id1) = build_tree(src1.path(), &v1_files);
    store.push(&m1, src1.path()).expect("push v1");

    let src2 = TempDir::under(&parent, "son-src2");
    let (m2, _id2) = build_tree(src2.path(), &v2_files);
    store.push(&m2, src2.path()).expect("push v2");

    // Lay V1 into the dest via a healthy atomic swap; capture an EXACT snapshot.
    let dest = TempDir::under(&parent, "son-dest");
    store
        .fetch_files_atomic(&m1, dest.path(), MaterializeMode::Linked)
        .expect("initial atomic swap (V1) must succeed");
    // Reading through the V1 links must yield V1 content before we break V2.
    assert_eq!(
        fs::read(dest.path().join("keep.txt")).unwrap(),
        b"V1-keep-content\n",
        "precondition: dest holds V1 keep.txt before the failed swap"
    );
    let dest_before = tree_snapshot(dest.path());

    // OBSERVABLE mid-stage failure: remove V2's ghost.bin object from the pool so
    // staging V2 fails partway (after staging keep.txt / sub, before the final
    // rename). No private hook — just a missing object.
    let ghost_sum = Blake3Hasher::new().hash_hex(&missing_content);
    let ghost_obj = object_disk(store_dir.path(), &ghost_sum);
    let _ = fs::set_permissions(&ghost_obj, fs::Permissions::from_mode(0o644)); // in case hardened
    fs::remove_file(&ghost_obj).expect("remove V2's ghost object to force a mid-stage failure");

    // Attempt the V2 swap — it must FAIL.
    let res = store.fetch_files_atomic(&m2, dest.path(), MaterializeMode::Linked);
    let err = res.expect_err(
        "an atomic swap whose staging references a MISSING object must FAIL (never \
         half-swap a partial tree)",
    );
    assert!(
        is_typed_staging_failure(&err),
        "the mid-stage failure must surface as a typed StoreError (ObjectNotFound / \
         Integrity / Backend / Io), not a panic, got {err:?}"
    );

    // KEYSTONE: the ORIGINAL dest (V1) is byte-for-byte intact — no partial V2
    // tree, no half-swapped state, no leftover ghost.bin, no torn directory.
    assert_eq!(
        tree_snapshot(dest.path()),
        dest_before,
        "swap-or-nothing: a mid-stage staging failure must leave the ORIGINAL dest \
         byte-for-byte intact (no partial/half-swapped V2 tree)"
    );
    assert_eq!(
        fs::read(dest.path().join("keep.txt")).expect("V1 keep.txt must survive"),
        b"V1-keep-content\n",
        "the original V1 keep.txt must be unchanged after the failed V2 swap"
    );
    assert!(
        !dest.path().join("ghost.bin").exists(),
        "no V2 entry (ghost.bin) may appear in the dest after a failed swap"
    );

    // No leftover staging / dest.old sibling from the aborted swap.
    let parent_of_dest = dest.path().parent().unwrap();
    let dest_name = dest
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for e in fs::read_dir(parent_of_dest).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == dest_name {
            continue;
        }
        assert!(
            !(name.starts_with(&dest_name) && (name.contains(".old") || name.contains("stag"))),
            "a failed atomic swap must not leave a staging/dest.old sibling behind: {name}"
        );
    }
}

// ===========================================================================
// CASE 3b — DEST-ABSENT atomic swap: a swap into a non-existent dest is a plain
// materialize (no original to preserve) and SUCCEEDS, producing the exact tree.
// Pins that the swap machinery handles the "nothing to rename aside" edge.
// Spec clause (adversary exhaustiveness): dest-absent => plain checkout.
// ===========================================================================

#[cfg(unix)]
#[test]
fn atomic_swap_into_absent_dest_is_plain_materialize() {
    // Spec clause (dest-absent): an atomic swap whose dest does not yet exist must
    // succeed (there is no original tree to rename aside) and produce the exact
    // mirror. Linked mode so it is zero-copy on any FS.
    let files: Vec<(&str, &[u8], &str)> = vec![
        ("a.txt", b"alpha\n".as_slice(), "644"),
        ("d/b.bin", b"beta\n".as_slice(), "600"),
    ];

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "absent", &files);

    // A dest PATH that does not exist yet (under a real, existing parent).
    let host = TempDir::under(&parent, "absent-host");
    let dest = host.path().join("not-created-yet");
    assert!(!dest.exists(), "precondition: dest must be absent");

    store
        .fetch_files_atomic(&manifest, &dest, MaterializeMode::Linked)
        .expect("atomic swap into an absent dest must succeed (plain materialize)");

    assert!(dest.is_dir(), "the dest must have been created");
    assert_eq!(
        fs::read(dest.join("a.txt")).expect("a.txt"),
        b"alpha\n",
        "absent-dest swap must produce a.txt with the source content"
    );
    assert_eq!(
        fs::read(dest.join("d/b.bin")).expect("d/b.bin"),
        b"beta\n",
        "absent-dest swap must produce the nested d/b.bin with the source content"
    );
    // Still zero-copy (no object duplication into the store).
    let _ = store_dir; // keep the store alive
}

// ###########################################################################
// IMPL-REVEALED CASES (review gate `mirror-atomic-swap-review`)
//
// Added after reading the landed impl (`FileStore::fetch_files_atomic`): the
// staged spec-suite above proved zero-copy / held-fd / swap-or-nothing /
// dest-absent. The impl now reveals concrete sibling-naming + a two-rename swap
// + a dest-exists rename-aside branch; the cases below pin behaviors the
// staged suite did NOT cover. NONE weaken anything above.
// ###########################################################################

/// Returns the names of any sibling of `dest` (under its parent) that looks
/// like an atomic-swap scratch artifact: a `.snapdir-staging*`, a
/// `.snapdir-dest-old*`, or anything that starts with the dest's own basename
/// and carries `.old` / `stag`. Used to prove NO scratch residue survives.
#[cfg(unix)]
fn swap_scratch_siblings(dest: &Path) -> Vec<String> {
    let parent = dest.parent().unwrap();
    let dest_name = dest.file_name().unwrap().to_string_lossy().into_owned();
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(parent) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == dest_name {
                continue;
            }
            let looks_like_swap_scratch = name.starts_with(".snapdir-staging")
                || name.starts_with(".snapdir-dest-old")
                || (name.starts_with(&dest_name)
                    && (name.contains(".old") || name.contains("stag")));
            if looks_like_swap_scratch {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

// ===========================================================================
// CASE R1 — dest.old / STAGING CLEANUP AFTER A *SUCCESSFUL* SWAP. The impl does
// `rename(dest -> .snapdir-dest-old-<tmp>)` + `rename(staging -> dest)` +
// best-effort `remove_dir_all(old)`. The staged suite only checked residue on
// FAILURE paths; this pins that a SUCCESSFUL swap over an existing dest leaves
// NO `.snapdir-dest-old*` / `.snapdir-staging*` sibling behind. Linked mode so
// it runs on any FS. (Impl branch: the dest-exists rename-aside arm.)
// ===========================================================================

#[cfg(unix)]
#[test]
fn successful_swap_over_existing_dest_leaves_no_old_or_staging_sibling() {
    let parent = coloc_parent();

    let v1_files: Vec<(&str, &[u8], &str)> = vec![("data.bin", b"V1-content\n".as_slice(), "644")];
    let v2_files: Vec<(&str, &[u8], &str)> = vec![("data.bin", b"V2-content\n".as_slice(), "644")];

    let store_dir = TempDir::under(&parent, "cleanup-store");
    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let src1 = TempDir::under(&parent, "cleanup-src1");
    let (m1, _i1) = build_tree(src1.path(), &v1_files);
    store.push(&m1, src1.path()).expect("push v1");
    let src2 = TempDir::under(&parent, "cleanup-src2");
    let (m2, _i2) = build_tree(src2.path(), &v2_files);
    store.push(&m2, src2.path()).expect("push v2");

    // RACE ISOLATION: the impl's swap scratch (`.snapdir-staging*` /
    // `.snapdir-dest-old*`, file_store.rs:326,345) is created as a sibling of
    // `dest` under `dest.parent()` and is named by a GLOBAL counter (NOT the
    // dest basename), so `swap_scratch_siblings` (generic-prefix scan) cannot
    // filter it by name. Cargo runs this file's tests in PARALLEL; if `dest`
    // sat directly under the shared `coloc_parent()`, this assertion would
    // catch a CONCURRENT sibling test's transient scratch. Give `dest` its OWN
    // private parent (`outer`) so `swap_scratch_siblings(dest)` only ever lists
    // THIS test's scratch. Linked mode needs no reflink, so a private parent is
    // fine even on the Btrfs CI leg.
    let outer = TempDir::under(&parent, "cleanup-isolated");
    let dest = TempDir::under(outer.path(), "cleanup-dest");
    store
        .fetch_files_atomic(&m1, dest.path(), MaterializeMode::Linked)
        .expect("initial swap (V1) must succeed");
    // After the FIRST swap (over an existing — but freshly created — dest) there
    // must already be no scratch sibling.
    assert!(
        swap_scratch_siblings(dest.path()).is_empty(),
        "no scratch sibling may survive the first successful swap: {:?}",
        swap_scratch_siblings(dest.path())
    );

    // A SECOND swap over the now-populated dest exercises the rename-aside arm
    // (rename dest -> dest.old, rename staging -> dest, remove old).
    store
        .fetch_files_atomic(&m2, dest.path(), MaterializeMode::Linked)
        .expect("second swap (V2) over existing dest must succeed");
    assert_eq!(
        fs::read(dest.path().join("data.bin")).unwrap(),
        b"V2-content\n",
        "the second swap must have installed V2"
    );

    // KEYSTONE: no `.snapdir-dest-old*` and no `.snapdir-staging*` sibling
    // survives a SUCCESSFUL swap — the old tree was removed and staging consumed.
    let leftovers = swap_scratch_siblings(dest.path());
    assert!(
        leftovers.is_empty(),
        "a successful swap must remove the renamed-aside old tree AND consume staging \
         — found leftover scratch sibling(s): {leftovers:?}"
    );
}

// ===========================================================================
// CASE R2 — LINKED IS NEVER REFUSED ON A NON-CoW TARGET. The impl guard is
// `matches!(mode, Auto) && !cow_reflink_supported(dest)`. With SNAPDIR_CLONEFILE=0
// (probe reports false) an Auto swap hard-errors (case 1b above), but a Linked
// swap MUST still SUCCEED — symlinks are zero-copy on any FS. Pins that the
// refusal is the Auto path ONLY. (Impl branch: the mode-aware guard.)
// ===========================================================================

#[cfg(unix)]
#[test]
fn linked_swap_is_never_refused_on_non_cow_target() {
    let files_owned = mixed_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let _g = env_lock();
    let _e = CloneEnv::set(Some("0")); // force non-CoW everywhere

    let parent = std::env::temp_dir();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "linked-noncow", &files);
    let dest = TempDir::under(&parent, "linked-noncow-dest");

    // Precondition: the probe reports false (same condition that makes Auto error).
    assert!(
        !cow_reflink_supported(dest.path()).expect("probe must not error"),
        "precondition: SNAPDIR_CLONEFILE=0 must make cow_reflink_supported report false"
    );

    // Linked mode MUST succeed despite the probe being false — symlinks copy
    // zero bytes on ANY filesystem, so the zero-copy guard must NOT fire here.
    store
        .fetch_files_atomic(&manifest, dest.path(), MaterializeMode::Linked)
        .expect(
            "Linked atomic swap must SUCCEED on a non-CoW target (symlinks are zero-copy on \
             any FS) — only the Auto path may refuse",
        );

    // The swapped-in entries are symlinks (zero-copy), proving no byte-copy.
    let hasher = Blake3Hasher::new();
    for (rel, content, _mode) in &files_owned {
        let link = dest.path().join(rel);
        let md = fs::symlink_metadata(&link).unwrap_or_else(|e| panic!("entry {rel}: {e}"));
        assert!(
            md.file_type().is_symlink(),
            "Linked swap on a non-CoW target must still produce a SYMLINK for {rel}, not a copy"
        );
        assert_eq!(&fs::read(&link).unwrap(), content, "content for {rel}");
    }
    // No object duplicated into the store (zero-copy).
    let distinct: std::collections::HashSet<String> = files_owned
        .iter()
        .map(|(_, c, _)| hasher.hash_hex(c))
        .collect();
    assert_eq!(
        count_objects(store_dir.path()),
        distinct.len(),
        "Linked swap on non-CoW must not duplicate objects into the store"
    );
}

// ===========================================================================
// CASE R3 — EXISTING DEST WITH EXTRANEOUS FILES → EXACT MIRROR, EXTRANEOUS GONE.
// The impl builds staging FRESH from the manifest then swaps it in wholesale, so
// any extraneous file present in the old dest (not in the new manifest) must be
// ABSENT afterward. Pins the swap is a wholesale replacement, not an in-place
// merge. (Impl branch: rename-aside + fresh staging.)
// ===========================================================================

#[cfg(unix)]
#[test]
fn swap_replaces_dest_wholesale_extraneous_files_are_gone() {
    let parent = coloc_parent();

    let files: Vec<(&str, &[u8], &str)> = vec![
        ("keep.txt", b"manifest-content\n".as_slice(), "644"),
        ("sub/inner.bin", b"inner\n".as_slice(), "600"),
    ];
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "wholesale", &files);

    // RACE ISOLATION (same rationale as R1): this test ends with a
    // `swap_scratch_siblings(dest)` generic-prefix scan, which would otherwise
    // catch a concurrent sibling test's transient `.snapdir-staging*` /
    // `.snapdir-dest-old*` scratch if `dest` sat directly under the shared
    // `coloc_parent()`. Give `dest` its own private parent so the scan only
    // ever lists THIS test's scratch.
    let outer = TempDir::under(&parent, "wholesale-isolated");
    // Pre-populate dest with EXTRANEOUS entries NOT in the manifest.
    let dest = TempDir::under(outer.path(), "wholesale-dest");
    fs::write(dest.path().join("EXTRANEOUS_TOP.txt"), b"stale-top").unwrap();
    fs::create_dir_all(dest.path().join("stale_dir")).unwrap();
    fs::write(dest.path().join("stale_dir/junk.bin"), b"stale-nested").unwrap();
    fs::write(dest.path().join("keep.txt"), b"OLD-different-content").unwrap();

    store
        .fetch_files_atomic(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("wholesale swap must succeed");

    // The manifest entries are present with the NEW content ...
    assert_eq!(
        fs::read(dest.path().join("keep.txt")).unwrap(),
        b"manifest-content\n",
        "keep.txt must hold the NEW manifest content, not the stale dest content"
    );
    assert_eq!(
        fs::read(dest.path().join("sub/inner.bin")).unwrap(),
        b"inner\n",
        "nested manifest entry must be present"
    );
    // ... and EVERY extraneous entry is GONE (wholesale replacement).
    assert!(
        !dest.path().join("EXTRANEOUS_TOP.txt").exists(),
        "extraneous top-level file must be GONE after a wholesale swap"
    );
    assert!(
        !dest.path().join("stale_dir").exists(),
        "extraneous directory must be GONE after a wholesale swap"
    );
    assert!(
        swap_scratch_siblings(dest.path()).is_empty(),
        "no scratch sibling may survive: {:?}",
        swap_scratch_siblings(dest.path())
    );
    let _ = store_dir;
}

// ===========================================================================
// CASE R4 — THE SWAP IS A REAL RENAME (SAME-FS, ATOMIC): the dest DIRECTORY's
// identity (device+inode) changes WHOLESALE across the swap. A real `rename` of
// a freshly-built sibling staging dir into place gives the dest a brand-new
// inode; an in-place mutation (merge / byte-copy into the existing dir) would
// KEEP the same inode. This proves the swap is rename-based, not a copy-merge.
// (Impl: staging is a same-parent temp_sibling renamed into place.)
// ===========================================================================

#[cfg(unix)]
#[test]
fn swap_is_a_real_rename_dest_inode_changes_wholesale() {
    let parent = coloc_parent();

    let v1_files: Vec<(&str, &[u8], &str)> = vec![("f.bin", b"v1\n".as_slice(), "644")];
    let v2_files: Vec<(&str, &[u8], &str)> = vec![("f.bin", b"v2\n".as_slice(), "644")];

    let store_dir = TempDir::under(&parent, "inode-store");
    let store = FileStore::from_root(store_dir.path().to_path_buf());
    let src1 = TempDir::under(&parent, "inode-src1");
    let (m1, _i1) = build_tree(src1.path(), &v1_files);
    store.push(&m1, src1.path()).expect("push v1");
    let src2 = TempDir::under(&parent, "inode-src2");
    let (m2, _i2) = build_tree(src2.path(), &v2_files);
    store.push(&m2, src2.path()).expect("push v2");

    let dest = TempDir::under(&parent, "inode-dest");
    store
        .fetch_files_atomic(&m1, dest.path(), MaterializeMode::Linked)
        .expect("first swap must succeed");

    let before = fs::metadata(dest.path()).expect("stat dest after V1");
    let (dev_before, ino_before) = (before.dev(), before.ino());

    store
        .fetch_files_atomic(&m2, dest.path(), MaterializeMode::Linked)
        .expect("second swap must succeed");

    let after = fs::metadata(dest.path()).expect("stat dest after V2");
    let (dev_after, ino_after) = (after.dev(), after.ino());

    // Same filesystem (a real rename never crosses devices) ...
    assert_eq!(
        dev_before, dev_after,
        "the swap must stay on one filesystem (staging is a same-FS sibling)"
    );
    // ... but the dest directory inode changed WHOLESALE — the swap renamed a
    // freshly-built tree into place rather than mutating the old dir in place.
    assert_ne!(
        ino_before, ino_after,
        "KEYSTONE: an atomic rename-swap must give the dest a NEW directory inode \
         (wholesale replacement); an unchanged inode would mean an in-place merge/copy"
    );
    assert_eq!(
        fs::read(dest.path().join("f.bin")).unwrap(),
        b"v2\n",
        "the swapped-in dest must hold V2 content"
    );
}
