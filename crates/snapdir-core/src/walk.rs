//! In-process filesystem walk producing a frozen-format [`Manifest`].
//!
//! This module reproduces the original `snapdir-manifest generate` behavior in
//! pure Rust, consuming the frozen [`manifest`](crate::manifest),
//! [`merkle`](crate::merkle) and [`excludes`](crate::excludes) APIs without
//! changing any of them. It walks a directory tree and emits one
//! [`ManifestEntry`] per file (`F`) and directory (`D`), computing per-file
//! content checksums with a [`Hasher`] and per-directory checksums/sizes with
//! [`directory_checksum`].
//!
//! ## Behaviors matched against the oracle
//!
//! - **Traversal** mirrors `find`/`find -L`: every directory becomes a `D`
//!   entry (path ending `/`) and every regular file directly inside it becomes
//!   an `F` entry. Directories are recorded even when empty.
//! - **Symlinks** are *followed by default* ([`FollowMode::Follow`], the
//!   oracle's `find -L`): a symlink to a directory is reported as a directory
//!   and descended into, a symlink to a file as a file, inheriting the
//!   target's type/permissions/size/checksum. [`FollowMode::NoFollow`] (plain
//!   `find`) drops symlinks entirely — they appear as neither `D` nor `F`.
//! - **Permissions** are the octal mode bits, matching `stat -f '%A'` (macOS)
//!   / `stat -c '%a'` (Linux): the low 12 bits of `st_mode` rendered in octal
//!   with no leading zero (e.g. `755`, `644`, `700`).
//! - **File size** is the content byte length (`%z` / `%s`). **Directory size**
//!   is the *sum of its direct members' sizes* (files and subdirectories),
//!   excluding the directory's own `stat` size — matching the oracle's
//!   `_snapdir_manifest_sum_lines` over the direct children.
//! - **Excludes** are applied via [`ExcludeMatcher`] against the *absolute*
//!   path of each candidate directory and file, mirroring the oracle's
//!   `find … | grep -E -v "$EXCLUDE"` (the filter runs before the relative
//!   `./` rewrite). A `%system%` expansion forces [`FollowMode::NoFollow`];
//!   the caller resolves that via [`expand_excludes`](crate::excludes::expand_excludes).
//! - **Paths** are absolute under [`PathMode::Absolute`], or rewritten to a
//!   leading `./` under [`PathMode::Relative`] (the oracle's
//!   `sed -E "s| \.?${root_dir}| .|"`). Directory paths always end with `/`.
//! - **Ordering** is `sort -k5` (byte-wise on the path), delegated to
//!   [`Manifest`]'s own sort.
//!
//! Per the library-purity principle this module reads the filesystem at the
//! *given* root path (that is its job) but reads no `$HOME`/config/environment
//! for behavior: the root, options, excludes and hasher all arrive as
//! parameters, and errors surface as the typed [`WalkError`].

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use thiserror::Error;

use crate::copy_guard::CopyGuard;
use crate::excludes::{ExcludeMatcher, FollowMode};
use crate::hash_file::HashFile;
use crate::manifest::{Manifest, ManifestEntry, PathType};
use crate::merkle::Hasher;
use crate::progress::{Meter, Phase};

/// Whether emitted paths are absolute or rewritten relative to the root.
///
/// Mirrors the oracle's `--absolute` flag: the default is
/// [`Relative`](PathMode::Relative) (paths prefixed with `./`), and
/// `--absolute` keeps the full absolute path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathMode {
    /// Rewrite paths to a leading `./` relative to the root (the default).
    #[default]
    Relative,
    /// Keep absolute paths (`--absolute`).
    Absolute,
}

/// Options controlling a [`walk`].
///
/// All inputs are parameters: this struct carries the symlink-follow setting,
/// the relative/absolute path mode, and the optional compiled exclude matcher.
/// The root path and [`Hasher`] are passed to [`walk`] directly.
#[derive(Debug, Clone, Default)]
pub struct WalkOptions {
    /// Whether to follow symlinks ([`FollowMode::Follow`] by default).
    pub follow: FollowMode,
    /// Whether to emit absolute or relative (`./`) paths.
    pub path_mode: PathMode,
    /// An optional compiled exclude matcher. When `Some`, any directory or
    /// file whose absolute path matches is dropped (`grep -E -v`).
    pub exclude: Option<ExcludeMatcher>,
    /// Desired cross-file hashing parallelism (`--walk-jobs` /
    /// `$SNAPDIR_WALK_JOBS`).
    ///
    /// Resolution: `Some(n)` with `n > 0` uses exactly `n` worker threads;
    /// `Some(0)` and `None` fall back to a default of
    /// [`available_parallelism`](std::thread::available_parallelism) capped by
    /// [`MAX_WALK_JOBS`]. `Some(1)` is an honest single-threaded path. The walk
    /// enumerates the tree single-threaded, then hashes every discovered file in
    /// parallel inside a *bounded*, scoped [`rayon::ThreadPool`] sized to the
    /// resolved count (never the global pool).
    ///
    /// The value **never changes the output**: results are written back into a
    /// fixed per-file slot keyed by discovery identity, so the manifest and the
    /// snapshot id are byte-identical regardless of hash-completion order (the
    /// directory checksum sorts+dedups its children and the manifest sorts by
    /// path). Large files are still hashed memory-mapped (see
    /// [`hash_file`](crate::hash_file)); when there are at least as many pending
    /// files as worker threads each file is hashed single-threaded to avoid
    /// oversubscribing the bounded pool, otherwise BLAKE3's intra-file `rayon`
    /// path is allowed so spare cores still help a lone big file.
    pub walk_jobs: Option<usize>,

    /// Optional local **object-store roots** hint enabling the linked-mode
    /// checksum-reuse fast path. Defaults to **empty** (the fast path is
    /// DORMANT — every existing call site is byte-identical).
    ///
    /// When non-empty AND the active hasher reports
    /// [`recovers_object_keys`](crate::hash_file::HashFile::recovers_object_keys)
    /// (true only for plain, non-keyed BLAKE3), a **symlink-to-file** whose
    /// canonical target is a valid object under one of these roots'
    /// `.objects/<3>/<3>/<3>/<rest>` layout has its content checksum
    /// **recovered from the path** (via [`recover_object_key`](crate::recover::recover_object_key))
    /// instead of being read and hashed — the recovered key is byte-identical to
    /// the hash the content would produce on a healthy store. Eligibility is
    /// per-entry; ineligible symlinks fall back to a normal followed-content
    /// hash, and a target whose object is MISSING (dangling) raises a typed
    /// [`WalkError::DanglingLinkedObject`].
    ///
    /// The CLI passes the store root(s) here regardless of strict-verify; the
    /// strict override is carried by [`verify_linked_objects`](WalkOptions::verify_linked_objects)
    /// instead (so a strict run still recognizes a dangling object and still
    /// re-hashes-and-ERRORS on a corrupted one). For the non-default-algo /
    /// keyed cases the CLI leaves this empty so the content is re-hashed. Each
    /// root is a `<store-root>` directory holding a `.objects/` pool (the path
    /// `FileStore` materializes linked symlinks into).
    pub object_store_roots: Vec<PathBuf>,

    /// **Strict-verify override** for the linked-mode fast path. Defaults to
    /// **`false`** — the fast path TRUSTS the object's address (recovers the
    /// checksum from the symlink target's path WITHOUT reading the bytes), which
    /// is the byte-identical default behavior.
    ///
    /// When `true` AND an entry would otherwise take the fast path (the same
    /// eligibility the recover path uses: a symlink whose canonical target is a
    /// valid object under a hinted [`object_store_roots`](WalkOptions::object_store_roots)
    /// root, with a hasher that
    /// [`recovers_object_keys`](crate::hash_file::HashFile::recovers_object_keys)),
    /// the walk does NOT trust the address: it **reads + hashes the content**
    /// (`H'`), recovers the address (`H`) from the object path, and compares. On
    /// a match it records `H` (correct). On a mismatch it returns a typed
    /// [`WalkError::LinkedObjectIntegrity`] naming the offending symlink and its
    /// object — the store's content no longer matches the address it is filed
    /// under. This is the `SNAPDIR_VERIFY_COPIES=1` re-hash-and-error keystone.
    ///
    /// This flag ONLY affects entries that WOULD have taken the fast path.
    /// Ineligible entries (wrong / keyed algo, escaped the store, non-linked)
    /// are unaffected and hash normally — `true` here never turns those into an
    /// error. A dangling target still raises
    /// [`WalkError::DanglingLinkedObject`] regardless of this flag.
    ///
    /// The CLI sets this to `true` when `SNAPDIR_VERIFY_COPIES=1`; core stays
    /// pure / env-free — this bool is the seam.
    pub verify_linked_objects: bool,
}

