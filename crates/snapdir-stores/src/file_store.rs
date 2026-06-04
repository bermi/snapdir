//! `FileStore`: the `file://` storage backend.
//!
//! A [`FileStore`] is rooted at a local directory and holds the frozen
//! content-addressable `.objects`/`.manifests` sharded layout, so a store
//! directory written by any conforming implementation is interchangeable:
//!
//! ```text
//! <root>/.objects/<sharded checksum>     raw file bytes
//! <root>/.manifests/<sharded snapshot id> manifest text
//! ```
//!
//! Sharding and the on-disk paths come straight from [`snapdir_core::store`]
//! ([`object_path`] / [`manifest_path`]); this module never reimplements them.
//!
//! # Oracle parity
//!
//! - **`new` / URL parsing** mirrors `_snapdir_file_store_get_store_dir`:
//!   strips a leading `file://`, `file:///`, `file://localhost/` (etc.) prefix
//!   down to an absolute path and drops a trailing slash.
//! - **`push`** mirrors `snapdir_file_store_get_push_command` +
//!   `_snapdir_file_store_persit`: it is a no-op if the manifest already exists
//!   (skip-if-present); otherwise it writes every referenced object that is
//!   absent (skip-if-present per object) *before* writing the manifest, so a
//!   present manifest always implies all of its objects are present.
//! - **`fetch_files` / `get_manifest`** mirror the fetch side of
//!   `_snapdir_file_store_persit`: copy to a temp path, verify the content
//!   BLAKE3 against the expected checksum, retry up to five times, then
//!   atomically rename into place.
//!
//! All I/O is native in-process filesystem I/O; nothing shells out.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use snapdir_core::manifest::{Manifest, PathType};
use snapdir_core::merkle::{Blake3Hasher, Hasher};
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};

use crate::util::{file_present_and_verified, hash_file};

/// Number of times the oracle retries a persist whose copied bytes fail their
/// checksum but whose source still verifies (`_SNAPDIR_FILE_STORE_RETRIES`).
const MAX_PERSIST_RETRIES: u32 = 5;

/// A content-addressable store backed by a local directory (the `file://`
/// backend).
///
/// Construct one with [`FileStore::new`] (parsing a `file://` URL or a bare
/// path) or [`FileStore::from_root`] (an already-resolved directory).
#[derive(Debug, Clone)]
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    /// Builds a store from a `store` URL or path, matching the oracle's
    /// `_snapdir_file_store_get_store_dir`.
    ///
    /// Accepts `file:///abs/path`, `file://localhost/abs/path`, `file://`
    /// followed by an absolute path, or a bare absolute path. A leading
    /// `file:` scheme (with any number of slashes, optionally `localhost`) is
    /// rewritten to a single leading `/`, and a trailing slash is dropped.
    #[must_use]
    pub fn new(store: &str) -> Self {
        Self::from_root(parse_store_dir(store))
    }

    /// Builds a store rooted at an already-resolved directory.
    #[must_use]
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the store's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute on-disk path of an object given its checksum.
    fn object_disk_path(&self, checksum: &str) -> PathBuf {
        self.root.join(object_path(checksum))
    }

    /// Absolute on-disk path of a manifest given its snapshot id.
    fn manifest_disk_path(&self, id: &str) -> PathBuf {
        self.root.join(manifest_path(id))
    }
}

