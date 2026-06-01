//! XDG content-addressable cache with the `cache-id` integrity-check mechanism.
//!
//! A snapdir *cache* is just a local content-addressable store (the same
//! sharded layout as a `file://` store): objects live under
//! `<cache_dir>/.objects/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>` and manifests
//! under `<cache_dir>/.manifests/<id…>`. This module mirrors the cache-side
//! integrity machinery of the Bash oracle:
//!
//! - [`check_snapshot_integrity`] mirrors `_snapdir_check_integrity` (`snapdir`
//!   ~L1691): given a snapshot id and a cache directory, assert the manifest is
//!   present locally, then verify every **file** object referenced by the
//!   manifest hashes (BLAKE3) to the checksum it is filed under. This is the
//!   "verify a cached snapshot by its id" check at the heart of
//!   `checkout`/`verify`.
//! - [`verify_cache`] mirrors `verify-cache` (`snapdir` ~L1011): enumerate every
//!   object under `.objects/*/*/*/*`, recompute its hash, and compare the actual
//!   hash to the **expected** hash encoded by the object's own sharded path (the
//!   path *is* the content address). Collect mismatches; when `purge` is set,
//!   delete the corrupt objects.
//! - [`flush_cache`] mirrors `flush-cache` (`snapdir` ~L1061): empty the cache
//!   directory, idempotent on a missing dir.
//!
//! Per the library-purity principle this module performs no terminal I/O and
//! reads no `$HOME`/`XDG`/environment for behavior. The cache directory is a
//! parameter; the CLI lane resolves `${XDG_CACHE_HOME:-$HOME/.cache}/snapdir`.
//! Hashing is in-process via the [`Hasher`] abstraction (the shipped default is
//! BLAKE3); we never shell out to `b3sum`. The sharded path layout is reused
//! from [`crate::store`] (`object_path`/`manifest_path`); it is not
//! reimplemented here.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::manifest::{Manifest, PathType};
use crate::merkle::Hasher;
use crate::store::{manifest_path, object_path, OBJECTS_DIR};

/// Errors the cache integrity machinery can surface.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CacheError {
    /// The manifest for the requested snapshot id was not present in the cache.
    ///
    /// Mirrors the oracle's "Manifest not found locally. Did you forget to
    /// fetch …?" failure in `_snapdir_check_integrity`.
    #[error("manifest not found locally for {id}. Did you forget to fetch {id} from the store?")]
    ManifestNotFound {
        /// The snapshot id that was looked up.
        id: String,
    },

    /// A file object referenced by the manifest was missing from the cache.
    #[error("object not found in cache: {checksum}")]
    ObjectNotFound {
        /// The object checksum (content address) that was looked up.
        checksum: String,
    },

    /// A cached object's bytes did not hash to the address it is filed under —
    /// the object is corrupt or tampered.
    #[error("checksum mismatch for {expected}: cached bytes hash to {actual}")]
    Integrity {
        /// The checksum the object is filed under (its content address).
        expected: String,
        /// The checksum actually computed over the cached bytes.
        actual: String,
    },

    /// A manifest's text could not be parsed.
    #[error("failed to parse cached manifest: {0}")]
    Parse(#[from] crate::manifest::ParseError),

    /// An underlying filesystem failure.
    #[error("cache I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Loads a cached manifest by snapshot `id` from `cache_dir`.
///
/// Reads `<cache_dir>/.manifests/<id…>` (the sharded manifest path) and parses
/// it. This is the "manifest must be present locally" precondition of
/// [`check_snapshot_integrity`], exposed on its own for callers that have only
/// an id and a cache directory.
///
/// # Errors
///
/// - [`CacheError::ManifestNotFound`] if no manifest is filed under `id`,
///   matching the oracle's `test -f … || { echo "…did you forget to fetch…" }`.
/// - [`CacheError::Parse`] if the cached bytes are not a valid manifest.
/// - [`CacheError::Io`] on any other read failure.
pub fn load_cached_manifest(cache_dir: &Path, id: &str) -> Result<Manifest, CacheError> {
    let path = cache_dir.join(manifest_path(id));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CacheError::ManifestNotFound { id: id.to_owned() });
        }
        Err(err) => return Err(CacheError::Io(err)),
    };
    Ok(Manifest::parse(&text)?)
}

