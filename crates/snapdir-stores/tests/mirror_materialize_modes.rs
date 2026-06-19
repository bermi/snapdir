//! Adversarial integration suite for **materialization modes** (phase 32, gate
//! `mirror-materialize-modes-spec-tests`).
//!
//! BLACK-BOX: authored from the gate SPEC alone, with NO visibility into the
//! `--linked` / linked-materialization implementation (it does NOT exist yet —
//! `fetch_files` is copy/reflink-only today). It will NOT compile/pass until the
//! stores-impl teammate moves this file into
//! `crates/snapdir-stores/tests/mirror_materialize_modes.rs`, lands the
//! materialization-mode API + `0444` object hardening + the local-source guard,
//! and wires the staged cases. Do NOT weaken any assertion to make it green — if
//! a behavior here fails against the landed impl, that is a real bug in the impl,
//! not in this test.
//!
//! ## SPEC under test (Phase 32 materialization modes)
//!
//! `fetch_files` gains an explicit **materialization mode** for how a manifest is
//! written into a destination. Two modes compose with `--delete`:
//!
//!   1. **auto (default)** — reflink (clonefile/FICLONE) where the dest fs
//!      supports it, else plain copy. INDEPENDENT, EDITABLE inodes; open-fd-safe.
//!      A write to an editable reflinked dest file (a CoW break) must leave the
//!      SOURCE store object byte-identical.
//!   2. **`--linked`** — dest entries are **symlinks into the LOCAL store
//!      objects** (`.objects/<h0:3>/<h3:6>/<h6:9>/<h9:>`); zero-copy on any fs;
//!      **read-only-enforced** by making store/cache objects mode `0444`. The
//!      thin store-view, NOT editable. Hard-errors if the object source is
//!      non-local (cannot symlink to a remote object).
//!
//! ## HARD INVARIANTS this suite pins (must be PREVENTED, not discouraged)
//!
//!   (a) `--linked` produces SYMLINKS whose target is the LOCAL store object for
//!       that entry's checksum (the sharded `.objects/...` path); reading through
//!       the link yields the correct content.
//!   (b) Linked objects are `0444`; a write THROUGH a symlinked dest file FAILS
//!       (EACCES / permission error) and leaves the store object's bytes
//!       UNCHANGED. `--linked` must NEVER re-chmod the object to a writable mode.
//!   (c) `--linked` to a NON-LOCAL object source is a HARD ERROR (clear, typed,
//!       non-panic) — you cannot symlink to a remote object.
//!   (d) auto mode reflinks on CoW (`CopyMethod::Cloned`) and copies otherwise;
//!       the dest inodes are INDEPENDENT and EDITABLE; a write to an editable
//!       reflinked dest file leaves the SOURCE store object byte-identical (the
//!       CoW break does not corrupt the shared object).
//!
//! ## Assumed (contracted) public API — impl lane MUST honor or re-point
//!
//! The exact mode-selection API does NOT exist yet. This suite is authored
//! against a PLAUSIBLE shape; the impl may RE-POINT the type names only (NOT
//! weaken the contract):
//!
//!   * `snapdir_stores::MaterializeMode` — an enum with at least:
//!       - `MaterializeMode::Auto`   (reflink-or-copy; editable; today's default)
//!       - `MaterializeMode::Linked` (symlink into local objects; 0444; read-only)
//!   * `FileStore::fetch_files_with_mode(&self, manifest: &Manifest, dest: &Path,
//!        mode: MaterializeMode) -> Result<(), StoreError>` — the mode-carrying
//!     fetch. `Auto` MUST be byte-for-byte today's `fetch_files`.
//!   * Non-local-source linked refusal surfaces as a TYPED, non-panic
//!     `StoreError` — assumed `StoreError::Unsupported { .. }` (a plausible new
//!     `#[non_exhaustive]` variant). The suite tolerates ANY non-`Io`/non-panic
//!     typed error there via `is_unsupported_like`, and FLAGS the layer question
//!     in the handoff (the refusal may live at the CLI layer; if the stores API
//!     cannot express a non-local object source, the impl/design must resolve it
//!     at the right layer and re-point `linked_remote_source_is_hard_error`).
//!
//! ## Env / parallelism note
//!
//! `SNAPDIR_CLONEFILE` is process-global and Rust runs `#[test]`s multithreaded
//! in one binary, so every test that toggles the knob or compares a
//! `clonefile_hits()` delta holds a single process-wide `ENV_LOCK` for its whole
//! body and RESTORES the prior value on drop. Mirrors `apfs_clone.rs` /
//! `reflink.rs` / `clone_skip.rs`.
//!
//! ## CoW gating
//!
//! CI / dev hosts vary: macOS dev is APFS (clone fires), Linux CI runs both an
//! ext4 leg (NO reflink) and a Btrfs loopback leg (real FICLONE). So the
//! "reflink actually fired" assertions are GATED: on macOS we assert the clone
//! fired; on Linux we assert it fired ONLY when a reflink root
//! (`SNAPDIR_REFLINK_TEST_DIR`) is provided, else we exercise the copy fallback
//! (which must STILL yield independent editable inodes). The
//! independence/correctness invariants are asserted on EVERY host, clone or copy.