impl Store for FileStore {
    fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError> {
        let path = self.manifest_disk_path(id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::ManifestNotFound { id: id.to_owned() });
            }
            Err(err) => return Err(StoreError::Io(err)),
        };

        // The snapshot id is BLAKE3 of the comment-stripped manifest text with
        // the oracle's trailing `echo` newline. Verify the stored bytes hash
        // back to `id` before trusting them (oracle: the manifest id check on
        // fetch). `snapshot_id` in core re-renders + re-hashes the parsed
        // manifest, so parse first, then verify against the parsed form.
        let text = String::from_utf8(bytes).map_err(|err| StoreError::Backend {
            message: format!("manifest {id} is not valid UTF-8"),
            source: Some(Box::new(err)),
        })?;
        let manifest = Manifest::parse(&text)?;

        let actual = snapdir_core::merkle::snapshot_id(&manifest, &Blake3Hasher::new());
        if actual != id {
            return Err(StoreError::Integrity {
                address: manifest_path(id),
                expected: id.to_owned(),
                actual,
            });
        }

        Ok(manifest)
    }

    fn fetch_files(&self, manifest: &Manifest, dest: &Path) -> Result<(), StoreError> {
        let hasher = Blake3Hasher::new();
        for entry in manifest.entries() {
            let rel = strip_leading_dot_slash(&entry.path);
            let target = dest.join(rel);
            match entry.path_type {
                PathType::Directory => {
                    fs::create_dir_all(&target)?;
                }
                PathType::File => {
                    // Skip-if-present-and-verified: a destination file that
                    // already exists and whose content hashes to the manifest's
                    // checksum needs no copy — and critically no object read at
                    // all (so a populated dest succeeds even if the store object
                    // is gone). A mismatching/corrupt local file falls through
                    // and is repaired by the persist below.
                    if file_present_and_verified(&target, &entry.checksum, &hasher) {
                        continue;
                    }
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let source = self.object_disk_path(&entry.checksum);
                    if !source.exists() {
                        return Err(StoreError::ObjectNotFound {
                            checksum: entry.checksum.clone(),
                        });
                    }
                    persist(&source, &target, &entry.checksum, &hasher)?;
                }
            }
        }
        Ok(())
    }

    fn push(&self, manifest: &Manifest, source: &Path) -> Result<(), StoreError> {
        // Compute the snapshot id of the manifest we are about to push so we
        // can locate (and skip-if-present) its manifest file.
        let hasher = Blake3Hasher::new();
        let id = snapdir_core::merkle::snapshot_id(manifest, &hasher);
        let manifest_target = self.manifest_disk_path(&id);

        // Skip-if-present: nothing to do when the manifest already exists. A
        // present manifest implies all its objects are present (we maintain
        // that invariant by writing the manifest last).
        if manifest_target.exists() {
            return Ok(());
        }

        // Push every referenced object that is absent, BEFORE the manifest.
        for entry in manifest.entries() {
            if entry.path_type != PathType::File {
                continue;
            }
            let object_target = self.object_disk_path(&entry.checksum);
            if object_target.exists() {
                // Skip-if-present per object: trust an object already filed
                // under its content address (it is content-addressable).
                continue;
            }
            let rel = strip_leading_dot_slash(&entry.path);
            let object_source = source.join(rel);
            persist(&object_source, &object_target, &entry.checksum, &hasher)?;
        }

        // Write the manifest last, via the same verify/retry/atomic-rename
        // path, so a present manifest always implies present objects.
        write_manifest(manifest, &manifest_target, &id, &hasher)?;
        Ok(())
    }
}

/// Copies `source` to `target`, verifying the content BLAKE3 against
/// `expected`, retrying up to [`MAX_PERSIST_RETRIES`] times, then atomically
/// renaming into place. Mirrors `_snapdir_file_store_persit`.
fn persist(
    source: &Path,
    target: &Path,
    expected: &str,
    hasher: &impl Hasher,
) -> Result<(), StoreError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut attempts_left = MAX_PERSIST_RETRIES;
    loop {
        // Copy to a unique temp path beside the target so the final rename is
        // an atomic, same-filesystem move (the oracle's `.tmp` discipline).
        let tmp = temp_sibling(target);
        copy_file(source, &tmp)?;

        let actual = hash_file(&tmp, hasher)?;
        if actual == expected {
            // Atomic rename into the final content-addressed location.
            fs::rename(&tmp, target)?;
            return Ok(());
        }

        // Copied bytes did not verify. Clean up the temp file and decide
        // whether to retry: the oracle only retries when the *source* still
        // hashes to the expected value, otherwise the source itself is bad.
        let _ = fs::remove_file(&tmp);
        let source_actual = hash_file(source, hasher)?;
        if source_actual != expected {
            return Err(StoreError::Integrity {
                address: source.display().to_string(),
                expected: expected.to_owned(),
                actual: source_actual,
            });
        }

        attempts_left = attempts_left.saturating_sub(1);
        if attempts_left == 0 {
            return Err(StoreError::Integrity {
                address: target.display().to_string(),
                expected: expected.to_owned(),
                actual,
            });
        }
    }
}