/// Verifies a cached snapshot by its id — mirrors `_snapdir_check_integrity`.
///
/// First asserts the manifest for `id` is present locally (loading it from
/// `<cache_dir>/.manifests/<id…>`), then, for every **file** entry of the
/// manifest (directory entries — whose path ends `/` — are excluded, exactly as
/// the oracle's `grep -v "/$"`), verifies that the cached object at its sharded
/// path hashes via `hasher` to the checksum it is filed under (column 3 of the
/// manifest line, i.e. the object's content address).
///
/// The oracle pipes `checksum  path` pairs into `b3sum --check`; this reproduces
/// that check in-process. The first corrupt or missing object short-circuits
/// with an error, matching `b3sum --check`'s non-zero exit.
///
/// # Errors
///
/// - [`CacheError::ManifestNotFound`] if the snapshot's manifest is absent.
/// - [`CacheError::ObjectNotFound`] if a referenced file object is missing.
/// - [`CacheError::Integrity`] if a cached object does not hash to its address.
/// - [`CacheError::Parse`] / [`CacheError::Io`] on read/parse failure.
pub fn check_snapshot_integrity(
    cache_dir: &Path,
    id: &str,
    hasher: &dyn Hasher,
) -> Result<(), CacheError> {
    let manifest = load_cached_manifest(cache_dir, id)?;
    check_manifest_integrity(cache_dir, &manifest, hasher)
}

/// Like [`check_snapshot_integrity`] but for an already-loaded [`Manifest`].
///
/// Skips the `.manifests/<id…>` lookup (the caller already holds the manifest)
/// and verifies every file object referenced by `manifest` against its content
/// address. Used internally by [`check_snapshot_integrity`]; exposed for callers
/// that fetched the manifest themselves.
///
/// # Errors
///
/// - [`CacheError::ObjectNotFound`] if a referenced file object is missing.
/// - [`CacheError::Integrity`] if a cached object does not hash to its address.
/// - [`CacheError::Io`] on a read failure.
pub fn check_manifest_integrity(
    cache_dir: &Path,
    manifest: &Manifest,
    hasher: &dyn Hasher,
) -> Result<(), CacheError> {
    for entry in manifest.entries() {
        // Directory lines are excluded from the object check (oracle:
        // `grep -v "/$"`). Directory `D` entries always have a trailing-slash
        // path; gate on the type, which is the structural truth behind that.
        if entry.path_type == PathType::Directory {
            continue;
        }
        let checksum = &entry.checksum;
        let object = cache_dir.join(object_path(checksum));
        let bytes = match std::fs::read(&object) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(CacheError::ObjectNotFound {
                    checksum: checksum.clone(),
                });
            }
            Err(err) => return Err(CacheError::Io(err)),
        };
        let actual = hasher.hash_hex(&bytes);
        if &actual != checksum {
            return Err(CacheError::Integrity {
                expected: checksum.clone(),
                actual,
            });
        }
    }
    Ok(())
}

/// Outcome of a whole-cache scan by [`verify_cache`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheReport {
    /// Number of objects scanned (every `.objects/*/*/*/*` entry).
    pub checked: usize,
    /// Content addresses (expected checksums) whose cached bytes did not hash
    /// back to the address — i.e. corrupt or tampered objects.
    pub corrupt: Vec<String>,
    /// Content addresses that were deleted because `purge` was set (a subset of
    /// `corrupt`; empty when `purge` is false).
    pub purged: Vec<String>,
}

impl CacheReport {
    /// Returns `true` when no corruption was detected (the oracle exits 0).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.corrupt.is_empty()
    }
}