// Wiring (shape only, no assertion change): silence workspace `-D warnings`
// clippy lints on this adversary-authored suite. The mode enum / new method are
// contracted-symbol presence checks; the round-trip tuples and skip matches are
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};

// CONTRACTED new symbols (assumed; impl lane may re-point the type names only):
//   * `MaterializeMode` enum (Auto | Linked)
//   * `FileStore::fetch_files_with_mode(manifest, dest, mode)`
use snapdir_stores::{FileStore, MaterializeMode, StreamStore};

// ---------------------------------------------------------------------------
// Test scaffolding (no dev-dependencies; mirrors apfs_clone.rs / reflink.rs).
// ---------------------------------------------------------------------------

/// A unique temp dir removed on drop. `new` places it under the system temp dir;
/// `under` places it under an explicit parent (used to co-locate src + store on
/// a reflink root so FICLONE can actually fire — same-FS co-location).
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
            "snapdir-materialize-test-{}-{tag}-{n}",
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
        // Best-effort cleanup. A linked checkout makes objects 0444 and dest
        // entries symlinks; both still remove cleanly (perms on the link's
        // PARENT dir, not the 0444 target, govern unlink), so a plain
        // remove_dir_all suffices, but we restore writability defensively in
        // case a future impl hardens parent dirs too.
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
    /// `Some("0")` disables the clone fast-path (forces `fs::copy`); `None`
    /// leaves it default-enabled.
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

/// `true` iff `e` is a typed, non-`Io` refusal (the kind the impl must use for
/// the "`--linked` to a non-local object source" hard error). Tolerant of the
/// exact variant name so the impl may re-point to `Unsupported`/`Backend`/a new
/// variant — but a bare `Io` or a panic is NOT acceptable. `ObjectNotFound` is
/// also acceptable shape for "can't materialize a non-local object".
fn is_unsupported_like(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::Backend { .. } | StoreError::ObjectNotFound { .. }
    ) || {
        // Future `StoreError::Unsupported { .. }` (or similar) — match on the
        // Display text so this stays robust to the precise variant name.
        let s = e.to_string().to_lowercase();
        !matches!(e, StoreError::Io(_))
            && (s.contains("unsupported")
                || s.contains("not supported")
                || s.contains("non-local")
                || s.contains("remote")
                || s.contains("local")
                || s.contains("symlink"))
    }
}

/// A representative tree exercising the boundary sizes + path shapes the SPEC
/// calls out: a file LARGER than 256 KiB, a tiny file, a 0-byte file, a nested
/// deep path, and a unicode/space path. Contents are deterministic.
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
        (
            "uni \u{2728}/space name.txt",
            "snowman \u{2603}\n".as_bytes().to_vec(),
            "644",
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
/// unset => `None` (caller falls back to a plain temp root + copy path). The
/// macOS dev host needs no such root (APFS clones under the system temp dir).
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
/// On Linux with a reflink root, everything must live UNDER it (FICLONE returns
/// EXDEV across filesystems). Elsewhere the system temp dir is fine.
fn coloc_parent() -> PathBuf {
    reflink_root().unwrap_or_else(std::env::temp_dir)
}

// ===========================================================================
// CASE (a) — `--linked` produces SYMLINKS into the LOCAL store objects, and
// reading THROUGH the link returns the correct content.
// Spec clause (a): "--linked produces symlinks to LOCAL store objects" + the
// symlinked file READS correct content + the link target is the right object key.
// ===========================================================================

#[cfg(unix)]
#[test]
fn linked_mode_creates_symlinks_into_local_objects_with_correct_targets() {
    // Spec clause (a): every regular-file dest entry under --linked is a SYMLINK
    // whose canonical target is the sharded local-store object for that entry's
    // checksum, and reading through the link yields the source content.
    let files_owned = mixed_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "linked-targets", &files);
    let dest = TempDir::under(&parent, "linked-targets-dest");

    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("linked checkout must succeed against a LOCAL store");

    let hasher = Blake3Hasher::new();
    for (rel, content, _mode) in &files_owned {
        let link = dest.path().join(rel);
        let lmeta = fs::symlink_metadata(&link)
            .unwrap_or_else(|e| panic!("linked dest entry {rel} must exist: {e}"));
        assert!(
            lmeta.file_type().is_symlink(),
            "linked dest entry {rel} MUST be a symlink (the thin store-view), not a regular file"
        );

        // The link's canonical target must be the LOCAL store object for this
        // content's checksum (the sharded .objects/... path).
        let sum = hasher.hash_hex(content);
        let want_obj = object_disk(store_dir.path(), &sum)
            .canonicalize()
            .expect("store object must exist on disk to be symlinked");
        let got_target = fs::canonicalize(&link)
            .unwrap_or_else(|e| panic!("symlink {rel} must resolve to an existing object: {e}"));
        assert_eq!(
            got_target, want_obj,
            "the symlink for {rel} must point at the LOCAL store object \
             (.objects/<sharded checksum>), not a copy"
        );

        // Reading THROUGH the link returns the correct content.
        let read = fs::read(&link).unwrap_or_else(|e| panic!("read-through {rel} failed: {e}"));
        assert_eq!(
            &read, content,
            "reading through the symlink for {rel} must return the source content"
        );
    }

    // KEYSTONE: zero-copy — no object was DUPLICATED into the dest tree; the
    // .objects pool still holds exactly the distinct objects (+ the dest holds
    // only links, which carry no object bytes of their own).
    let distinct: std::collections::HashSet<String> = files_owned
        .iter()
        .map(|(_, c, _)| Blake3Hasher::new().hash_hex(c))
        .collect();
    assert_eq!(
        count_objects(store_dir.path()),
        distinct.len(),
        "linked mode is zero-copy: it must NOT duplicate objects into the store"
    );
}