/// Writes a manifest's text to `target`, verifying it hashes to `id`, then
/// atomically renaming into place. The manifest's "content" is the
/// snapshot-id-bearing text (`Display` + trailing newline), so we verify with
/// [`snapdir_core::merkle::snapshot_id`] rather than a raw byte hash.
fn write_manifest(
    manifest: &Manifest,
    target: &Path,
    id: &str,
    hasher: &impl Hasher,
) -> Result<(), StoreError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    // The on-disk manifest must hash (snapshot_id) back to `id`. Render once
    // and confirm before writing.
    let actual = snapdir_core::merkle::snapshot_id(manifest, hasher);
    if actual != id {
        return Err(StoreError::Integrity {
            address: target.display().to_string(),
            expected: id.to_owned(),
            actual,
        });
    }

    // Oracle stores `echo "${manifest}"` — the manifest text plus a single
    // trailing newline (the same bytes snapshot_id hashes).
    let mut text = manifest.to_string();
    text.push('\n');

    let tmp = temp_sibling(target);
    fs::write(&tmp, text.as_bytes())?;
    fs::rename(&tmp, target)?;
    Ok(())
}

/// Copies a regular file's bytes from `source` to `target` (mirrors the
/// oracle's `cp -RL -n`: dereference, do not clobber — `target` is a fresh
/// temp path so the no-clobber aspect is implicit).
fn copy_file(source: &Path, target: &Path) -> Result<(), StoreError> {
    fs::copy(source, target)?;
    Ok(())
}

/// Builds a unique temp sibling path for `target` (same directory, so the
/// final rename stays on one filesystem). Uses pid + a process-monotonic
/// counter so concurrent persists never collide.
fn temp_sibling(target: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp_name = format!("{file_name}.{pid}.{n}.tmp");
    match target.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}

/// Strips a leading `./` (relative-mode manifest paths) and a trailing `/`
/// (directory entries) so the remainder can be joined onto a destination root.
fn strip_leading_dot_slash(path: &str) -> &str {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    trimmed.strip_suffix('/').unwrap_or(trimmed)
}