/// Verifies every object in the cache — mirrors `snapdir verify-cache`.
///
/// Enumerates every object at `<cache_dir>/.objects/*/*/*/*`, recomputes its
/// hash via `hasher`, and compares it to the **expected** checksum encoded by
/// the object's own sharded path (the path is the content address). The
/// expected checksum is reconstructed exactly as the oracle does
/// (`sed 's| .*.objects/| |; s|/||g'`): concatenate the four path segments after
/// `.objects/` with the separators removed.
///
/// Returns a [`CacheReport`]: how many objects were checked, which were corrupt,
/// and — when `purge` is set — which were deleted. An absent or empty
/// `.objects` directory is a clean pass with zero checked, matching the oracle's
/// `test -d "${cache_dir}/.objects" || return 0`.
///
/// # Errors
///
/// - [`CacheError::Io`] on a directory-traversal or read failure (other than the
///   `.objects` directory simply being absent, which is a clean pass).
pub fn verify_cache(
    cache_dir: &Path,
    purge: bool,
    hasher: &dyn Hasher,
) -> Result<CacheReport, CacheError> {
    let objects_root = cache_dir.join(OBJECTS_DIR);
    if !objects_root.is_dir() {
        // Oracle: `test -d "${cache_dir}"/.objects || return 0`.
        return Ok(CacheReport::default());
    }

    let mut report = CacheReport::default();

    // The oracle globs exactly `.objects/*/*/*/*` — three intermediate shard
    // levels then the leaf file. Walk those four levels deterministically.
    for path in collect_objects(&objects_root)? {
        report.checked += 1;

        // Reconstruct the expected checksum from the path: the four components
        // below `.objects/` concatenated (oracle `sed` strips the separators).
        let Some(expected) = expected_checksum_from_path(&objects_root, &path) else {
            continue;
        };

        let bytes = std::fs::read(&path)?;
        let actual = hasher.hash_hex(&bytes);

        if actual != expected {
            report.corrupt.push(expected.clone());
            if purge {
                // Oracle: `rm "${cache_dir}/$(_snapdir_get_object_rel_path …)"`.
                std::fs::remove_file(&path)?;
                report.purged.push(expected);
            }
        }
    }

    // Deterministic order regardless of filesystem readdir order.
    report.corrupt.sort();
    report.purged.sort();
    Ok(report)
}