/// Errors raised while walking the filesystem.
#[derive(Debug, Error)]
pub enum WalkError {
    /// The root path is not absolute. The walk needs an absolute root so it can
    /// rewrite relative paths exactly as the oracle does (it `readlink`s the
    /// argument to an absolute path first); the CLI lane resolves the user's
    /// argument before calling [`walk`].
    #[error("walk root must be an absolute path, got {0:?}")]
    RootNotAbsolute(PathBuf),

    /// The root path does not resolve to a directory.
    #[error("walk root is not a directory: {0:?}")]
    RootNotDirectory(PathBuf),

    /// An I/O error occurred while reading the tree at `path`.
    #[error("i/o error while walking {path:?}: {source}")]
    Io {
        /// The path being read when the error occurred.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// A path could not be rendered as UTF-8. The frozen manifest format is
    /// UTF-8 text; non-UTF-8 paths cannot be represented.
    #[error("path is not valid UTF-8: {0:?}")]
    NonUtf8Path(PathBuf),

    /// A regular file enumerated during discovery was gone before/at hashing —
    /// a transient tree-in-flux race (the tree changed under snapdir), NOT a
    /// durable IO fault. Distinct from [`WalkError::Io`], which is reserved for
    /// genuine permission/IO errors.
    #[error(
        "file vanished during walk (tree changed under snapdir): {path:?}; \
         re-run on a quiescent tree"
    )]
    FileVanishedDuringWalk {
        /// The path of the file that vanished mid-walk.
        path: PathBuf,
    },

    /// A file's size/mtime/content changed while it was being hashed, so the
    /// recorded checksum/size would be incoherent (detected via a mid-mmap
    /// `SIGBUS` fault or a stat-before/stat-after / bytes-vs-recorded-size
    /// discrepancy). The tree changed under snapdir.
    #[error(
        "file changed during walk (tree changed under snapdir): {path:?}; \
         re-run on a quiescent tree"
    )]
    FileChangedDuringWalk {
        /// The path of the file that changed mid-hash.
        path: PathBuf,
    },

    /// A linked-mode fast-path symlink pointed at an object under a hinted
    /// store root, but the object is **missing** (dangling): the checkout's
    /// backing object was GC'd / removed. The fast path will neither silently
    /// drop the entry nor read garbage — it surfaces this typed error naming the
    /// symlink (and its target), so an in-flux / pruned store fails cleanly
    /// instead of panicking. (Only ever raised when the object-store-roots hint
    /// is active; without the hint a dangling symlink is dropped as before,
    /// matching `find -L`.)
    #[error(
        "linked object missing (dangling symlink into the store): {path:?} -> {target:?}; \
         the backing object was removed/GC'd"
    )]
    DanglingLinkedObject {
        /// The dest symlink whose target object is missing.
        path: PathBuf,
        /// The resolved object path the symlink pointed at.
        target: PathBuf,
    },

    /// Strict-verify (`SNAPDIR_VERIFY_COPIES=1`,
    /// [`WalkOptions::verify_linked_objects`]) re-hashed a linked object's
    /// CONTENT and found it no longer matches the address its object path is
    /// filed under: the store is corrupt (the content was mutated while keeping
    /// — or having been moved under — the wrong address). The fast path would
    /// have trusted the address; strict-verify catches the integrity violation
    /// and refuses to record a checksum that does not match the bytes. Only ever
    /// raised when both the object-store-roots hint and `verify_linked_objects`
    /// are active, and only for an entry that WOULD have taken the fast path.
    #[error(
        "linked object integrity check failed (content does not match its address): \
         {path:?} -> {target:?}; addressed as {expected}, content hashes to {actual}"
    )]
    LinkedObjectIntegrity {
        /// The dest symlink whose backing object's content is corrupt.
        path: PathBuf,
        /// The resolved object path the symlink pointed at.
        target: PathBuf,
        /// The hash recovered from the object's address (what it claims to be).
        expected: String,
        /// The hash of the object's actual content (what it really is).
        actual: String,
    },

    /// A directory invariant expected during the bottom-up finalize pass was
    /// violated because the tree mutated mid-walk (e.g. a directory vanished
    /// after discovery). Replaces the former `expect()` panics so an in-flux
    /// tree yields a clean typed error rather than a backtrace.
    #[error(
        "tree structure changed during walk (tree changed under snapdir): {path:?}; \
         re-run on a quiescent tree"
    )]
    TreeStructureChanged {
        /// The path/key whose structural invariant was violated.
        path: PathBuf,
    },
}

impl WalkError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        WalkError::Io {
            path: path.into(),
            source,
        }
    }
}

/// Renders the octal permission string for a file mode, matching
/// `stat -f '%A'` (macOS) / `stat -c '%a'` (Linux): the low 12 mode bits in
/// octal with no leading zero (e.g. `755`, `644`, `4755`).
fn octal_permissions(mode: u32) -> String {
    format!("{:o}", mode & 0o7777)
}

/// Returns a path as `&str`, or a [`WalkError::NonUtf8Path`].
fn path_str(path: &Path) -> Result<&str, WalkError> {
    path.to_str()
        .ok_or_else(|| WalkError::NonUtf8Path(path.to_path_buf()))
}

/// Upper bound on the auto-resolved cross-file hashing parallelism.
///
/// When `walk_jobs` is `None`/`Some(0)` the walk uses
/// [`available_parallelism`](std::thread::available_parallelism) capped at this
/// value, so a many-core host does not spawn an unbounded hashing pool. An
/// explicit `Some(n)` is honored verbatim (the caller asked for exactly `n`).
const MAX_WALK_JOBS: usize = 16;

/// Resolves the desired [`WalkOptions::walk_jobs`] to a concrete worker count
/// (always `>= 1`). `Some(n>0)` → `n` (honored verbatim); `Some(0)`/`None` →
/// `available_parallelism` capped by [`MAX_WALK_JOBS`], falling back to `1`.
fn resolve_jobs(walk_jobs: Option<usize>) -> usize {
    match walk_jobs {
        Some(n) if n > 0 => n,
        _ => std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .clamp(1, MAX_WALK_JOBS),
    }
}

/// A discovered file entry, before path rewriting. During discovery the
/// `checksum`/`size` slots are left empty and the file's content is hashed in a
/// later parallel pass keyed by `(dir_key, file_index)`, then written back here.
struct FileRecord {
    /// Absolute path of the file.
    abs_path: String,
    permissions: String,
    /// Filled by the parallel hashing pass (empty during discovery).
    checksum: String,
    size: u64,
}

/// A file discovered but not yet hashed: its content path plus the identity of
/// the [`FileRecord`] slot (its owning directory key + index in that
/// directory's `files` vec) to write the `(checksum, size)` result back into.
struct PendingHash {
    /// Absolute path of the directory owning this file (the `dirs` map key).
    dir_key: String,
    /// Index of this file in its directory's `files` vec.
    file_index: usize,
    /// Path to hash the content through (follows symlinks).
    content_path: PathBuf,
    /// The size recorded at discovery (the entry's own `lstat` length). The
    /// hash pass compares the bytes it actually streams/maps against this to
    /// catch a file that GREW or SHRANK mid-hash (→ `FileChangedDuringWalk`).
    recorded_size: u64,
    /// Whether this entry is a followed symlink. A symlink's recorded SIZE is
    /// its own `lstat` length (deliberately != the dereferenced content
    /// length), so the bytes-vs-`recorded_size` drift check is SKIPPED for it.
    is_symlink: bool,
}

/// A discovered directory, holding its absolute path and (filled during the
/// post-order pass) its computed checksum and member-size total.
struct DirRecord {
    /// Absolute path of the directory (no trailing slash, except root `/`).
    abs_path: String,
    permissions: String,
    /// Absolute paths of direct child directories, in discovery order.
    child_dirs: Vec<String>,
    /// Direct child files.
    files: Vec<FileRecord>,
}