// ===========================================================================
// CASE (b) — Linked objects are 0444 and a WRITE THROUGH the symlinked dest
// file FAILS, leaving the store object byte-identical (no shared-object
// corruption; --linked never re-chmods the object writable).
// Spec clause (b): "objects are 0444 so a write through a link FAILS (store
// uncorrupted)" + "--linked must never re-chmod the object to a writable mode".
// ===========================================================================

#[cfg(unix)]
#[test]
fn linked_objects_are_0444_and_write_through_link_fails_leaving_object_intact() {
    // Spec clause (b): the store objects a linked checkout points at are mode
    // 0444; an in-place write THROUGH a symlinked dest file is REJECTED
    // (permission error) and the underlying store object's bytes are UNCHANGED.
    let content = b"shared-object-must-not-be-corruptible\n".to_vec();
    let files: Vec<(&str, &[u8], &str)> = vec![("doc.txt", content.as_slice(), "644")];

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "ro-link", &files);
    let dest = TempDir::under(&parent, "ro-link-dest");

    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("linked checkout must succeed");

    let sum = Blake3Hasher::new().hash_hex(&content);
    let obj = object_disk(store_dir.path(), &sum);

    // (1) The store object is hardened to 0444 (read-only for everyone).
    assert_eq!(
        mode_bits(&obj),
        Some(0o444),
        "linked mode must harden the store object to 0444 (read-only-enforced)"
    );

    // (2) A write THROUGH the symlinked dest file must FAIL (the link resolves to
    // the 0444 object; opening it for write is EACCES). The exact error kind may
    // vary, but it MUST be an error — never a silent success.
    let link = dest.path().join("doc.txt");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "dest entry must be a symlink for the write-through to target the object"
    );
    let write_res = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&link)
        .and_then(|mut f| {
            use std::io::Write as _;
            f.write_all(b"CORRUPTION")
        });
    assert!(
        write_res.is_err(),
        "writing through a linked (0444) dest file MUST fail — the store object \
         must not be corruptible through the link; got Ok(())"
    );
    if let Err(e) = &write_res {
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "the write-through failure must be a permission error (0444 object), got {e:?}"
        );
    }

    // (3) The store object's bytes are UNCHANGED after the failed write.
    assert_eq!(
        fs::read(&obj).expect("object still readable"),
        content,
        "the store object bytes must be byte-identical after a failed write-through"
    );
    // And the object still verifies through the read-time BLAKE3 backstop.
    assert_eq!(
        store.get_object(&sum).expect("object verifies"),
        content,
        "the store object must still verify (get_object) after the blocked write"
    );

    // (4) --linked must NOT have re-chmod'd the object to a writable manifest
    // mode (the source file was 0644; the object must be 0444, not 0644).
    assert_ne!(
        mode_bits(&obj),
        Some(0o644),
        "linked mode must NOT re-chmod the object to the writable manifest mode"
    );
}

// ===========================================================================
// CASE (c) — `--linked` against a NON-LOCAL object source is a HARD ERROR.
// Spec clause (c): "--linked to a NON-LOCAL object source is a HARD ERROR"
// (clear, typed, non-panic). You cannot symlink to a remote object.
//
// NOTE TO IMPL LANE (flagged in handoff): the meaningful "non-local source"
// distinction may only be expressible at the CLI layer (the stores `FileStore`
// is inherently local). We encode the INTENDED contract two ways so at least one
// is exercisable at the stores API:
//   (c1) a linked checkout whose required object is simply ABSENT locally (the
//        closest the stores API can get to "can't reach the object source")
//        MUST be a typed, non-panic error — never a dangling symlink.
//   (c2) IF the impl exposes a way to attempt a linked materialization from a
//        non-`FileStore` (remote) source through the stores API, that MUST be a
//        typed Unsupported-like error. If it does not, the impl must encode and
//        enforce this at the CLI layer and re-point this case there.
// ===========================================================================