/// Collects every object at exactly `<objects_root>/*/*/*/*` (three shard levels
/// then the leaf), mirroring the oracle's `.objects/*/*/*/*` glob.
fn collect_objects(objects_root: &Path) -> Result<Vec<PathBuf>, CacheError> {
    let mut out = Vec::new();
    for l0 in read_subdirs(objects_root)? {
        for l1 in read_subdirs(&l0)? {
            for l2 in read_subdirs(&l1)? {
                for entry in std::fs::read_dir(&l2)? {
                    let path = entry?.path();
                    if path.is_file() {
                        out.push(path);
                    }
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Returns the immediate subdirectories of `dir`.
fn read_subdirs(dir: &Path) -> Result<Vec<PathBuf>, CacheError> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            out.push(path);
        }
    }
    Ok(out)
}

/// Reconstructs the content address (expected checksum) of an object from its
/// sharded path under `objects_root`, exactly as the oracle's
/// `sed 's| .*.objects/| |; s|/||g'` does: take the path components below
/// `.objects/` and concatenate them with the separators removed.
fn expected_checksum_from_path(objects_root: &Path, object: &Path) -> Option<String> {
    let rel = object.strip_prefix(objects_root).ok()?;
    let mut checksum = String::new();
    for component in rel.components() {
        checksum.push_str(component.as_os_str().to_str()?);
    }
    Some(checksum)
}

/// Empties the local cache — mirrors `snapdir flush-cache`.
///
/// Removes the cache directory's contents (objects and manifests). The oracle
/// does `rm -rf "${cache_dir}"`; this removes the directory's *contents* so the
/// directory itself (which the caller may have created) survives, while still
/// leaving the cache empty. Idempotent on a missing cache directory (a clean
/// no-op pass).
///
/// # Errors
///
/// - [`CacheError::Io`] on a removal failure other than the directory simply
///   being absent.
pub fn flush_cache(cache_dir: &Path) -> Result<(), CacheError> {
    match std::fs::read_dir(cache_dir) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
            }
            Ok(())
        }
        // A missing cache dir is already "empty" — idempotent no-op.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(CacheError::Io(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestEntry;
    use crate::merkle::Blake3Hasher;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A self-cleaning scratch directory under the system temp dir. Mirrors the
    /// helper in `walk.rs`, deliberately avoiding a `tempfile` dev-dependency:
    /// the cache module is library-pure and never reads the environment itself —
    /// only this test harness builds fixtures on disk.
    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let base = std::env::temp_dir();
            let path = base.join(format!("snapdir-cache-test-{pid}-{n}"));
            fs::create_dir_all(&path).expect("create scratch dir");
            Scratch { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Writes `bytes` to the cache as an object filed under its real BLAKE3
    /// address, returning that checksum.
    fn put_object(cache_dir: &Path, bytes: &[u8]) -> String {
        let checksum = Blake3Hasher.hash_hex(bytes);
        let path = cache_dir.join(object_path(&checksum));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        checksum
    }

    /// Writes a manifest to the cache filed under `id`, returning the manifest.
    fn put_manifest(cache_dir: &Path, id: &str, manifest: &Manifest) {
        let path = cache_dir.join(manifest_path(id));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{manifest}")).unwrap();
    }

    /// Builds a small clean cache: a root dir entry + two file objects, with a
    /// manifest filed under `id`. Returns `(id, file checksums)`.
    fn build_clean_cache(cache_dir: &Path) -> (String, String, String) {
        let foo = b"foo\n";
        let bar = b"bar\n";
        let foo_sum = put_object(cache_dir, foo);
        let bar_sum = put_object(cache_dir, bar);

        let mut manifest = Manifest::new();
        manifest.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            "rootsum",
            0,
            "./",
        ));
        manifest.push(ManifestEntry::new(
            PathType::File,
            "600",
            &foo_sum,
            foo.len() as u64,
            "./foo",
        ));
        manifest.push(ManifestEntry::new(
            PathType::File,
            "600",
            &bar_sum,
            bar.len() as u64,
            "./bar",
        ));

        let id = "cafef00dcafef00dcafef00dcafef00dcafef00dcafef00dcafef00dcafef00d".to_string();
        put_manifest(cache_dir, &id, &manifest);
        (id, foo_sum, bar_sum)
    }

    #[test]
    fn cache_clean_passes_integrity_and_verify() {
        let tmp = Scratch::new();
        let (id, _foo, _bar) = build_clean_cache(tmp.path());

        check_snapshot_integrity(tmp.path(), &id, &Blake3Hasher).expect("clean cache passes");

        let report = verify_cache(tmp.path(), false, &Blake3Hasher).unwrap();
        assert_eq!(report.checked, 2, "two objects scanned");
        assert!(report.is_clean(), "no corruption: {report:?}");
        assert!(report.purged.is_empty());
    }

    #[test]
    fn cache_tampered_object_detected_by_both_checks() {
        let tmp = Scratch::new();
        let (id, foo_sum, _bar) = build_clean_cache(tmp.path());

        // Tamper with one object's bytes in place (path/address unchanged).
        let foo_path = tmp.path().join(object_path(&foo_sum));
        fs::write(&foo_path, b"TAMPERED").unwrap();

        // check_snapshot_integrity: the file object no longer hashes to its
        // manifest checksum.
        match check_snapshot_integrity(tmp.path(), &id, &Blake3Hasher) {
            Err(CacheError::Integrity { expected, .. }) => assert_eq!(expected, foo_sum),
            other => panic!("expected Integrity error, got {other:?}"),
        }

        // verify_cache: the object's bytes no longer match its path-encoded
        // address.
        let report = verify_cache(tmp.path(), false, &Blake3Hasher).unwrap();
        assert_eq!(report.checked, 2);
        assert_eq!(report.corrupt, vec![foo_sum.clone()]);
        assert!(report.purged.is_empty(), "no purge without flag");
        assert!(!report.is_clean());
        // The corrupt object is still on disk (not purged).
        assert!(foo_path.exists());
    }

    #[test]
    fn cache_purge_removes_only_corrupt_object() {
        let tmp = Scratch::new();
        let (_id, foo_sum, bar_sum) = build_clean_cache(tmp.path());

        let foo_path = tmp.path().join(object_path(&foo_sum));
        let bar_path = tmp.path().join(object_path(&bar_sum));
        fs::write(&foo_path, b"TAMPERED").unwrap();

        let report = verify_cache(tmp.path(), true, &Blake3Hasher).unwrap();
        assert_eq!(report.checked, 2);
        assert_eq!(report.corrupt, vec![foo_sum.clone()]);
        assert_eq!(report.purged, vec![foo_sum]);
        assert!(!foo_path.exists(), "corrupt object purged");
        assert!(bar_path.exists(), "clean object kept");

        // A re-scan now sees only the surviving clean object and passes.
        let rescan = verify_cache(tmp.path(), false, &Blake3Hasher).unwrap();
        assert_eq!(rescan.checked, 1);
        assert!(rescan.is_clean());
    }

    #[test]
    fn cache_missing_manifest_yields_not_found() {
        let tmp = Scratch::new();
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        match check_snapshot_integrity(tmp.path(), id, &Blake3Hasher) {
            Err(CacheError::ManifestNotFound { id: got }) => assert_eq!(got, id),
            other => panic!("expected ManifestNotFound, got {other:?}"),
        }
    }

    #[test]
    fn cache_missing_object_yields_not_found() {
        let tmp = Scratch::new();
        let (id, foo_sum, _bar) = build_clean_cache(tmp.path());
        // Delete one referenced object but keep the manifest.
        fs::remove_file(tmp.path().join(object_path(&foo_sum))).unwrap();
        match check_snapshot_integrity(tmp.path(), &id, &Blake3Hasher) {
            Err(CacheError::ObjectNotFound { checksum }) => assert_eq!(checksum, foo_sum),
            other => panic!("expected ObjectNotFound, got {other:?}"),
        }
    }

    #[test]
    fn cache_directory_lines_excluded_from_integrity() {
        // A manifest whose only entry is a directory (no file objects on disk)
        // still passes integrity — directory lines are excluded.
        let tmp = Scratch::new();
        let mut manifest = Manifest::new();
        manifest.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            "deadbeef",
            0,
            "./",
        ));
        let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        put_manifest(tmp.path(), id, &manifest);
        check_snapshot_integrity(tmp.path(), id, &Blake3Hasher)
            .expect("directory-only manifest passes");
    }

    #[test]
    fn cache_empty_or_absent_objects_dir_is_clean_pass() {
        // Absent .objects entirely.
        let tmp = Scratch::new();
        let report = verify_cache(tmp.path(), false, &Blake3Hasher).unwrap();
        assert_eq!(report, CacheReport::default());
        assert!(report.is_clean());
        assert_eq!(report.checked, 0);

        // Present-but-empty .objects.
        fs::create_dir_all(tmp.path().join(OBJECTS_DIR)).unwrap();
        let report = verify_cache(tmp.path(), false, &Blake3Hasher).unwrap();
        assert_eq!(report.checked, 0);
        assert!(report.is_clean());
    }

    #[test]
    fn cache_verify_reconstructs_expected_checksum_from_path() {
        // Directly guard the sed-equivalent path->checksum reconstruction: an
        // object filed under a known address reconstructs exactly that address.
        let tmp = Scratch::new();
        let checksum = put_object(tmp.path(), b"hello cache\n");
        let objects_root = tmp.path().join(OBJECTS_DIR);
        let object = tmp.path().join(object_path(&checksum));
        let got = expected_checksum_from_path(&objects_root, &object).unwrap();
        assert_eq!(got, checksum);
    }

    #[test]
    fn cache_flush_empties_objects_and_manifests() {
        let tmp = Scratch::new();
        let (_id, _foo, _bar) = build_clean_cache(tmp.path());
        assert!(tmp.path().join(OBJECTS_DIR).exists());
        assert!(tmp.path().join(MANIFESTS_DIR_TEST).exists());

        flush_cache(tmp.path()).expect("flush succeeds");

        assert!(!tmp.path().join(OBJECTS_DIR).exists());
        assert!(!tmp.path().join(MANIFESTS_DIR_TEST).exists());
        // The cache dir itself survives and is empty.
        assert!(tmp.path().is_dir());
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn cache_flush_is_idempotent_on_missing_dir() {
        let tmp = Scratch::new();
        let missing = tmp.path().join("does-not-exist");
        flush_cache(&missing).expect("flush on missing dir is a no-op");
    }

    const MANIFESTS_DIR_TEST: &str = ".manifests";
}