/// Walks the directory tree rooted at `root`, producing a [`Manifest`] that
/// matches the original `snapdir-manifest` output byte-for-byte for the same
/// tree and checksum function.
///
/// `root` must be an **absolute** path to a directory (the CLI lane resolves
/// the user's argument first, mirroring the oracle's `readlink`). `hasher`
/// supplies the content/merkle checksum function (BLAKE3 by default; the
/// `--checksum-bin` matrix swaps in [`Md5Hasher`](crate::merkle::Md5Hasher) /
/// [`Sha256Hasher`](crate::merkle::Sha256Hasher) / keyed BLAKE3). `options`
/// carries the follow mode, path mode and optional exclude matcher.
///
/// # Errors
///
/// Returns [`WalkError`] if `root` is not absolute, is not a directory, holds a
/// non-UTF-8 path, or if an I/O error occurs while reading the tree.
pub fn walk<H: Hasher + HashFile + Sync>(
    root: &Path,
    options: &WalkOptions,
    hasher: &H,
) -> Result<Manifest, WalkError> {
    walk_with_meter(root, options, hasher, None)
}

/// Like [`walk`], but records hashing progress into an optional [`Meter`].
///
/// When `meter` is `Some`, the phase starts at [`Phase::Discovering`] while the
/// tree is enumerated (each regular file bumps the discovered counter), then
/// flips to [`Phase::Hashing`] with `objects_total` set to the discovered file
/// count before the parallel hash pass; for each regular file whose bytes are
/// read and hashed, the file's byte length is added to the meter's bytes-in
/// counter and the file is counted as one finished object. When `meter` is
/// `None` this behaves exactly like [`walk`].
///
/// The recording is purely advisory: the returned [`Manifest`] is
/// **byte-identical** whether or not a meter is supplied — the meter is updated
/// with a couple of cheap [`Ordering::Relaxed`](std::sync::atomic::Ordering)
/// atomic ops per file and never influences traversal or output.
///
/// # Errors
///
/// Returns [`WalkError`] under the same conditions as [`walk`].
pub fn walk_with_meter<H: Hasher + HashFile + Sync>(
    root: &Path,
    options: &WalkOptions,
    hasher: &H,
    meter: Option<&Meter>,
) -> Result<Manifest, WalkError> {
    // Discard the guard side channel: the manifest is byte-identical whether or
    // not guards are captured. The existing entry points return just the
    // `Manifest`, unchanged.
    walk_inner(root, options, hasher, meter, false).map(|(manifest, _guards)| manifest)
}

/// Like [`walk_with_meter`], but ALSO returns a [`CopyGuard`] side channel: a
/// `HashMap` keyed by the **absolute working-tree path** of each captured file
/// (`FileRecord.abs_path`, i.e. the path a store later clones from), valued by
/// its `(size, mtime, ctime, ino)` [`CopyGuard`].
///
/// This is an *additive* second return: the [`Manifest`] is **byte-identical**
/// to what [`walk`] / [`walk_with_meter`] produce for the same tree — the guard
/// map is never serialized into the manifest and never influences traversal,
/// ordering, checksums or the snapshot id. A store may re-`stat` a guarded
/// source path at clone time and skip the redundant post-copy re-hash iff all
/// four fields still match (see [`copy_guard`](crate::copy_guard)).
///
/// ## Which entries get a guard (symlink-conservative)
///
/// A guard is emitted **only for a plain regular file whose hashed content
/// path equals its own path** — i.e. a real (non-symlink) file. Followed
/// symlinks (`find -L`), where the manifest entry path and the dereferenced
/// content path differ, are **omitted**: the path the store would re-`stat`
/// (the symlink's own `lstat`) is not the path whose bytes were hashed, so
/// trusting it would be unsound. Omitting them simply means the store re-hashes
/// (no skip) — always safe. Directories, broken symlinks, and special files
/// never get a guard. On non-unix targets the map is always empty
/// ([`CopyGuard::from_metadata`] returns `None`), so the optimization is inert.
///
/// # Errors
///
/// Returns [`WalkError`] under the same conditions as [`walk`].
pub fn walk_with_guards<H: Hasher + HashFile + Sync>(
    root: &Path,
    options: &WalkOptions,
    hasher: &H,
    meter: Option<&Meter>,
) -> Result<(Manifest, HashMap<PathBuf, CopyGuard>), WalkError> {
    walk_inner(root, options, hasher, meter, true)
}

/// Shared implementation behind [`walk`], [`walk_with_meter`] and
/// [`walk_with_guards`]. When `capture_guards` is `true` it populates and
/// returns the [`CopyGuard`] map; otherwise the map is empty. The traversal,
/// hashing and emitted [`Manifest`] are identical regardless of the flag.
fn walk_inner<H: Hasher + HashFile + Sync>(
    root: &Path,
    options: &WalkOptions,
    hasher: &H,
    meter: Option<&Meter>,
    capture_guards: bool,
) -> Result<(Manifest, HashMap<PathBuf, CopyGuard>), WalkError> {
    if let Some(meter) = meter {
        meter.set_phase(Phase::Discovering);
    }
    if !root.is_absolute() {
        return Err(WalkError::RootNotAbsolute(root.to_path_buf()));
    }

    // Resolve the root's metadata following symlinks (the oracle always works
    // on the resolved root directory).
    let root_meta = std::fs::metadata(root).map_err(|e| WalkError::io(root, e))?;
    if !root_meta.is_dir() {
        return Err(WalkError::RootNotDirectory(root.to_path_buf()));
    }
    // The oracle's `stat -f '%A'` / `stat -c '%a'` does NOT follow symlinks, so
    // a directory's PERMISSIONS column always comes from its own `lstat`. For
    // the root we `lstat` it directly (it is normally a real directory; if it
    // is itself a symlink the user passed, its own perms still apply).
    let root_lstat = std::fs::symlink_metadata(root).map_err(|e| WalkError::io(root, e))?;
    let root_permissions = octal_permissions(root_lstat.permissions().mode());

    let root_str = path_str(root)?.to_owned();

    // Discover every directory (depth-first, following symlinks per `follow`),
    // recording its direct files and direct child directories. We collect into
    // an ordered map keyed by absolute path so the post-order pass can compute
    // directory checksums bottom-up. Discovery is SINGLE-THREADED and does NOT
    // hash file contents — each leaf file is recorded as a `PendingHash` whose
    // `(dir_key, file_index)` identifies the fixed `FileRecord` slot to fill.
    let mut dirs: BTreeMap<String, DirRecord> = BTreeMap::new();
    let mut pending: Vec<PendingHash> = Vec::new();
    // The out-of-band guard side channel: populated only when requested, and
    // only for plain regular files (see `walk_with_guards`). Keyed by the
    // file's absolute working-tree path (what a store later clones from).
    let mut guards: HashMap<PathBuf, CopyGuard> = HashMap::new();
    let mut guard_sink = capture_guards.then_some(&mut guards);
    // Linked-mode fast path is enabled only when a store-root hint was supplied
    // AND the active hasher's digest matches the store's addressing algorithm
    // (plain, non-keyed BLAKE3 — see `HashFile::recovers_object_keys`). Computed
    // once; passed down so per-entry symlink handling can recover a checksum
    // from the object path instead of reading the content. When `false` (the
    // default — empty hint) discovery is byte-identical to before.
    let recover_keys = !options.object_store_roots.is_empty() && hasher.recovers_object_keys();
    discover_dir(
        root,
        &root_str,
        root_permissions,
        options,
        recover_keys,
        hasher,
        &mut dirs,
        &mut pending,
        &mut guard_sink,
        meter,
    )?;

    // Discovery is complete: the pending vec is the real FILE count, so set it
    // as the meter's total and flip to the Hashing phase. This gives the
    // renderer a determinate FILE-count % for the hash pass. Advisory only —
    // the manifest is assembled from `dirs`/`pending` independent of the meter.
    if let Some(meter) = meter {
        meter.set_total(pending.len() as u64);
        meter.set_phase(Phase::Hashing);
    }

    // Hash every discovered file in parallel, writing each `(checksum, size)`
    // result back into its FIXED slot. The result is order-independent, so the
    // manifest and snapshot id are byte-identical regardless of which worker
    // finishes first. Bounded by a scoped pool sized to the resolved job count.
    hash_pending(&pending, options.walk_jobs, hasher, meter, &mut dirs)?;

    // Compute each directory's checksum + member-size bottom-up. `dirs` is keyed
    // by path in a BTreeMap (lexicographic), so a child path always sorts after
    // its parent prefix; processing in reverse key order guarantees children are
    // finalized before their parents. We memoize finalized (checksum, size).
    let keys: Vec<String> = dirs.keys().cloned().collect();
    let mut finalized: BTreeMap<String, (String, u64)> = BTreeMap::new();
    for key in keys.iter().rev() {
        let record = &dirs[key];

        // Direct children's checksums (files + subdirs) for the merkle rule,
        // and their sizes for the member-size sum.
        let mut child_checksums: Vec<String> = Vec::new();
        let mut member_size: u64 = 0;
        for file in &record.files {
            child_checksums.push(file.checksum.clone());
            member_size += file.size;
        }
        for child in &record.child_dirs {
            // On a quiescent tree a child dir is always finalized before its
            // parent (reverse key order). A miss means the tree mutated
            // mid-walk (the child vanished after discovery): a clean typed
            // error instead of the former `expect()` panic/backtrace.
            let Some((csum, size)) = finalized.get(child) else {
                return Err(WalkError::TreeStructureChanged {
                    path: PathBuf::from(child),
                });
            };
            child_checksums.push(csum.clone());
            member_size += size;
        }

        let checksum =
            crate::merkle::directory_checksum(child_checksums.iter().map(String::as_str), hasher);
        finalized.insert(key.clone(), (checksum, member_size));
    }

    // Emit manifest entries. Files first, then their directory, in any order —
    // the Manifest sorts by path (`sort -k5`) on Display.
    let mut manifest = Manifest::new();
    for (key, record) in &dirs {
        let (checksum, size) = &finalized[key];
        let dir_path = render_dir_path(key, &root_str, options.path_mode);
        manifest.push(ManifestEntry::new(
            PathType::Directory,
            record.permissions.clone(),
            checksum.clone(),
            *size,
            dir_path,
        ));
        for file in &record.files {
            let file_path = rewrite_path(&file.abs_path, &root_str, options.path_mode);
            manifest.push(ManifestEntry::new(
                PathType::File,
                file.permissions.clone(),
                file.checksum.clone(),
                file.size,
                file_path,
            ));
        }
    }
    manifest.sort();
    Ok((manifest, guards))
}