#[cfg(unix)]
#[test]
fn linked_missing_local_object_is_typed_error_not_a_dangling_symlink() {
    // Spec clause (c1): a linked checkout that cannot resolve a required object
    // to a real LOCAL store object must HARD-ERROR (typed, non-panic) and must
    // NOT leave a dangling symlink in the dest (a dangling link is the failure
    // mode "symlink to a non-local/absent object" must never silently produce).
    let content = b"object-will-be-missing\n".to_vec();
    let files: Vec<(&str, &[u8], &str)> = vec![("ghost.bin", content.as_slice(), "644")];

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "missing-obj", &files);

    // Remove the backing object so the linked materialization has nothing local
    // to point at (the stores-API analogue of a non-local/unreachable source).
    let sum = Blake3Hasher::new().hash_hex(&content);
    let obj = object_disk(store_dir.path(), &sum);
    let _ = fs::set_permissions(&obj, fs::Permissions::from_mode(0o644)); // in case a prior run hardened it
    fs::remove_file(&obj).expect("remove the backing object");

    let dest = TempDir::under(&parent, "missing-obj-dest");
    let res = store.fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked);

    let err = res.expect_err(
        "a linked checkout with no resolvable LOCAL object MUST hard-error \
         (never silently create a dangling symlink)",
    );
    assert!(
        is_unsupported_like(&err),
        "the linked-no-local-object failure must be a typed, non-panic StoreError \
         (ObjectNotFound / Unsupported-like), got {err:?}"
    );

    // No dangling symlink may have been left in the dest.
    let link = dest.path().join("ghost.bin");
    if let Ok(meta) = fs::symlink_metadata(&link) {
        if meta.file_type().is_symlink() {
            assert!(
                fs::metadata(&link).is_err(),
                "internal: a leftover link to a present object would invalidate \
                 the precondition"
            );
            panic!("FORBIDDEN: a dangling symlink to a missing/non-local object was created");
        }
    }
}

// ===========================================================================
// CASE (d) — auto mode reflinks on CoW (CopyMethod::Cloned) and copies
// otherwise; dest inodes are INDEPENDENT + EDITABLE; a write to an editable
// reflinked dest file leaves the SOURCE store object byte-identical.
// Spec clause (d): "auto mode reflinks on CoW and copies otherwise, independent
// editable inodes" + "a write to an editable reflinked file leaves the source
// object byte-identical".
// ===========================================================================

#[cfg(unix)]
#[test]
fn auto_mode_independent_editable_inode_write_does_not_corrupt_source_object() {
    // Spec clause (d): an auto-mode dest file is a real, INDEPENDENT, EDITABLE
    // inode (reflink-CoW or copy). Writing to it (a CoW break on reflink fs) must
    // leave the SOURCE store object byte-identical — no shared-object corruption.
    // A >256 KiB payload so a reflink shares real extents and a CoW-break bug
    // would surface.
    let content: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let files: Vec<(&str, &[u8], &str)> = vec![("editable.bin", content.as_slice(), "644")];

    let parent = coloc_parent();

    let _g = env_lock();
    let _e = CloneEnv::set(None); // clone fast-path enabled (reflink where supported)

    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "auto-edit", &files);
    let dest = TempDir::under(&parent, "auto-edit-dest");

    let before = snapdir_stores::clonefile_hits();
    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Auto)
        .expect("auto checkout must succeed");
    let after = snapdir_stores::clonefile_hits();

    let dest_file = dest.path().join("editable.bin");
    let dest_meta = fs::symlink_metadata(&dest_file).expect("dest file metadata");

    // (1) The dest entry is a REGULAR FILE (NOT a symlink) — auto mode is the
    // editable mode, never the thin link view.
    assert!(
        dest_meta.file_type().is_file(),
        "auto-mode dest entry must be a regular file (editable), never a symlink"
    );

    // (2) It is an INDEPENDENT inode from the store object (reflink or copy both
    // yield a distinct inode; a hardlink/symlink to the object would NOT).
    let sum = Blake3Hasher::new().hash_hex(&content);
    let obj = object_disk(store_dir.path(), &sum);
    let obj_meta = fs::symlink_metadata(&obj).expect("object metadata");
    assert_ne!(
        (dest_meta.dev(), dest_meta.ino()),
        (obj_meta.dev(), obj_meta.ino()),
        "auto-mode dest file must be an INDEPENDENT inode, not the store object itself"
    );

    // (3) On a clone-capable host the reflink fast-path MUST have fired (so the
    // CoW-break invariant below is not vacuously over a plain copy). On a
    // copy-only host the copy path is exercised instead.
    if reflink_capable() {
        assert!(
            after > before,
            "auto mode on a CoW-capable host must reflink (CopyMethod::Cloned) — \
             clonefile_hits must advance: {before} -> {after}"
        );
    }

    // (4) The dest file is EDITABLE: writing to it must SUCCEED (manifest mode
    // 0644). This is the editable-inode contract.
    let mut new_bytes = content.clone();
    new_bytes[0] ^= 0xff;
    new_bytes[content.len() - 1] ^= 0xff;
    fs::write(&dest_file, &new_bytes).expect("auto-mode dest file must be writable/editable");

    // (5) KEYSTONE: after editing the dest file (a CoW break on reflink fs), the
    // SOURCE store object's bytes are UNCHANGED — independent inodes, no shared-
    // object corruption.
    assert_eq!(
        fs::read(&obj).expect("object still readable"),
        content,
        "editing the reflinked/copied dest file must NOT change the source store \
         object bytes (CoW break / independent inode)"
    );
    assert_eq!(
        store.get_object(&sum).expect("object verifies"),
        content,
        "the store object must still verify after the dest file was edited"
    );

    // (6) And the edited dest file actually holds the NEW bytes (the write landed
    // on the dest inode, proving independence in the other direction).
    assert_eq!(
        fs::read(&dest_file).expect("read edited dest"),
        new_bytes,
        "the edit must have landed on the independent dest inode"
    );
}

// ===========================================================================
// CASE (d2) — auto mode with the clone fast-path FORCED OFF (SNAPDIR_CLONEFILE=0)
// still yields an EDITABLE, INDEPENDENT inode (plain copy fallback), and editing
// it leaves the source object intact. Pins the "copies otherwise" half of clause
// (d) on EVERY host (no reflink fs required).
// ===========================================================================

