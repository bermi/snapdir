//! In-process filesystem walk producing a frozen-format [`Manifest`].
//!
//! This module reproduces `./snapdir-manifest`'s `generate` behavior in pure
//! Rust, consuming the frozen [`manifest`](crate::manifest),
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

use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::excludes::{ExcludeMatcher, FollowMode};
use crate::manifest::{Manifest, ManifestEntry, PathType};
use crate::merkle::Hasher;

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

/// A discovered file entry, before path rewriting.
struct FileRecord {
    /// Absolute path of the file.
    abs_path: String,
    permissions: String,
    checksum: String,
    size: u64,
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
/// matches `./snapdir-manifest`'s output byte-for-byte for the same tree and
/// checksum function.
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
pub fn walk<H: Hasher>(
    root: &Path,
    options: &WalkOptions,
    hasher: &H,
) -> Result<Manifest, WalkError> {
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
    // directory checksums bottom-up.
    let mut dirs: BTreeMap<String, DirRecord> = BTreeMap::new();
    discover_dir(
        root,
        &root_str,
        root_permissions,
        options,
        hasher,
        &mut dirs,
    )?;

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
            let (csum, size) = finalized
                .get(child)
                .expect("child dir finalized before parent (reverse key order)");
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
    Ok(manifest)
}

/// Recursively discovers the directory at `abs_path` (already known to be a
/// directory), recording its direct files and child directories, then recurses
/// into each child directory.
fn discover_dir<H: Hasher>(
    dir: &Path,
    abs_path: &str,
    permissions: String,
    options: &WalkOptions,
    hasher: &H,
    dirs: &mut BTreeMap<String, DirRecord>,
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
                hasher,
                dirs,
            )?;
        } else if file_type.is_file() {
            // Read content through the link for the checksum; take SIZE from the
            // entry's own `lstat` (for a symlink that is the target-path length,
            // matching the oracle's `%z` / `%s` on the un-dereferenced symlink).
            let bytes = std::fs::read(&entry_path).map_err(|e| WalkError::io(&entry_path, e))?;
            let checksum = hasher.hash_hex(&bytes);
            record.files.push(FileRecord {
                abs_path: entry_abs,
                permissions: own_permissions,
                checksum,
                size: link_meta.len(),
            });
        }
        // Anything else (sockets, fifos, devices) is neither `-type d` nor
        // `-type f`, so it is skipped — matching `find`.
    }

    dirs.insert(record.abs_path.clone(), record);
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
    use super::*;
    use crate::merkle::{Blake3Hasher, Md5Hasher, Sha256Hasher};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Absolute path to the frozen Bash oracle (`./snapdir-manifest`), located
    /// relative to this crate's manifest dir (READ ONLY — never modified).
    fn oracle_bin() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../snapdir-manifest")
            .canonicalize()
            .expect("oracle binary exists at repo root")
    }

    /// Returns the name of an available checksum binary for the oracle, or
    /// `None` if it is not installed (so a test can skip rather than fail on a
    /// CI box lacking, e.g., `md5sum`).
    fn checksum_bin_available(name: &str) -> bool {
        Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// A self-cleaning scratch directory under the system temp dir. Avoids a
    /// `tempfile` dev-dependency; the walk is library-pure and never reads the
    /// environment itself — only this test harness builds fixtures on disk.
    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            // Resolve through canonicalize so macOS's /var -> /private/var (and
            // any other symlinked temp prefix) matches the oracle's readlink.
            let base = std::env::temp_dir()
                .canonicalize()
                .expect("temp dir canonicalizes");
            let path = base.join(format!("snapdir-walk-test-{tag}-{pid}-{n}"));
            fs::create_dir_all(&path).expect("create scratch dir");
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

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, contents).expect("write file");
    }

    /// Runs the oracle on `root` with the given checksum bin + extra flags,
    /// returning its stdout manifest text (trailing newline trimmed).
    fn run_oracle(root: &Path, checksum_bin: &str, extra: &[&str]) -> String {
        let mut cmd = Command::new("bash");
        cmd.arg(oracle_bin());
        // Path-first, then flags: the oracle's --no-follow only works when the
        // PATH precedes the flag (a harness-confirmed oracle quirk).
        cmd.arg(root);
        cmd.arg("--checksum-bin").arg(checksum_bin);
        for f in extra {
            cmd.arg(f);
        }
        let out = cmd.output().expect("run oracle");
        assert!(
            out.status.success(),
            "oracle failed: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("oracle output is utf-8")
            .trim_end_matches('\n')
            .to_owned()
    }

    /// Builds a [`WalkOptions`] for the given follow/path/exclude combination.
    fn opts(follow: FollowMode, path_mode: PathMode, exclude: Option<&str>) -> WalkOptions {
        WalkOptions {
            follow,
            path_mode,
            exclude: exclude.map(|p| ExcludeMatcher::new(p).expect("valid exclude regex")),
        }
    }

    /// Asserts the Rust walk reproduces the oracle's manifest byte-for-byte for
    /// every available checksum binary. Returns the BLAKE3 manifest text for
    /// further assertions (e.g. snapshot id).
    fn assert_matches_oracle(root: &Path, options: &WalkOptions, oracle_extra: &[&str]) -> String {
        let b3 = Blake3Hasher::new();
        let md5 = Md5Hasher::new();
        let sha = Sha256Hasher::new();

        // b3sum is the shipped default and required for these tests.
        assert!(
            checksum_bin_available("b3sum"),
            "b3sum must be installed to run walk interop tests"
        );
        let oracle_b3 = run_oracle(root, "b3sum", oracle_extra);
        let rust_b3 = walk(root, options, &b3).expect("walk b3").to_string();
        assert_eq!(rust_b3, oracle_b3, "BLAKE3 manifest mismatch vs oracle");
        let blake3_text = rust_b3;

        if checksum_bin_available("md5sum") {
            let oracle_md5 = run_oracle(root, "md5sum", oracle_extra);
            let rust_md5 = walk(root, options, &md5).expect("walk md5").to_string();
            assert_eq!(rust_md5, oracle_md5, "MD5 manifest mismatch vs oracle");
        }
        if checksum_bin_available("sha256sum") {
            let oracle_sha = run_oracle(root, "sha256sum", oracle_extra);
            let rust_sha = walk(root, options, &sha).expect("walk sha256").to_string();
            assert_eq!(rust_sha, oracle_sha, "SHA-256 manifest mismatch vs oracle");
        }
        blake3_text
    }

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
    fn walk_empty_directory_matches_oracle() {
        let scratch = Scratch::new("empty-dir");
        // An empty directory: just the `D ./` line, checksum blake3(""). The
        // permission bits are environment-dependent (umask), so we assert the
        // empty-string checksum + zero size and rely on the byte-for-byte
        // oracle match inside `assert_matches_oracle` for the rest.
        let manifest = assert_matches_oracle(scratch.root(), &WalkOptions::default(), &[]);
        assert_eq!(manifest.lines().count(), 1, "only the root D line");
        assert!(
            manifest
                .contains("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./"),
            "empty dir checksum is blake3(\"\") with size 0: {manifest}"
        );
    }

    #[test]
    fn walk_single_empty_file_matches_oracle() {
        let scratch = Scratch::new("empty-file");
        write_file(&scratch.root().join("empty.txt"), b"");
        assert_matches_oracle(scratch.root(), &WalkOptions::default(), &[]);
    }

    #[test]
    fn walk_nested_tree_matches_oracle_relative_and_absolute() {
        let scratch = Scratch::new("nested");
        let r = scratch.root();
        write_file(&r.join("a/aa/aaa/aaa1f"), b"aaa1f\n");
        write_file(&r.join("a/aa/aaa/aaa2f"), b"aaa2f\n");
        write_file(&r.join("a/aa/aa1f"), b"aa1f\n");
        write_file(&r.join("a/a1f"), b"a1f\n");
        write_file(&r.join("r1f"), b"r1f\n");
        write_file(&r.join("b/bb/bbb/bbb1f"), b"bbb1f\n");
        write_file(&r.join("b/bb/bbb/bbb2f"), b"bbb2f\n");
        write_file(&r.join("c/cc/ccc/ccc1f"), b"ccc1f\n");
        // Empty subdirectory with no files.
        fs::create_dir_all(r.join("d")).unwrap();

        assert_matches_oracle(r, &opts(FollowMode::Follow, PathMode::Relative, None), &[]);
        assert_matches_oracle(
            r,
            &opts(FollowMode::Follow, PathMode::Absolute, None),
            &["--absolute"],
        );
    }

    #[test]
    fn walk_directory_size_is_sum_of_members() {
        // Cross-check the dir-size summation independently of the oracle text.
        let scratch = Scratch::new("dir-size");
        let r = scratch.root();
        write_file(&r.join("f1"), b"hello"); // 5
        write_file(&r.join("sub/f2"), b"world!!"); // 7
        write_file(&r.join("sub/f3"), b"x"); // 1
        let manifest = walk(r, &WalkOptions::default(), &Blake3Hasher::new()).expect("walk");
        let root_dir = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./")
            .expect("root dir entry");
        let sub_dir = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./sub/")
            .expect("sub dir entry");
        assert_eq!(sub_dir.size, 8, "sub = f2(7) + f3(1)");
        assert_eq!(root_dir.size, 13, "root = f1(5) + sub(8)");
        // And the whole thing still matches the oracle byte-for-byte.
        assert_matches_oracle(r, &WalkOptions::default(), &[]);
    }

    #[test]
    fn walk_symlink_followed_by_default_matches_oracle() {
        let scratch = Scratch::new("symlink-follow");
        let r = scratch.root();
        write_file(&r.join("a/aa/f1"), b"hello");
        write_file(&r.join("a/f2"), b"world!!");
        write_file(&r.join("r1f"), b"r");
        // Symlink to a directory: followed by default, appears as ./a_link/...
        std::os::unix::fs::symlink("a", r.join("a_link")).expect("symlink dir");
        // Symlink to a file: followed by default, appears as a file entry.
        std::os::unix::fs::symlink("r1f", r.join("r1f_link")).expect("symlink file");

        let manifest =
            assert_matches_oracle(r, &opts(FollowMode::Follow, PathMode::Relative, None), &[]);
        // The followed dir symlink must materialize the target's subtree.
        assert!(manifest.contains("\nD 755 "));
        assert!(
            manifest.lines().any(|l| l.ends_with(" ./a_link/")),
            "followed symlink dir must appear: {manifest}"
        );
        assert!(
            manifest.lines().any(|l| l.ends_with(" ./r1f_link")),
            "followed symlink file must appear"
        );
    }

    #[test]
    fn walk_no_follow_drops_symlinks_matches_oracle() {
        let scratch = Scratch::new("symlink-nofollow");
        let r = scratch.root();
        write_file(&r.join("a/aa/f1"), b"hello");
        write_file(&r.join("a/f2"), b"world!!");
        write_file(&r.join("r1f"), b"r");
        std::os::unix::fs::symlink("a", r.join("a_link")).expect("symlink dir");
        std::os::unix::fs::symlink("r1f", r.join("r1f_link")).expect("symlink file");

        let manifest = assert_matches_oracle(
            r,
            &opts(FollowMode::NoFollow, PathMode::Relative, None),
            &["--no-follow"],
        );
        assert!(
            !manifest.contains("_link"),
            "no-follow must drop all symlinks: {manifest}"
        );
    }

    #[test]
    fn walk_exclude_regex_matches_oracle() {
        let scratch = Scratch::new("exclude-regex");
        let r = scratch.root();
        write_file(&r.join("keep/k"), b"x");
        write_file(&r.join("drop/d"), b"y");
        write_file(&r.join("top.txt"), b"top");

        // The oracle matches the regex against the ABSOLUTE find path, so the
        // Rust matcher must too. Build the exclude with the absolute root.
        let abs = r.to_str().unwrap();
        let pattern = format!("{abs}/drop");
        let manifest = assert_matches_oracle(
            r,
            &opts(FollowMode::Follow, PathMode::Relative, Some(&pattern)),
            &[&format!("--exclude={pattern}")],
        );
        assert!(!manifest.contains("drop"), "drop/ excluded: {manifest}");
        assert!(manifest.contains("./keep/"), "keep/ retained");
    }

    #[test]
    fn walk_exclude_common_matches_oracle() {
        if !checksum_bin_available("b3sum") {
            return;
        }
        let scratch = Scratch::new("exclude-common");
        let r = scratch.root();
        write_file(&r.join("src/main.rs"), b"fn main() {}\n");
        write_file(&r.join(".git/objects/secret"), b"secret");
        write_file(&r.join("node_modules/pkg/index.js"), b"//js\n");

        // %common% expands to (/(...)($|/)) — match the oracle's expansion. We
        // reuse the same expansion the CLI lane would (no env reads in core).
        let expanded = crate::excludes::expand_excludes(
            "%common%",
            "/nonexistent/.cache/",
            "/nonexistent/cache",
        );
        let pattern = expanded.pattern.expect("non-empty");
        let manifest = assert_matches_oracle(
            r,
            &opts(FollowMode::Follow, PathMode::Relative, Some(&pattern)),
            &["--exclude=%common%"],
        );
        assert!(!manifest.contains(".git"), "%common% excludes .git");
        assert!(
            !manifest.contains("node_modules"),
            "%common% excludes node_modules"
        );
        assert!(manifest.contains("./src/"), "src retained");
    }

    #[test]
    fn walk_snapshot_id_matches_oracle_id_derivation() {
        // The walk feeds snapshot_id: BLAKE3 of the manifest text + trailing
        // newline. Cross-check against the oracle's manifest piped through the
        // same derivation (grep -v '^#' | b3sum --no-names).
        let scratch = Scratch::new("snapshot-id");
        let r = scratch.root();
        write_file(&r.join("a/f1"), b"hello\n");
        write_file(&r.join("b/f2"), b"world\n");
        let hasher = Blake3Hasher::new();
        let manifest = walk(r, &WalkOptions::default(), &hasher).expect("walk");
        let id = crate::merkle::snapshot_id(&manifest, &hasher);

        // Oracle id = b3sum --no-names of (manifest text + newline).
        let oracle_text = run_oracle(r, "b3sum", &[]);
        let mut bytes = oracle_text.into_bytes();
        bytes.push(b'\n');
        let oracle_id = hasher.hash_hex(&bytes);
        assert_eq!(id, oracle_id, "snapshot id must match oracle derivation");
    }
}