/// Recursively discovers the directory at `abs_path` (already known to be a
/// directory), recording its direct files and child directories, then recurses
/// into each child directory.
#[allow(clippy::too_many_arguments)] // internal recursion carrying walk state + the discovery meter
#[allow(clippy::too_many_lines)] // one cohesive readdir loop: per-entry exclude/follow/type + fast-path
fn discover_dir<H: Hasher + HashFile + Sync>(
    dir: &Path,
    abs_path: &str,
    permissions: String,
    options: &WalkOptions,
    recover_keys: bool,
    hasher: &H,
    dirs: &mut BTreeMap<String, DirRecord>,
    pending: &mut Vec<PendingHash>,
    guards: &mut Option<&mut HashMap<PathBuf, CopyGuard>>,
    meter: Option<&Meter>,
) -> Result<(), WalkError> {
    // `permissions` is the directory's own `lstat` octal mode (a symlinked
    // directory keeps the symlink's perms, matching the oracle's non-following
    // `stat -f '%A'` / `stat -c '%a'`).
    let mut record = DirRecord {
        abs_path: abs_path.to_owned(),
        permissions,
        child_dirs: Vec::new(),
        files: Vec::new(),
    };

    let read_dir = std::fs::read_dir(dir).map_err(|e| WalkError::io(dir, e))?;
    for entry in read_dir {
        let entry = entry.map_err(|e| WalkError::io(dir, e))?;
        let entry_path = entry.path();
        let entry_abs = path_str(&entry_path)?.to_owned();

        // Excludes run on the absolute path (`grep -E -v` over `find` output),
        // before any relative rewrite. A matching path is dropped for both the
        // directory listing and the file listing.
        if let Some(matcher) = &options.exclude {
            if matcher.is_excluded(&entry_abs) {
                continue;
            }
        }

        // `symlink_metadata` does not traverse the final symlink, so we can
        // detect symlinks and honor the follow mode like plain `find` vs
        // `find -L`.
        let link_meta = entry
            .metadata()
            .or_else(|_| std::fs::symlink_metadata(&entry_path))
            .map_err(|e| WalkError::io(&entry_path, e))?;
        let is_symlink = link_meta.file_type().is_symlink();

        if is_symlink && !options.follow.follows_symlinks() {
            // Plain `find` lists a symlink as type `l`; it is neither a `-type d`
            // nor a `-type f`, so it never enters the manifest under no-follow.
            continue;
        }

        // Resolve the (possibly symlinked) target's metadata. Following symlinks
        // (`find -L`) makes a symlink-to-dir a directory and a symlink-to-file a
        // file, inheriting the target's type/perms/size/checksum.
        let target_meta = match std::fs::metadata(&entry_path) {
            Ok(m) => m,
            Err(e) => {
                // A broken symlink (or a symlink loop on some platforms) cannot
                // be stat'd through. `find -L` likewise cannot classify it as a
                // file or directory, so it is omitted. Surface real I/O errors
                // on non-symlink entries.
                if is_symlink && (e.kind() == io::ErrorKind::NotFound || is_loop_error(&e)) {
                    // LINKED-MODE FAST PATH: a dangling symlink whose target IS a
                    // recoverable object under a hinted store root is NOT a
                    // benign broken link to silently drop — the checkout's
                    // backing object was removed/GC'd. Surface a typed error
                    // (never a panic, never a silent drop). Otherwise a broken
                    // symlink is dropped exactly as before, matching `find -L`.
                    if let Some(err) =
                        dangling_linked_object(recover_keys, &e, dir, &entry_path, options)
                    {
                        return Err(err);
                    }
                    continue;
                }
                return Err(WalkError::io(&entry_path, e));
            }
        };
        let file_type = target_meta.file_type();

        // PERMISSIONS (and, for files, SIZE) come from the entry's own `lstat`,
        // because the oracle's `stat` is non-following: a symlinked entry keeps
        // the symlink's perms/size while its CHECKSUM is read through the link
        // (b3sum/md5sum/sha256sum all follow symlinks). For a real (non-symlink)
        // entry `lstat` == `stat`, so this is identical there.
        let own_permissions = octal_permissions(link_meta.permissions().mode());

        if file_type.is_dir() {
            record.child_dirs.push(entry_abs.clone());
            discover_dir(
                &entry_path,
                &entry_abs,
                own_permissions,
                options,
                recover_keys,
                hasher,
                dirs,
                pending,
                guards,
                meter,
            )?;
        } else if file_type.is_file() {
            // Record the file with an EMPTY checksum slot and queue a pending
            // hash job. The content is hashed (through the link) in the later
            // parallel pass; the result is written back into this exact slot,
            // identified by `(abs_path, file_index)`. SIZE comes from the
            // entry's own `lstat` (for a symlink the target-path length,
            // matching the oracle's `%z` / `%s` on the un-dereferenced symlink)
            // — NOT the dereferenced content length the hasher would report.
            // SYMLINK SAFETY: capture a `CopyGuard` ONLY for a plain regular
            // file whose hashed content path equals its own path — i.e. NOT a
            // followed symlink. For a real file `link_meta` (the entry's own
            // `lstat`) equals its `stat`, and `entry_path` is exactly the path
            // a store would later re-`stat` before cloning. For a followed
            // symlink-to-file the path the store re-stats (the symlink's own
            // `lstat`) is not the path whose bytes were hashed, so trusting it
            // would be unsound: we OMIT the guard and the store re-hashes (no
            // skip = always safe). On non-unix `from_metadata` returns `None`.
            if !is_symlink {
                if let Some(sink) = guards.as_deref_mut() {
                    if let Some(guard) = CopyGuard::from_metadata(&link_meta) {
                        sink.insert(entry_path.clone(), guard);
                    }
                }
            }

            // Advisory: count this file as discovered so the renderer can show
            // a live, growing count during the (otherwise silent) enumeration
            // pass. Output-orthogonal — never influences the manifest.
            if let Some(meter) = meter {
                meter.object_discovered();
            }

            // LINKED-MODE FAST PATH (dormant unless `recover_keys`): for a
            // symlink-to-file whose target is an object under a hinted store
            // root, RECOVER the content checksum from the object path instead of
            // reading + hashing the bytes. The recovered key is byte-identical
            // to the content hash on a healthy store, so the manifest/id is
            // unchanged from the read path — but no content is read. SIZE still
            // comes from the symlink's own `lstat` (checksum-only fast path), so
            // a linked re-snapshot does NOT reproduce the source id. Ineligible
            // symlinks (escaped / non-object target) fall through to the normal
            // followed-content hash below; a MISSING object (dangling) is a
            // typed error, never a panic or a silent drop.
            if recover_keys && is_symlink {
                // The target object is already known to exist here: `target_meta`
                // stat'd through the link successfully above, so this is NOT a
                // dangling link (the dangling case is caught earlier and turned
                // into `WalkError::DanglingLinkedObject`). `read_link` + pure
                // path parsing recover the key; no content is read.
                if let Some(key) = std::fs::read_link(&entry_path).ok().and_then(|t| {
                    crate::recover::recover_object_key(dir, &t, &options.object_store_roots)
                }) {
                    // STRICT-VERIFY override (`SNAPDIR_VERIFY_COPIES=1`): when
                    // `verify_linked_objects` is set, do NOT trust the address —
                    // re-hash the content and ERROR on a mismatch (corrupt store).
                    // On the default path (`false`) the recovered `key` is trusted
                    // and recorded WITHOUT reading the bytes. Either way the
                    // checksum-to-record is resolved here.
                    let checksum = if options.verify_linked_objects {
                        verify_linked_checksum(hasher, dir, &entry_path, key)?
                    } else {
                        key
                    };
                    // Record the (recovered or verified) checksum directly. No
                    // `PendingHash` is queued; SIZE is still the symlink's own
                    // `lstat` length (checksum-only fast path).
                    record.files.push(FileRecord {
                        abs_path: entry_abs,
                        permissions: own_permissions,
                        checksum,
                        size: link_meta.len(),
                    });
                    if let Some(meter) = meter {
                        meter.object_finished();
                    }
                    continue;
                }
            }

            let file_index = record.files.len();
            record.files.push(FileRecord {
                abs_path: entry_abs,
                permissions: own_permissions,
                checksum: String::new(),
                size: link_meta.len(),
            });
            pending.push(PendingHash {
                dir_key: abs_path.to_owned(),
                file_index,
                content_path: entry_path,
                recorded_size: link_meta.len(),
                is_symlink,
            });
        }
        // Anything else (sockets, fifos, devices) is neither `-type d` nor
        // `-type f`, so it is skipped — matching `find`.
    }

    dirs.insert(record.abs_path.clone(), record);
    Ok(())
}