#[cfg(unix)]
#[test]
fn auto_mode_copy_fallback_is_independent_and_editable() {
    // Spec clause (d, copy half): with SNAPDIR_CLONEFILE=0 auto mode plain-copies;
    // the dest is still an independent editable inode and an edit does not touch
    // the source object. Asserts the counter does NOT advance (no clone fired).
    let content: Vec<u8> = (0..(128 * 1024u32)).map(|i| (i % 197) as u8).collect();
    let files: Vec<(&str, &[u8], &str)> = vec![("copy.bin", content.as_slice(), "600")];

    let _g = env_lock();
    let _e = CloneEnv::set(Some("0")); // force the plain fs::copy path

    let parent = std::env::temp_dir();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "auto-copy", &files);
    let dest = TempDir::under(&parent, "auto-copy-dest");

    let before = snapdir_stores::clonefile_hits();
    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Auto)
        .expect("auto (copy) checkout must succeed");
    let after = snapdir_stores::clonefile_hits();
    assert_eq!(
        after, before,
        "SNAPDIR_CLONEFILE=0 must force the copy path; no clone may fire: {before} -> {after}"
    );

    let dest_file = dest.path().join("copy.bin");
    let sum = Blake3Hasher::new().hash_hex(&content);
    let obj = object_disk(store_dir.path(), &sum);

    let d = fs::symlink_metadata(&dest_file).unwrap();
    let o = fs::symlink_metadata(&obj).unwrap();
    assert!(d.file_type().is_file(), "copy dest must be a regular file");
    assert_ne!(
        (d.dev(), d.ino()),
        (o.dev(), o.ino()),
        "the copied dest file must be an independent inode"
    );

    // Restored content correct + restored mode honors the manifest (0600).
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        content,
        "copied content must match"
    );
    assert_eq!(
        mode_bits(&dest_file),
        Some(0o600),
        "auto-mode restored file must carry the manifest-recorded mode 0600"
    );

    // Edit the copy; the source object stays byte-identical.
    let mut edited = content.clone();
    edited[10] ^= 0xa5;
    fs::write(&dest_file, &edited).expect("copied dest must be editable");
    assert_eq!(
        fs::read(&obj).unwrap(),
        content,
        "editing the copied dest file must not change the source object"
    );
}

// ===========================================================================
// CASE (e) — auto mode is byte-for-byte today's `fetch_files`. The mode-carrying
// fetch with Auto must produce restored content + perms identical to the legacy
// `fetch_files` (the impl must not regress the additive default).
// Spec clause: auto (default, no flag) == today's reflink-or-copy materialize.
// ===========================================================================

#[cfg(unix)]
#[test]
fn auto_mode_equals_legacy_fetch_files() {
    // Spec clause (auto == today): fetch_files_with_mode(.., Auto) must restore
    // the exact same content and permission bits as the legacy fetch_files for
    // the same manifest — proving Auto is the unchanged additive default.
    let files_owned = mixed_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let parent = coloc_parent();
    let (store, _store_dir, manifest, _id, _src) = staged_store(&parent, "auto-eq", &files);

    let dest_legacy = TempDir::under(&parent, "auto-eq-legacy");
    let dest_auto = TempDir::under(&parent, "auto-eq-auto");

    store
        .fetch_files(&manifest, dest_legacy.path())
        .expect("legacy fetch_files");
    store
        .fetch_files_with_mode(&manifest, dest_auto.path(), MaterializeMode::Auto)
        .expect("auto fetch_files_with_mode");

    for (rel, content, mode) in &files_owned {
        let lp = dest_legacy.path().join(rel);
        let ap = dest_auto.path().join(rel);
        assert_eq!(
            fs::read(&lp).unwrap(),
            fs::read(&ap).unwrap(),
            "auto-mode content for {rel} must equal legacy fetch_files"
        );
        assert_eq!(
            fs::read(&ap).unwrap(),
            *content,
            "auto-mode content for {rel} must equal the source"
        );
        assert_eq!(
            mode_bits(&lp),
            mode_bits(&ap),
            "auto-mode perms for {rel} must equal legacy fetch_files"
        );
        let want = u32::from_str_radix(mode, 8).unwrap();
        assert_eq!(
            mode_bits(&ap),
            Some(want),
            "auto-mode restored {rel} must carry the manifest-recorded mode {mode}"
        );
        // Neither path may be a symlink (auto is editable).
        assert!(
            fs::symlink_metadata(&ap).unwrap().file_type().is_file(),
            "auto-mode {rel} must be a regular file, not a symlink"
        );
    }
}

// ===========================================================================
// CASE (f) — Mixed tree under --linked: directories are real dirs, files are
// links, nested + unicode/space paths handled, 0-byte object links read empty.
// Spec clause (adversary exhaustiveness): mixed manifests (files + dirs +
// nested); symlinked file reads correct content for every entry including the
// degenerate 0-byte object.
// ===========================================================================