/// Resolves a `store` URL/path to its on-disk directory, matching the oracle's
/// `_snapdir_file_store_get_store_dir`:
///
/// ```sh
/// store_dir="$(echo "$store" | sed -E 's|^file:/*(localhost/?)?|/|')"
/// echo "${store_dir%/}"
/// ```
///
/// i.e. replace a leading `file:` + any number of `/` (optionally followed by
/// `localhost` + optional `/`) with a single `/`, then strip a trailing slash.
fn parse_store_dir(store: &str) -> PathBuf {
    let resolved = if let Some(rest) = store.strip_prefix("file:") {
        // Drop the run of slashes the scheme leaves behind.
        let rest = rest.trim_start_matches('/');
        // An optional `localhost` host segment, with an optional trailing
        // slash, is also dropped by the oracle's regex.
        let rest = if let Some(after) = rest.strip_prefix("localhost") {
            after.strip_prefix('/').unwrap_or(after)
        } else {
            rest
        };
        // The regex always substitutes a single leading `/`.
        format!("/{rest}")
    } else {
        store.to_owned()
    };

    // `${store_dir%/}` — strip a single trailing slash (but keep a bare "/").
    let trimmed = if resolved.len() > 1 {
        resolved.strip_suffix('/').unwrap_or(&resolved)
    } else {
        &resolved
    };
    PathBuf::from(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapdir_core::manifest::ManifestEntry;
    use std::fs;
    use std::path::Path;

    // A tiny temp-dir helper so tests don't pull in a dev-dependency. Creates a
    // unique directory under the system temp dir and removes it on drop.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "snapdir-filestore-test-{}-{tag}-{n}",
                std::process::id()
            ));
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

    /// Builds a manifest for a source tree containing `foo` ("foo\n") and
    /// `bar` ("bar\n") and writes those files into `source`. Returns the
    /// manifest and its snapshot id. Checksums are the real BLAKE3 of the
    /// file bytes so the store's verification passes.
    fn make_foo_bar_source(source: &Path) -> (Manifest, String) {
        let hasher = Blake3Hasher::new();
        fs::write(source.join("foo"), b"foo\n").unwrap();
        fs::write(source.join("bar"), b"bar\n").unwrap();
        let foo_sum = hasher.hash_hex(b"foo\n");
        let bar_sum = hasher.hash_hex(b"bar\n");

        let root_sum =
            snapdir_core::merkle::directory_checksum([foo_sum.as_str(), bar_sum.as_str()], &hasher);

        let mut manifest = Manifest::new();
        manifest.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            root_sum,
            8,
            "./",
        ));
        manifest.push(ManifestEntry::new(
            PathType::File,
            "600",
            bar_sum,
            4,
            "./bar",
        ));
        manifest.push(ManifestEntry::new(
            PathType::File,
            "600",
            foo_sum,
            4,
            "./foo",
        ));
        let manifest = Manifest::from_entries(manifest.entries().to_vec());
        let id = snapdir_core::merkle::snapshot_id(&manifest, &hasher);
        (manifest, id)
    }

    #[test]
    fn file_store_parse_store_dir_matches_oracle_sed() {
        // file:// + abs path -> abs path; trailing slash stripped.
        assert_eq!(
            parse_store_dir("file:///tmp/store"),
            PathBuf::from("/tmp/store")
        );
        assert_eq!(
            parse_store_dir("file:///tmp/store/"),
            PathBuf::from("/tmp/store")
        );
        // localhost host segment dropped.
        assert_eq!(
            parse_store_dir("file://localhost/tmp/store"),
            PathBuf::from("/tmp/store")
        );
        // file:// + abs path with two slashes.
        assert_eq!(
            parse_store_dir("file://tmp/store"),
            PathBuf::from("/tmp/store")
        );
        // bare absolute path left intact.
        assert_eq!(parse_store_dir("/tmp/store"), PathBuf::from("/tmp/store"));
        // bare root preserved.
        assert_eq!(parse_store_dir("file:///"), PathBuf::from("/"));
    }

    #[test]
    fn file_store_push_lands_objects_at_sharded_keys_and_manifest_last() {
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_foo_bar_source(src_dir.path());

        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("push ok");

        // Objects land at the exact sharded keys.
        for entry in manifest.entries() {
            if entry.path_type == PathType::File {
                let obj = store_dir.path().join(object_path(&entry.checksum));
                assert!(obj.exists(), "expected object at {}", obj.display());
                // Content matches.
                let bytes = fs::read(&obj).unwrap();
                assert_eq!(
                    Blake3Hasher::new().hash_hex(&bytes),
                    entry.checksum,
                    "object content must hash to its address"
                );
            }
        }

        // Manifest written at its sharded key, and hashes back to the id.
        let man_path = store_dir.path().join(manifest_path(&id));
        assert!(man_path.exists(), "manifest must exist after push");
        let read_back = store.get_manifest(&id).expect("manifest reads back");
        assert_eq!(read_back, manifest);
    }

    #[test]
    fn file_store_push_skips_when_manifest_present() {
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_foo_bar_source(src_dir.path());
        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("first push");

        // Remove an object but keep the manifest: a second push must skip
        // entirely (manifest-present short-circuit), leaving the object gone.
        let foo_entry = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./foo")
            .unwrap();
        let obj = store_dir.path().join(object_path(&foo_entry.checksum));
        fs::remove_file(&obj).unwrap();

        let _ = id;
        store
            .push(&manifest, src_dir.path())
            .expect("second push skips");
        assert!(
            !obj.exists(),
            "manifest-present push must be a full no-op (object stays removed)"
        );
    }

    #[test]
    fn file_store_push_skips_present_objects_but_adds_missing() {
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_foo_bar_source(src_dir.path());
        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("first push");

        // Delete the manifest and one object; re-push must re-create the
        // missing object (and the manifest) without erroring on the present one.
        let man_path = store_dir.path().join(manifest_path(&id));
        fs::remove_file(&man_path).unwrap();
        let foo_entry = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./foo")
            .unwrap();
        let foo_obj = store_dir.path().join(object_path(&foo_entry.checksum));
        fs::remove_file(&foo_obj).unwrap();

        store.push(&manifest, src_dir.path()).expect("re-push");
        assert!(foo_obj.exists(), "missing object must be re-added");
        assert!(man_path.exists(), "manifest must be re-written");
    }

    #[test]
    fn file_store_fetch_round_trips_and_verifies() {
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let dest_dir = TempDir::new("dest");
        let (manifest, id) = make_foo_bar_source(src_dir.path());
        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("push");

        let fetched = store.get_manifest(&id).expect("get manifest");
        store
            .fetch_files(&fetched, dest_dir.path())
            .expect("fetch files");

        assert_eq!(fs::read(dest_dir.path().join("foo")).unwrap(), b"foo\n");
        assert_eq!(fs::read(dest_dir.path().join("bar")).unwrap(), b"bar\n");
    }

    #[test]
    fn file_store_get_manifest_missing_is_not_found() {
        let store_dir = TempDir::new("store");
        let store = FileStore::from_root(store_dir.path());
        let missing = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        match store.get_manifest(missing) {
            Err(StoreError::ManifestNotFound { id }) => assert_eq!(id, missing),
            other => panic!("expected ManifestNotFound, got {other:?}"),
        }
    }

    #[test]
    fn file_store_get_manifest_tampered_fails_integrity() {
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_foo_bar_source(src_dir.path());
        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("push");

        // Tamper with the stored manifest bytes.
        let man_path = store_dir.path().join(manifest_path(&id));
        fs::write(&man_path, b"D 700 deadbeef 0 ./\n").unwrap();

        match store.get_manifest(&id) {
            Err(StoreError::Integrity { expected, .. }) => assert_eq!(expected, id),
            other => panic!("expected Integrity, got {other:?}"),
        }
    }

    #[test]
    fn file_store_fetch_missing_object_is_not_found() {
        let store_dir = TempDir::new("store");
        let dest_dir = TempDir::new("dest");
        let hasher = Blake3Hasher::new();
        let foo_sum = hasher.hash_hex(b"foo\n");

        let mut manifest = Manifest::new();
        manifest.push(ManifestEntry::new(PathType::Directory, "700", "x", 4, "./"));
        manifest.push(ManifestEntry::new(
            PathType::File,
            "600",
            foo_sum.clone(),
            4,
            "./foo",
        ));

        let store = FileStore::from_root(store_dir.path());
        match store.fetch_files(&manifest, dest_dir.path()) {
            Err(StoreError::ObjectNotFound { checksum }) => assert_eq!(checksum, foo_sum),
            other => panic!("expected ObjectNotFound, got {other:?}"),
        }
    }

    #[test]
    fn file_store_persist_rejects_corrupt_source() {
        // A "source" object whose bytes do not match the claimed checksum must
        // fail integrity (the oracle's "Invalid source checksum" path), not
        // silently store corrupt data.
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let dest_dir = TempDir::new("dest");
        let hasher = Blake3Hasher::new();

        // Real foo source/manifest, then corrupt the stored object so fetch's
        // verify-on-copy trips and the source (the corrupt store object) fails.
        let (manifest, id) = make_foo_bar_source(src_dir.path());
        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("push");

        let foo_entry = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./foo")
            .unwrap();
        let foo_obj = store_dir.path().join(object_path(&foo_entry.checksum));
        fs::write(&foo_obj, b"corrupted not foo\n").unwrap();
        // Sanity: the corrupted bytes really differ from the expected sum.
        assert_ne!(hasher.hash_hex(b"corrupted not foo\n"), foo_entry.checksum);

        let fetched = store.get_manifest(&id).expect("manifest still valid");
        match store.fetch_files(&fetched, dest_dir.path()) {
            Err(StoreError::Integrity { expected, .. }) => {
                assert_eq!(expected, foo_entry.checksum);
            }
            other => panic!("expected Integrity from corrupt object, got {other:?}"),
        }
        // The corrupt object must NOT have been materialized at the dest.
        assert!(!dest_dir.path().join("foo").exists());
    }

    #[test]
    fn fetch_skip_present_verified() {
        // Push a tree, fetch it (populating dest), then DELETE the store's whole
        // `.objects` tree so any object read would now fail with ObjectNotFound.
        // A second fetch into the SAME dest must still return Ok — proving every
        // file was skipped via local checksum match (ZERO object reads).
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let dest_dir = TempDir::new("dest");
        let (manifest, id) = make_foo_bar_source(src_dir.path());

        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("push");

        let fetched = store.get_manifest(&id).expect("get manifest");
        store
            .fetch_files(&fetched, dest_dir.path())
            .expect("first fetch populates dest");
        assert_eq!(fs::read(dest_dir.path().join("foo")).unwrap(), b"foo\n");
        assert_eq!(fs::read(dest_dir.path().join("bar")).unwrap(), b"bar\n");

        // Nuke every object in the store. Any read of an object now fails.
        let objects = store_dir.path().join(".objects");
        fs::remove_dir_all(&objects).expect("remove .objects tree");
        assert!(!objects.exists());

        // Second fetch into the populated dest must succeed without reading a
        // single (now-missing) object.
        store
            .fetch_files(&fetched, dest_dir.path())
            .expect("second fetch skips every present+verified file (no object reads)");

        // Dest contents intact.
        assert_eq!(fs::read(dest_dir.path().join("foo")).unwrap(), b"foo\n");
        assert_eq!(fs::read(dest_dir.path().join("bar")).unwrap(), b"bar\n");
    }

    #[test]
    fn file_store_fetch_repairs_corrupt_dest_and_skips_intact() {
        // With store objects present: corrupt one dest file. The corrupted file
        // is re-fetched (repaired) to match its checksum again, while an
        // unrelated already-correct dest file is still skipped.
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let dest_dir = TempDir::new("dest");
        let (manifest, id) = make_foo_bar_source(src_dir.path());

        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("push");
        let fetched = store.get_manifest(&id).expect("get manifest");
        store
            .fetch_files(&fetched, dest_dir.path())
            .expect("first fetch populates dest");

        // Corrupt `foo` in the dest; leave `bar` correct.
        fs::write(dest_dir.path().join("foo"), b"WRONG\n").unwrap();
        // Remove `bar`'s store object so it CANNOT be re-fetched; the only way a
        // second fetch can succeed is if `bar` is skipped (present + verified).
        let bar_entry = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./bar")
            .unwrap();
        let bar_obj = store_dir.path().join(object_path(&bar_entry.checksum));
        fs::remove_file(&bar_obj).unwrap();

        store
            .fetch_files(&fetched, dest_dir.path())
            .expect("repair corrupt foo, skip intact bar");

        // foo repaired back to its checksummed content; bar untouched.
        assert_eq!(fs::read(dest_dir.path().join("foo")).unwrap(), b"foo\n");
        assert_eq!(fs::read(dest_dir.path().join("bar")).unwrap(), b"bar\n");
    }

    #[test]
    fn file_store_fetch_mismatch_then_missing_object_errors() {
        // Confirms the skip is checksum-gated, not mere existence: corrupt a
        // dest file AND remove its store object → fetch cannot repair and errors
        // ObjectNotFound (it did not blindly skip the present-but-wrong file).
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let dest_dir = TempDir::new("dest");
        let (manifest, id) = make_foo_bar_source(src_dir.path());

        let store = FileStore::from_root(store_dir.path());
        store.push(&manifest, src_dir.path()).expect("push");
        let fetched = store.get_manifest(&id).expect("get manifest");
        store
            .fetch_files(&fetched, dest_dir.path())
            .expect("first fetch populates dest");

        let foo_entry = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./foo")
            .unwrap();
        // Corrupt the dest file so the skip gate fails for it...
        fs::write(dest_dir.path().join("foo"), b"WRONG\n").unwrap();
        // ...and remove its store object so it cannot be repaired.
        let foo_obj = store_dir.path().join(object_path(&foo_entry.checksum));
        fs::remove_file(&foo_obj).unwrap();

        match store.fetch_files(&fetched, dest_dir.path()) {
            Err(StoreError::ObjectNotFound { checksum }) => {
                assert_eq!(checksum, foo_entry.checksum);
            }
            other => panic!("expected ObjectNotFound (cannot repair), got {other:?}"),
        }
    }

    #[test]
    fn file_store_strip_leading_dot_slash() {
        assert_eq!(strip_leading_dot_slash("./foo"), "foo");
        assert_eq!(strip_leading_dot_slash("./a/b/c"), "a/b/c");
        assert_eq!(strip_leading_dot_slash("./a/"), "a");
        assert_eq!(strip_leading_dot_slash("./"), "");
        assert_eq!(strip_leading_dot_slash("/abs/path"), "/abs/path");
    }
}