/// Decides whether a broken symlink (`metadata` returned `NotFound`) is a
/// dangling LINKED object that must surface a typed
/// [`WalkError::DanglingLinkedObject`], rather than being silently dropped.
///
/// Returns `Some(err)` only when the linked-mode fast path is active
/// (`recover_keys`), the failure was `NotFound` (not a symlink loop), and the
/// symlink's target lexically resolves to a recoverable object under a hinted
/// store root. Otherwise `None` (the caller drops the broken link as `find -L`
/// does). Pure aside from one `read_link`; never reads content, never panics.
fn dangling_linked_object(
    recover_keys: bool,
    err: &io::Error,
    link_parent: &Path,
    entry_path: &Path,
    options: &WalkOptions,
) -> Option<WalkError> {
    if !recover_keys || err.kind() != io::ErrorKind::NotFound {
        return None;
    }
    let target = std::fs::read_link(entry_path).ok()?;
    crate::recover::recover_object_key(link_parent, &target, &options.object_store_roots)?;
    Some(WalkError::DanglingLinkedObject {
        path: entry_path.to_path_buf(),
        target,
    })
}

/// Strict-verify a fast-path-eligible linked object: re-hash its CONTENT and
/// confirm it matches the address (`expected`) recovered from the object path.
///
/// Only called for an entry that WOULD take the fast path (eligible symlink,
/// recoverable object key) when [`WalkOptions::verify_linked_objects`] is set —
/// the `SNAPDIR_VERIFY_COPIES=1` keystone. It reads + hashes the bytes the
/// default fast path deliberately skips, then:
/// - on a **match** returns the verified checksum (identical to `expected`), so
///   the caller records a checksum proven against the content;
/// - on a **mismatch** returns [`WalkError::LinkedObjectIntegrity`] naming the
///   symlink, its target object, the claimed address and the actual content
///   hash — the store filed bytes under the wrong address (corruption), and the
///   walk refuses to trust it.
///
/// A genuine read/IO fault on the content surfaces as [`WalkError::Io`].
fn verify_linked_checksum<H: HashFile>(
    hasher: &H,
    link_parent: &Path,
    entry_path: &Path,
    expected: String,
) -> Result<String, WalkError> {
    let (actual, _len) = hasher
        .hash_file_hex(entry_path)
        .map_err(|e| WalkError::io(entry_path, e))?;
    if actual == expected {
        return Ok(actual);
    }
    let target = std::fs::read_link(entry_path).map_or_else(
        |_| entry_path.to_path_buf(),
        |t| resolve_link_target(link_parent, &t),
    );
    Err(WalkError::LinkedObjectIntegrity {
        path: entry_path.to_path_buf(),
        target,
        expected,
        actual,
    })
}

/// Resolves a (possibly relative) symlink target against the link's parent
/// directory, for the human-facing `target` field of an integrity error.
fn resolve_link_target(link_parent: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_parent.join(target)
    }
}

/// Hashes every [`PendingHash`] in parallel inside a bounded, scoped
/// [`rayon::ThreadPool`] sized to the resolved [`WalkOptions::walk_jobs`] count,
/// writing each `(checksum, len)` back into its fixed `FileRecord` slot.
///
/// The write-back is keyed by the discovery identity (`dir_key` + `file_index`),
/// so the manifest is byte-identical regardless of completion order. Advisory
/// [`Meter`] updates (`add_in` + `object_finished`) happen once per file inside
/// the worker closure; the meter is atomic/`Sync`, so only interleaving changes.
///
/// # Errors
///
/// Returns the first [`WalkError::io`] if any file fails to hash; the *which*
/// error is unspecified but the walk deterministically fails.
fn hash_pending<H: Hasher + HashFile + Sync>(
    pending: &[PendingHash],
    walk_jobs: Option<usize>,
    hasher: &H,
    meter: Option<&Meter>,
    dirs: &mut BTreeMap<String, DirRecord>,
) -> Result<(), WalkError> {
    if pending.is_empty() {
        return Ok(());
    }

    let jobs = resolve_jobs(walk_jobs);
    // Oversubscription guard (perf, not correctness): when there are at least as
    // many pending files as worker threads, every thread already has a whole
    // file to hash — so hash each file SINGLE-THREADED (no intra-file BLAKE3
    // rayon fan-out) to keep the bounded pool from oversubscribing. When there
    // are fewer files than threads, let BLAKE3 use its rayon path so the spare
    // cores still accelerate a lone big file.
    let per_file_seq = pending.len() >= jobs;

    let hash_one = |item: &PendingHash| -> Result<(String, usize, String), WalkError> {
        let (checksum, hashed_bytes) = match if per_file_seq {
            hasher.hash_file_hex_seq(&item.content_path)
        } else {
            hasher.hash_file_hex(&item.content_path)
        } {
            Ok(ok) => ok,
            // A file enumerated in discovery is now gone: a transient tree-in-flux
            // race, not a durable IO fault.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(WalkError::FileVanishedDuringWalk {
                    path: item.content_path.clone(),
                });
            }
            // The SIGBUS guard caught a mid-hash truncation/shrink (mmap fault):
            // the recorded checksum would be incoherent.
            Err(e) if crate::sigbus::is_mmap_fault(&e) => {
                return Err(WalkError::FileChangedDuringWalk {
                    path: item.content_path.clone(),
                });
            }
            // Genuine permission / IO fault.
            Err(e) => return Err(WalkError::io(&item.content_path, e)),
        };

        // Size-drift guard: the bytes actually hashed must match the size
        // recorded at discovery, or the file grew/shrank mid-walk and the
        // (size, checksum) pair would be incoherent. SKIP for followed symlinks,
        // whose recorded SIZE is the symlink's own `lstat` length (deliberately
        // != the dereferenced content length the hasher reports).
        if !item.is_symlink && hashed_bytes != item.recorded_size {
            return Err(WalkError::FileChangedDuringWalk {
                path: item.content_path.clone(),
            });
        }

        if let Some(meter) = meter {
            meter.add_in(hashed_bytes);
            meter.object_finished();
        }
        Ok((item.dir_key.clone(), item.file_index, checksum))
    };

    // A single honest single-threaded path when only one job is requested:
    // hash in place with no pool at all.
    let results: Vec<(String, usize, String)> = if jobs == 1 {
        pending.iter().map(hash_one).collect::<Result<_, _>>()?
    } else {
        // Build a bounded, scoped pool sized to the resolved job count and run
        // the parallel hash inside `install`, so total threads are explicitly
        // capped (never the global pool).
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build()
            .map_err(|e| WalkError::io(PathBuf::from("<walk thread pool>"), io::Error::other(e)))?;
        pool.install(|| {
            pending
                .par_iter()
                .map(hash_one)
                .collect::<Result<Vec<_>, _>>()
        })?
    };

    // Write each result back into its fixed slot. Order-independent: the slot is
    // addressed by (dir_key, file_index), so completion order is irrelevant.
    for (dir_key, file_index, checksum) in results {
        // The owning dir was discovered before its files were queued; a miss
        // means the tree mutated mid-walk. Clean typed error, not an `expect()`
        // panic.
        let Some(record) = dirs.get_mut(&dir_key) else {
            return Err(WalkError::TreeStructureChanged {
                path: PathBuf::from(&dir_key),
            });
        };
        record.files[file_index].checksum = checksum;
    }
    Ok(())
}