#[cfg(unix)]
#[test]
fn linked_mode_mixed_tree_dirs_real_files_links_zero_byte_ok() {
    // Spec clause (mixed tree): under --linked, directory entries are REAL
    // directories (not links), every regular-file entry is a symlink to its
    // object, nested + unicode/space paths resolve, and the 0-byte object links
    // to an empty object that reads back empty.
    let files_owned = mixed_files();
    let files: Vec<(&str, &[u8], &str)> = files_owned
        .iter()
        .map(|(p, c, m)| (*p, c.as_slice(), *m))
        .collect();

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "linked-mixed", &files);
    let dest = TempDir::under(&parent, "linked-mixed-dest");

    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("linked mixed checkout must succeed");

    // Directory entries from the manifest are real directories in the dest.
    for entry in manifest.entries() {
        if entry.path_type != PathType::Directory {
            continue;
        }
        let rel = entry.path.strip_prefix("./").unwrap_or(entry.path.as_str());
        let rel = rel.strip_suffix('/').unwrap_or(rel);
        if rel.is_empty() {
            continue; // the root "./" entry maps to dest itself
        }
        let d = dest.path().join(rel);
        let m =
            fs::symlink_metadata(&d).unwrap_or_else(|e| panic!("dir entry {rel} must exist: {e}"));
        assert!(
            m.file_type().is_dir(),
            "directory entry {rel} must be a real directory under --linked, not a symlink"
        );
    }

    // Every file (incl. nested + unicode/space + 0-byte) is a link reading
    // back its source content.
    for (rel, content, _mode) in &files_owned {
        let link = dest.path().join(rel);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "file entry {rel} must be a symlink under --linked"
        );
        assert_eq!(
            &fs::read(&link).unwrap(),
            content,
            "read-through {rel} must equal source content"
        );
    }

    // The 0-byte object exists, is 0444, and the link reads back empty.
    let empty_sum = Blake3Hasher::new().hash_hex(b"");
    let empty_obj = object_disk(store_dir.path(), &empty_sum);
    assert!(empty_obj.is_file(), "0-byte object must exist");
    assert_eq!(
        mode_bits(&empty_obj),
        Some(0o444),
        "the 0-byte object must also be hardened to 0444 under --linked"
    );
    assert!(
        fs::read(dest.path().join("empty")).unwrap().is_empty(),
        "the link to the 0-byte object must read back empty"
    );
}

// ===========================================================================
// CASE (g) — Linked checkout is IDEMPOTENT: a second linked checkout into a
// dest already linked succeeds and the links/objects are unchanged (objects
// stay 0444, links still resolve). Re-running must not error or corrupt.
// Spec clause (adversary exhaustiveness): idempotency / re-runs.
// ===========================================================================

#[cfg(unix)]
#[test]
fn linked_mode_second_run_is_idempotent() {
    // Spec clause (idempotency): a repeated linked checkout into the same dest
    // must succeed, leave the objects 0444, and leave the links resolving to the
    // same objects (no duplication, no corruption, no error).
    let content = b"idempotent-linked-content\n".to_vec();
    let files: Vec<(&str, &[u8], &str)> = vec![("a/b/c.txt", content.as_slice(), "644")];

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "linked-idem", &files);
    let dest = TempDir::under(&parent, "linked-idem-dest");

    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("first linked checkout");
    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("second linked checkout must be idempotent (no error)");

    let sum = Blake3Hasher::new().hash_hex(&content);
    let obj = object_disk(store_dir.path(), &sum);
    assert_eq!(
        mode_bits(&obj),
        Some(0o444),
        "object must remain 0444 after an idempotent re-run"
    );
    let link = dest.path().join("a/b/c.txt");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the dest entry must still be a symlink after the second run"
    );
    assert_eq!(
        fs::read(&link).unwrap(),
        content,
        "the link must still read the correct content after the second run"
    );
    assert_eq!(
        count_objects(store_dir.path()),
        1,
        "an idempotent re-run must not duplicate objects"
    );
}

// ===========================================================================
// REVIEW-ADDED CASES (impl now visible). These pin branches the landed impl
// in `src/file_store.rs` exposes: `harden_object_readonly` idempotency, the
// `atomic_symlink` canonical target, the `cow_reflink_supported` public probe
// (residue + SNAPDIR_CLONEFILE=0 + non-CoW path), and the no-corruption depth
// invariant via the public `get_object`. NONE of these weaken the contract;
// they strengthen it against the revealed implementation.
// ===========================================================================

// ---------------------------------------------------------------------------
// REVIEW (h) — 0444 idempotency / RE-hardening. A SECOND linked checkout over an
// already-0444 object stays 0444 and succeeds (the atomic-symlink replace path),
// and writing THROUGH the (now re-checked-out) link still fails. Pins the
// `harden_object_readonly` idempotent no-op branch (`mode & 0o7777 == 0o444`) +
// `atomic_symlink`'s rename-over-existing-link replace, distinctly from the
// first-run hardening exercised in case (g).
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn linked_rehardening_second_checkout_stays_0444_and_write_still_fails() {
    let content = b"re-harden-must-stay-0444\n".to_vec();
    let files: Vec<(&str, &[u8], &str)> = vec![("re.txt", content.as_slice(), "644")];

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "reharden", &files);
    let dest = TempDir::under(&parent, "reharden-dest");

    let sum = Blake3Hasher::new().hash_hex(&content);
    let obj = object_disk(store_dir.path(), &sum);

    // First linked checkout hardens the object to 0444.
    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("first linked checkout");
    assert_eq!(
        mode_bits(&obj),
        Some(0o444),
        "first linked checkout must harden the object to 0444"
    );

    // A SECOND linked checkout, with the object ALREADY 0444, must succeed (the
    // `harden_object_readonly` idempotent no-op branch + the atomic-symlink
    // rename-over-existing-link replace) and leave the object 0444 — never
    // re-chmod it writable.
    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("second linked checkout over an already-0444 object must succeed (idempotent)");
    assert_eq!(
        mode_bits(&obj),
        Some(0o444),
        "re-hardening an already-0444 object must keep it 0444 (idempotent no-op)"
    );

    // The dest entry is still a symlink and writing THROUGH it still fails.
    let link = dest.path().join("re.txt");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "after the re-checkout the dest entry must still be a symlink"
    );
    let write_res = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&link)
        .and_then(|mut f| {
            use std::io::Write as _;
            f.write_all(b"CORRUPTION")
        });
    assert!(
        write_res.is_err(),
        "writing through the re-hardened (0444) link MUST still fail"
    );
    if let Err(e) = &write_res {
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "the re-hardened write-through failure must be a permission error, got {e:?}"
        );
    }

    // The object's bytes are still intact + still verify through get_object.
    assert_eq!(
        store.get_object(&sum).expect("object verifies after re-checkout"),
        content,
        "the object must still verify (get_object) after the idempotent re-checkout + blocked write"
    );
    assert_eq!(
        count_objects(store_dir.path()),
        1,
        "the re-checkout must not duplicate the object"
    );
}

// ---------------------------------------------------------------------------
// REVIEW (i) — Symlink-chain / canonicalization. The created link's canonical
// target is EXACTLY the sharded `.objects/<h0:3>/<h3:6>/<h6:9>/<h9:>` object —
// no `..`/double-indirection surprise, no chain-of-links — and the link's
// immediate readlink target is NOT itself another symlink. Pins
// `atomic_symlink(object, target)`'s single-hop link to the object_disk_path.
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn linked_target_is_single_hop_canonical_sharded_object_no_chain() {
    let content = b"single-hop-canonical-target\n".to_vec();
    let files: Vec<(&str, &[u8], &str)> = vec![("only.bin", content.as_slice(), "644")];

    let parent = coloc_parent();
    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "canon", &files);
    let dest = TempDir::under(&parent, "canon-dest");

    store
        .fetch_files_with_mode(&manifest, dest.path(), MaterializeMode::Linked)
        .expect("linked checkout");

    let sum = Blake3Hasher::new().hash_hex(&content);
    let link = dest.path().join("only.bin");

    // The link's IMMEDIATE readlink target must itself be a regular file (the
    // object), i.e. a SINGLE hop — not another symlink (no link-to-link chain).
    let immediate = fs::read_link(&link).expect("dest entry must be a symlink");
    let immediate_meta =
        fs::symlink_metadata(&immediate).expect("the link's immediate target must exist");
    assert!(
        immediate_meta.file_type().is_file(),
        "the symlink must point DIRECTLY at the object file (single hop, not a chain to another link)"
    );

    // The canonical resolution of the link equals the canonical sharded object
    // path — confirming no `..`/double-indirection produced a different inode.
    let want_obj = object_disk(store_dir.path(), &sum)
        .canonicalize()
        .expect("object must exist");
    let got = fs::canonicalize(&link).expect("link must resolve");
    assert_eq!(
        got, want_obj,
        "the link's canonical target must be exactly the sharded .objects/<...> object"
    );

    // The canonical target path actually lives under the store's .objects pool
    // (defends against a future regression pointing links elsewhere).
    let objects_root = store_dir
        .path()
        .join(".objects")
        .canonicalize()
        .expect(".objects must exist");
    assert!(
        got.starts_with(&objects_root),
        "the resolved target {got:?} must live under the store .objects pool {objects_root:?}"
    );

    // And reading through the (single-hop) link returns the right content.
    assert_eq!(
        fs::read(&link).expect("read through link"),
        content,
        "reading through the single-hop link must return the source content"
    );
}