/// Detects a symlink-loop I/O error (`ELOOP`) so the walk can skip it the way
/// `find -L` halts on / omits a self-referential symlink.
fn is_loop_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc_eloop())
}

/// `ELOOP` is 40 on Linux and 62 on macOS/BSD. We avoid a `libc` dependency by
/// matching on the message kind via the raw errno of both platforms.
const fn libc_eloop() -> i32 {
    #[cfg(target_os = "linux")]
    {
        40
    }
    #[cfg(not(target_os = "linux"))]
    {
        62
    }
}

/// Renders a directory's path for the manifest: always trailing-`/`, and either
/// absolute or rewritten to a leading `./` relative to `root`.
fn render_dir_path(abs_path: &str, root: &str, mode: PathMode) -> String {
    let rewritten = rewrite_path(abs_path, root, mode);
    // Directory paths always end with `/`. The root rewrites to "." -> "./";
    // a nested dir "./a" -> "./a/". Absolute "/abs/a" -> "/abs/a/".
    if rewritten.ends_with('/') {
        rewritten
    } else {
        format!("{rewritten}/")
    }
}

/// Applies the oracle's relative rewrite `sed -E "s| \.?${root_dir}| .|"`:
/// the leading `root` prefix of an absolute path becomes `.`. In absolute mode
/// the path is returned unchanged.
fn rewrite_path(abs_path: &str, root: &str, mode: PathMode) -> String {
    match mode {
        PathMode::Absolute => abs_path.to_owned(),
        PathMode::Relative => {
            if abs_path == root {
                // The root directory itself becomes ".".
                ".".to_owned()
            } else if let Some(rest) = abs_path.strip_prefix(root) {
                // rest starts with '/': "/a/aa/f1" -> "./a/aa/f1".
                format!(".{rest}")
            } else {
                // Defensive: not under root (should not happen). Leave as-is.
                abs_path.to_owned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Pure-Rust walk tests.
    //!
    //! Originally these shelled out to the legacy Bash oracle
    //! (the `snapdir-manifest` script) and asserted byte-identity. The oracle
    //! has since been deleted from the branch, so each case is now pinned
    //! against an
    //! **embedded golden manifest constant** (or, where a column is
    //! platform-dependent, a structural assertion). The golden bytes were
    //! captured once from this very `walk` implementation over fixtures with
    //! **explicit, fixed permissions** (dirs `0o700`/`0o755`, files `0o600`),
    //! which makes the `TYPE PERMS CHECKSUM SIZE PATH` output fully
    //! deterministic. The content/size/checksum/merkle columns were
    //! cross-checked against the recorded oracle vectors in
    //! `crates/snapdir-core/tests/compat_golden.rs` (e.g. the empty-file
    //! `af1349b9…` checksum and the `./a/aa/aaa/` merkle `8aed4caf…`).
    //!
    //! Symlink rows (`./a_link/`, `./r1f_link`) carry the symlink's *own* lstat
    //! permissions, which differ across platforms (macOS reports `755`, Linux
    //! `777`), so those tests assert structure (presence/absence + materialized
    //! subtree) rather than a byte-exact perm column.
    use super::*;
    use crate::merkle::Blake3Hasher;
    use crate::progress::{Meter, Phase};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A self-cleaning scratch directory under the system temp dir. Avoids a
    /// `tempfile` dev-dependency; the walk is library-pure and never reads the
    /// environment itself — only this test harness builds fixtures on disk. The
    /// root is chmod'd to a fixed `0o755` so the root `D` line's perm column is
    /// deterministic across umasks.
    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            // Resolve through canonicalize so macOS's /var -> /private/var (and
            // any other symlinked temp prefix) is already normalized.
            let base = std::env::temp_dir()
                .canonicalize()
                .expect("temp dir canonicalizes");
            let path = base.join(format!("snapdir-walk-test-{tag}-{pid}-{n}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create scratch dir");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("chmod scratch root");
            Scratch { path }
        }

        fn root(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Writes a file (creating parents) with a fixed `0o600` mode so the `F`
    /// line's perm column is deterministic.
    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, contents).expect("write file");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod file");
    }

    /// Recursively chmods `root` and every descendant directory to `mode`, so
    /// every `D` line's perm column is pinned (independent of the process umask).
    fn chmod_dirs(root: &Path, mode: u32) {
        fs::set_permissions(root, fs::Permissions::from_mode(mode)).expect("chmod dir");
        for entry in fs::read_dir(root).expect("read_dir").flatten() {
            let ft = entry.file_type().expect("file_type");
            // `is_dir()` here is lstat-based via DirEntry::file_type, so a
            // symlink-to-dir is NOT recursed into (its own perms stay as-is).
            if ft.is_dir() {
                chmod_dirs(&entry.path(), mode);
            }
        }
    }

    /// Builds a [`WalkOptions`] for the given follow/path/exclude combination.
    fn opts(follow: FollowMode, path_mode: PathMode, exclude: Option<&str>) -> WalkOptions {
        WalkOptions {
            follow,
            path_mode,
            exclude: exclude.map(|p| ExcludeMatcher::new(p).expect("valid exclude regex")),
            ..WalkOptions::default()
        }
    }

    /// Runs the walk and returns its `Display` manifest text (no trailing
    /// newline — `Manifest`'s `Display` does not emit one).
    fn manifest_text(root: &Path, options: &WalkOptions) -> String {
        walk(root, options, &Blake3Hasher::new())
            .expect("walk")
            .to_string()
    }

    // -- Empty-string / empty-file checksum reused from the oracle vectors -----
    // (matches compat_golden.rs::EMPTY_FILE_B3).
    const EMPTY_B3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    #[test]
    fn walk_root_must_be_absolute() {
        let err = walk(
            Path::new("relative/path"),
            &WalkOptions::default(),
            &Blake3Hasher::new(),
        )
        .unwrap_err();
        assert!(matches!(err, WalkError::RootNotAbsolute(_)));
    }

    #[test]
    fn walk_empty_directory_golden() {
        // An empty directory: a single `D` line whose checksum is the merkle of
        // zero children == blake3("") and whose size is 0. Root chmod'd to 755.
        let scratch = Scratch::new("empty-dir");
        let expected = format!("D 755 {EMPTY_B3} 0 ./");
        assert_eq!(
            manifest_text(scratch.root(), &WalkOptions::default()),
            expected
        );
    }

    #[test]
    fn walk_single_empty_file_golden() {
        // Root `D` line (its merkle == single empty-file child) plus the `F`
        // line for the empty file. Both content checksums are blake3("").
        let scratch = Scratch::new("empty-file");
        write_file(&scratch.root().join("empty.txt"), b"");
        let expected = format!(
            "D 755 dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b 0 ./\n\
             F 600 {EMPTY_B3} 0 ./empty.txt"
        );
        assert_eq!(
            manifest_text(scratch.root(), &WalkOptions::default()),
            expected
        );
    }

    /// The deep guide tree under [`PathMode::Relative`]. Dirs are `0o700`, files
    /// `0o600`; every checksum/merkle value matches the recorded oracle vectors
    /// (cf. `compat_golden.rs::MULTILEVEL_MANIFEST` — same `./a/…`/`./b/…`/`./c/…`
    /// subtree). The extra empty `./d/` dir carries the blake3("") merkle.
    const NESTED_RELATIVE_GOLDEN: &str = "\
D 700 3f938f681dcbd616d00d42f704d525c05e7ed2746888c35c8214127c632587c3 43 ./
D 700 ed23cfd2037d23cf8c6b67497425e7a06d5e40ea2bd8e43fc434006022dafe86 21 ./a/
F 600 3c9cb8b8c8f3588f8e59e18d284330b0a951be644fbef2b9784b56e15d1c6096 4 ./a/a1f
D 700 ee795476bff6c1816b4c7558a74ee0b44ec600c3cde6b02564508f67d536a656 17 ./a/aa/
F 600 a2951028421deef48d1ba185f4c497c2d986f1dd76079baf2f5eb8479f132b5a 5 ./a/aa/aa1f
D 700 8aed4caf45b22aa4c8a195945136e3a01f77864e91fabe2d9272feeee87ae334 12 ./a/aa/aaa/
F 600 5cfee4fb4074748633b4ccbddb6b184a9b5e2f5ce74df6d2803f5fea0392a197 6 ./a/aa/aaa/aaa1f
F 600 3791f11a017feedffd24c2656e18d5c4ca9d6c404c8f40ccc511b6351c8575a6 6 ./a/aa/aaa/aaa2f
D 700 9a8b0e35c000df69893648b91d15cc30ab88ae5a40af48228caf5fa443dafc9b 12 ./b/
D 700 d41c2090167e6f546a510f0da98d8a8355d6bd2b61666644604c73b3a8f5b5d9 12 ./b/bb/
D 700 3b9023fa454aa22466feeb8cbf55a2c764dd79de0e93c9a793e8b54caec227da 12 ./b/bb/bbb/
F 600 8d18b7f3aabbef192a524fa2549d1d36b48c9030d234c9bdf87caa267fb09933 6 ./b/bb/bbb/bbb1f
F 600 2e16e172b6e337325f271d4eae00bc1ea20e41609ef78665710cada1477005cc 6 ./b/bb/bbb/bbb2f
D 700 15eb2657c1e6f5a24023c10429bb6f1b7d81b2cc2057eedee2192fbf3e7b892c 6 ./c/
D 700 e711f4e76ae9b3e25ad9a32b5f115cc9a81e55a428c552aa0bcab8543967f51a 6 ./c/cc/
D 700 31a1955d5a65328f31014650cf79b5c0c3d9b82de19352ade8d299cc22f6ec40 6 ./c/cc/ccc/
F 600 24f0cf3553e0dac0ce8aead4279e0fc368899e89ef776999d0d7e812b5ca0f3b 6 ./c/cc/ccc/ccc1f
D 700 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./d/
F 600 27a55588c59999fd686667c4b186af08161b95c287216f0cde723f0e191d1974 4 ./r1f";

    fn build_nested(root: &Path) {
        write_file(&root.join("a/aa/aaa/aaa1f"), b"aaa1f\n");
        write_file(&root.join("a/aa/aaa/aaa2f"), b"aaa2f\n");
        write_file(&root.join("a/aa/aa1f"), b"aa1f\n");
        write_file(&root.join("a/a1f"), b"a1f\n");
        write_file(&root.join("r1f"), b"r1f\n");
        write_file(&root.join("b/bb/bbb/bbb1f"), b"bbb1f\n");
        write_file(&root.join("b/bb/bbb/bbb2f"), b"bbb2f\n");
        write_file(&root.join("c/cc/ccc/ccc1f"), b"ccc1f\n");
        // Empty subdirectory with no files.
        fs::create_dir_all(root.join("d")).unwrap();
        chmod_dirs(root, 0o700);
    }

    #[test]
    fn walk_nested_tree_relative_golden() {
        let scratch = Scratch::new("nested-rel");
        build_nested(scratch.root());
        assert_eq!(
            manifest_text(
                scratch.root(),
                &opts(FollowMode::Follow, PathMode::Relative, None)
            ),
            NESTED_RELATIVE_GOLDEN
        );
    }

    #[test]
    fn walk_nested_tree_absolute_golden() {
        // Under PathMode::Absolute every PATH column is the scratch root prefix
        // + the relative tail; the TYPE/PERMS/CHECKSUM/SIZE columns are
        // identical to the relative golden. We reconstruct the expected text by
        // rewriting the relative golden's `./` prefix to the absolute root,
        // proving the only difference is the path rendering.
        let scratch = Scratch::new("nested-abs");
        let r = scratch.root();
        build_nested(r);
        let root_str = r.to_str().unwrap();
        let expected: String = NESTED_RELATIVE_GOLDEN
            .lines()
            .map(|line| {
                // Replace the leading "./" of the PATH (last field) with the
                // absolute root. The path is everything after the 4th space.
                let (head, path) = line.rsplit_once(' ').unwrap();
                let abs_path = if path == "./" {
                    format!("{root_str}/")
                } else {
                    format!("{root_str}/{}", path.strip_prefix("./").unwrap())
                };
                format!("{head} {abs_path}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            manifest_text(r, &opts(FollowMode::Follow, PathMode::Absolute, None)),
            expected
        );
    }

    #[test]
    fn walk_directory_size_is_sum_of_members_golden() {
        // Cross-check dir-size summation: each `D` line's SIZE is the sum of its
        // members (recursively), independent of the directory's own stat size.
        let scratch = Scratch::new("dir-size");
        let r = scratch.root();
        write_file(&r.join("f1"), b"hello"); // 5
        write_file(&r.join("sub/f2"), b"world!!"); // 7
        write_file(&r.join("sub/f3"), b"x"); // 1
        chmod_dirs(r, 0o700);

        let expected = "\
D 700 5681c72cfd0ddea4f54683365bc4082b92147bf33976875653133cc4aed0f96a 13 ./
F 600 ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f 5 ./f1
D 700 2ac73ec4f4ec2ef21ebfba467be499a58aef80a34d7001d68bdeb14cb58a954d 8 ./sub/
F 600 8bafa24d36bc2aa6edc0d041e763cb59ebadb71b6e63ab4ac9314de95e9a0de7 7 ./sub/f2
F 600 3ae7d805f6789a6402acb70ad4096a85a56bf6804eaf25c0493ac697548d30b5 1 ./sub/f3";
        let manifest = walk(r, &WalkOptions::default(), &Blake3Hasher::new()).expect("walk");
        assert_eq!(manifest.to_string(), expected);

        // Structural cross-check of the summation rule independent of the bytes.
        let root_dir = manifest.entries().iter().find(|e| e.path == "./").unwrap();
        let sub_dir = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./sub/")
            .unwrap();
        assert_eq!(sub_dir.size, 8, "sub = f2(7) + f3(1)");
        assert_eq!(root_dir.size, 13, "root = f1(5) + sub(8)");
    }

    /// Builds the symlink fixture: a real `a/` subtree plus a dir-symlink
    /// `a_link -> a` and a file-symlink `r1f_link -> r1f`. Real dirs chmod'd to
    /// `0o700`; files `0o600`. The symlinks' own perms are left platform-default
    /// (NOT chmod'd) — hence the structural (not byte-golden) assertions below.
    fn build_symlinks(root: &Path) {
        write_file(&root.join("a/aa/f1"), b"hello");
        write_file(&root.join("a/f2"), b"world!!");
        write_file(&root.join("r1f"), b"r");
        std::os::unix::fs::symlink("a", root.join("a_link")).expect("symlink dir");
        std::os::unix::fs::symlink("r1f", root.join("r1f_link")).expect("symlink file");
        chmod_dirs(root, 0o700);
    }

    #[test]
    fn walk_symlink_followed_by_default() {
        let scratch = Scratch::new("symlink-follow");
        let r = scratch.root();
        build_symlinks(r);

        let manifest = manifest_text(r, &opts(FollowMode::Follow, PathMode::Relative, None));

        // The dir symlink is followed: it materializes as its own `D ./a_link/`
        // row whose CHECKSUM equals the real `./a/` directory's merkle, plus the
        // full target subtree mirrored under ./a_link/.
        let a_dir_b3 = "0c862ed8e62262f84e7fc0fe4a6c566adec4a85ef22f8a46b7ad4c9344146701";
        assert!(
            manifest
                .lines()
                .any(|l| l.starts_with("D ") && l.contains(a_dir_b3) && l.ends_with(" ./a/")),
            "real ./a/ dir present with its merkle: {manifest}"
        );
        assert!(
            manifest
                .lines()
                .any(|l| l.starts_with("D ") && l.contains(a_dir_b3) && l.ends_with(" ./a_link/")),
            "followed symlink dir ./a_link/ mirrors ./a/'s merkle: {manifest}"
        );
        // Mirrored target subtree entries (content checksums are deterministic).
        assert!(manifest.lines().any(|l| l.ends_with(" ./a_link/aa/")));
        assert!(manifest.lines().any(|l| {
            l.starts_with("F ")
                && l.contains("ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f")
                && l.ends_with(" ./a_link/aa/f1")
        }));
        // The file symlink is followed: it appears as an `F` row pointing at the
        // target's content (blake3("r")), ending in ./r1f_link.
        let r1f_b3 = "b2dea48d667b2821a9bcf69eded39a2458a1d8165ca7fcac64c3557b69a7ea08";
        assert!(
            manifest
                .lines()
                .any(|l| l.starts_with("F ") && l.contains(r1f_b3) && l.ends_with(" ./r1f_link")),
            "followed symlink file ./r1f_link present: {manifest}"
        );
        assert!(
            manifest
                .lines()
                .any(|l| l.starts_with("F ") && l.contains(r1f_b3) && l.ends_with(" ./r1f")),
            "real ./r1f present: {manifest}"
        );
    }

    #[test]
    fn walk_no_follow_drops_symlinks() {
        let scratch = Scratch::new("symlink-nofollow");
        let r = scratch.root();
        build_symlinks(r);

        // With --no-follow the symlinks are dropped entirely; the manifest is a
        // byte-exact golden over only the real entries (no `_link` rows). Note
        // the root `D` SIZE is 13 (= sum of real members), not the 28 of the
        // followed case (which double-counts via a_link/).
        let expected = "\
D 700 61a8f1898844a17eeed84d34c2e3b5fd9c7fef136dba5f7036ae70294595a085 13 ./
D 700 0c862ed8e62262f84e7fc0fe4a6c566adec4a85ef22f8a46b7ad4c9344146701 12 ./a/
D 700 6cd17c61c7e42c50586ee5f3f54dbc4f809f71073fc176ed2ae865103dd33625 5 ./a/aa/
F 600 ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f 5 ./a/aa/f1
F 600 8bafa24d36bc2aa6edc0d041e763cb59ebadb71b6e63ab4ac9314de95e9a0de7 7 ./a/f2
F 600 b2dea48d667b2821a9bcf69eded39a2458a1d8165ca7fcac64c3557b69a7ea08 1 ./r1f";
        let manifest = manifest_text(r, &opts(FollowMode::NoFollow, PathMode::Relative, None));
        assert_eq!(manifest, expected);
        assert!(!manifest.contains("_link"), "no-follow drops all symlinks");
    }

    #[test]
    fn walk_exclude_regex_golden() {
        let scratch = Scratch::new("exclude-regex");
        let r = scratch.root();
        write_file(&r.join("keep/k"), b"x");
        write_file(&r.join("drop/d"), b"y");
        write_file(&r.join("top.txt"), b"top");
        chmod_dirs(r, 0o700);

        // The matcher runs against the ABSOLUTE find path, so the exclude is
        // anchored at the absolute root + "/drop". `drop/` is dropped entirely;
        // `keep/` and `top.txt` remain (byte-exact golden over the survivors).
        let abs = r.to_str().unwrap();
        let pattern = format!("{abs}/drop");
        let manifest = manifest_text(
            r,
            &opts(FollowMode::Follow, PathMode::Relative, Some(&pattern)),
        );
        let expected = "\
D 700 b6f1055a5f14fdd55fa831ff6d2e2f433c7ca7fa2cc43e63a8cd0a4542d3010a 4 ./
D 700 b9030f201b43e2a72e62951476c0bcfafe3b020ece221d2254d8610ea9e88fb5 1 ./keep/
F 600 3ae7d805f6789a6402acb70ad4096a85a56bf6804eaf25c0493ac697548d30b5 1 ./keep/k
F 600 ef854702aa94ba4f60c67d731671c9e0e49a031be6ce475489e91f7a33cb5243 3 ./top.txt";
        assert_eq!(manifest, expected);
        assert!(!manifest.contains("drop"), "drop/ excluded");
    }

    #[test]
    fn walk_exclude_common_golden() {
        let scratch = Scratch::new("exclude-common");
        let r = scratch.root();
        write_file(&r.join("src/main.rs"), b"fn main() {}\n");
        write_file(&r.join(".git/objects/secret"), b"secret");
        write_file(&r.join("node_modules/pkg/index.js"), b"//js\n");
        chmod_dirs(r, 0o700);

        // %common% expands to the regex that drops .git, node_modules, etc.
        // (the CLI lane uses the same expansion; core never reads the env).
        let expanded = crate::excludes::expand_excludes(
            "%common%",
            "/nonexistent/.cache/",
            "/nonexistent/cache",
        );
        let pattern = expanded.pattern.expect("non-empty");
        let manifest = manifest_text(
            r,
            &opts(FollowMode::Follow, PathMode::Relative, Some(&pattern)),
        );
        // Only ./src survives — byte-exact golden over the survivors.
        let expected = "\
D 700 ad5409ad5f97a26c908382b379b23971ee143e6bcd29a7d663175936d2cd4e94 13 ./
D 700 069cd5e102d7dd39faa7093b5b2d784c32e19b01f829a902c14aa10b7182debc 13 ./src/
F 600 2d1ebfa706ba230165250f744796a92accba5e1b6fa357983b65319da33f8e93 13 ./src/main.rs";
        assert_eq!(manifest, expected);
        assert!(!manifest.contains(".git"), "%common% excludes .git");
        assert!(
            !manifest.contains("node_modules"),
            "%common% excludes node_modules"
        );
    }

    #[test]
    fn progress_meter_walk_records_files_and_bytes() {
        // A small tree with known file sizes; the meter records the total bytes
        // hashed and one finished object per file.
        let scratch = Scratch::new("meter-records");
        let r = scratch.root();
        write_file(&r.join("f1"), b"hello"); // 5
        write_file(&r.join("sub/f2"), b"world!!"); // 7
        write_file(&r.join("sub/f3"), b"x"); // 1
        chmod_dirs(r, 0o700);

        let meter = Meter::new();
        let _ = walk_with_meter(
            r,
            &WalkOptions::default(),
            &Blake3Hasher::new(),
            Some(&meter),
        )
        .expect("walk");

        let snap = meter.snapshot();
        assert_eq!(snap.bytes_in, 5 + 7 + 1, "sum of file byte lengths");
        assert_eq!(snap.objects_done, 3, "one finished object per file");
        assert_eq!(snap.objects_discovered, 3, "one discovered per file");
        assert_eq!(
            snap.objects_total, 3,
            "total set to the discovered file count before hashing"
        );
        assert_eq!(snap.in_flight, 0, "no object left in flight");
        assert_eq!(
            snap.phase,
            Phase::Hashing,
            "walk ends in the Hashing phase after discovery"
        );
    }

    #[test]
    fn progress_meter_walk_output_unchanged() {
        // Recording into a meter must not change the manifest: walk(None) and
        // walk_with_meter(Some) over the same tree are byte-identical.
        let scratch = Scratch::new("meter-unchanged");
        let r = scratch.root();
        build_nested(r);
        let opts = opts(FollowMode::Follow, PathMode::Relative, None);

        let without = walk(r, &opts, &Blake3Hasher::new()).expect("walk");
        let meter = Meter::new();
        let with = walk_with_meter(r, &opts, &Blake3Hasher::new(), Some(&meter)).expect("walk");

        assert_eq!(
            without.to_string(),
            with.to_string(),
            "meter recording must not change the manifest"
        );
        // And it really did record (sanity: nine files in build_nested).
        assert_eq!(meter.snapshot().objects_done, 8);
    }

    #[test]
    fn walk_snapshot_id_is_blake3_of_manifest_text() {
        // The snapshot id is BLAKE3 of the manifest text + a trailing newline
        // (comment lines stripped). Cross-check the public derivation against an
        // explicit recomputation over the walk's own output.
        let scratch = Scratch::new("snapshot-id");
        let r = scratch.root();
        write_file(&r.join("a/f1"), b"hello\n");
        write_file(&r.join("b/f2"), b"world\n");
        chmod_dirs(r, 0o700);
        let hasher = Blake3Hasher::new();
        let manifest = walk(r, &WalkOptions::default(), &hasher).expect("walk");
        let id = crate::merkle::snapshot_id(&manifest, &hasher);

        let mut bytes = manifest.to_string().into_bytes();
        bytes.push(b'\n');
        let expected = hasher.hash_hex(&bytes);
        assert_eq!(
            id, expected,
            "snapshot id == blake3(manifest_text + \"\\n\")"
        );
        assert_eq!(id.len(), 64, "id is 64 lowercase hex chars");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