// ---------------------------------------------------------------------------
// REVIEW (j) — `cow_reflink_supported(dest)` public probe. Returns a bool
// WITHOUT corrupting/leaving probe temps in dest's parent; honors
// `SNAPDIR_CLONEFILE=0` (=> false). On a clone-capable host the default probe
// reports true. Black-box against the public crate-root fn.
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn cow_reflink_probe_reports_bool_leaves_no_residue_and_honors_clonefile_off() {
    let _g = env_lock();

    let parent = coloc_parent();
    let dir = TempDir::under(&parent, "cow-probe");
    // The eventual dest need not exist; use a child path under our temp dir so
    // the probe runs in `dir` (its parent).
    let dest = dir.path().join("would-be-dest");

    let snapshot_entries = |d: &Path| -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };
    let before = snapshot_entries(dir.path());

    // (1) Default knob (clone enabled where the FS supports it).
    {
        let _e = CloneEnv::set(None);
        let supported = snapdir_stores::cow_reflink_supported(&dest)
            .expect("probe must not error on a writable dir");
        // On a clone-capable host (macOS APFS, or a Linux reflink root) the
        // probe MUST report true; elsewhere it may legitimately be false. Either
        // way it returns a bool (no panic, no error).
        if reflink_capable() {
            assert!(
                supported,
                "cow_reflink_supported must report true on a clone-capable host (default knob)"
            );
        }
    }

    // (2) SNAPDIR_CLONEFILE=0 forces the probe to report false (it routes
    // through the same copy_file machinery, which honors the knob).
    {
        let _e = CloneEnv::set(Some("0"));
        let forced_off = snapdir_stores::cow_reflink_supported(&dest)
            .expect("probe must not error with the clone knob forced off");
        assert!(
            !forced_off,
            "SNAPDIR_CLONEFILE=0 must make cow_reflink_supported report false"
        );
    }

    // (3) No residue: the probe must clean up BOTH its probe temps; the dir
    // holds exactly what it held before (the would-be dest itself is never
    // created as a file — only its parent is touched).
    let after = snapshot_entries(dir.path());
    assert_eq!(
        before, after,
        "cow_reflink_supported must leave NO probe temp files behind in the dest parent: \
         before={before:?} after={after:?}"
    );
    // Belt-and-suspenders: no stray probe-named entries.
    for name in &after {
        assert!(
            !name.contains("snapdir-cow-probe"),
            "a probe temp file leaked into the dest parent: {name}"
        );
    }
}

// ---------------------------------------------------------------------------
// REVIEW (k) — `cow_reflink_supported` on a NON-CoW path reports false without
// residue. We force the copy fallback via SNAPDIR_CLONEFILE=0 (the
// host-independent way to exercise the "not CoW" branch: copy_file returns
// CopyMethod::Copied, so the probe must report false) and confirm no temp file
// is left. (A genuinely non-CoW filesystem is not portably mountable in a unit
// test; the forced-off knob exercises the identical `!Cloned => false` branch.)
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn cow_reflink_probe_false_on_non_cow_path_no_residue() {
    let _g = env_lock();
    let _e = CloneEnv::set(Some("0")); // force the non-CoW (copy) branch

    let dir = TempDir::new("cow-probe-noncow");
    let dest = dir.path().join("nested/created/on/demand/dest");

    // The probe creates `dest`'s parent if missing; confirm it still reports
    // false (copy path) and errors on nothing.
    let supported = snapdir_stores::cow_reflink_supported(&dest)
        .expect("probe must succeed (creating the parent) and not error");
    assert!(
        !supported,
        "the non-CoW (copy) path must make cow_reflink_supported report false"
    );

    // The parent was created; it must hold NO probe residue.
    let probe_parent = dest.parent().unwrap();
    let leaked: Vec<String> = fs::read_dir(probe_parent)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains("snapdir-cow-probe"))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leaked.is_empty(),
        "the non-CoW probe must leave no probe temps behind, found: {leaked:?}"
    );
}

// ---------------------------------------------------------------------------
// REVIEW (l) — No-corruption DEPTH after an auto-mode CoW/copy edit. The
// keystone in case (d) checks the object after editing a >256KiB reflinked
// file; this case additionally drives the full BLAKE3 verify path (get_object)
// AND a second auto re-fetch into a fresh dest to prove the object remained a
// faithful source after the edit (no shared-extent CoW-break leakage). Pins
// the verify-on-fetch backstop interacting with an auto edit.
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn auto_edit_then_object_still_verifies_and_refetches_clean() {
    let content: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let files: Vec<(&str, &[u8], &str)> = vec![("v.bin", content.as_slice(), "644")];

    let parent = coloc_parent();

    let _g = env_lock();
    let _e = CloneEnv::set(None);

    let (store, store_dir, manifest, _id, _src) = staged_store(&parent, "verify-depth", &files);
    let dest1 = TempDir::under(&parent, "verify-depth-d1");

    store
        .fetch_files_with_mode(&manifest, dest1.path(), MaterializeMode::Auto)
        .expect("first auto checkout");

    // Edit the (possibly reflinked) dest file — a CoW break.
    let dest_file = dest1.path().join("v.bin");
    let mut edited = content.clone();
    edited[0] ^= 0xff;
    edited[content.len() / 2] ^= 0xff;
    edited[content.len() - 1] ^= 0xff;
    fs::write(&dest_file, &edited).expect("auto dest must be editable");

    let sum = Blake3Hasher::new().hash_hex(&content);

    // (1) The object still verifies via the public get_object BLAKE3 backstop.
    assert_eq!(
        store
            .get_object(&sum)
            .expect("object verifies after the edit"),
        content,
        "after a CoW-break edit the source object must still BLAKE3-verify (get_object)"
    );

    // (2) A SECOND auto fetch into a FRESH dest restores the ORIGINAL content,
    // proving the edit never leaked into the shared object via reflinked extents.
    let dest2 = TempDir::under(&parent, "verify-depth-d2");
    store
        .fetch_files_with_mode(&manifest, dest2.path(), MaterializeMode::Auto)
        .expect("second auto checkout into a fresh dest");
    assert_eq!(
        fs::read(dest2.path().join("v.bin")).expect("read re-fetched file"),
        content,
        "a re-fetch after a CoW-break edit must restore the ORIGINAL (uncorrupted) content"
    );
    assert_eq!(
        count_objects(store_dir.path()),
        1,
        "the edit + re-fetch must not have duplicated/added objects"
    );
}
