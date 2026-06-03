//! Integration tests for the external-store emit-command shim.
//!
//! These exercise [`snapdir_stores::ExternalStore`] end-to-end against the
//! `tests/snapdir-mock-store` script, which mirrors the documented emit-command
//! contract (`get-manifest-command` / `get-fetch-files-command` /
//! `get-push-command`) a third-party store binary implements. The shim spawns
//! the (third-party) mock binary, captures the shell scripts it emits, and
//! `eval`s them — proving the shim honors the contract and its invariants
//! (objects-before-manifest on push, id-verify on get-manifest, error scan on
//! fetch). Test names contain `shim` so `cargo test -p snapdir-stores shim`
//! selects them.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};

use snapdir_stores::ExternalStore;

/// Absolute path to the mock store script shipped beside this test.
fn mock_store_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapdir-mock-store")
}

/// A unique temp dir removed on drop (no dev-dependency needed).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "snapdir-shim-test-{}-{tag}-{n}",
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

/// Builds a one-file manifest ("foo" -> "foo\n") and writes a sharded staging
/// directory (object + manifest in `.objects/`/`.manifests/`) under `staging`,
/// exactly the layout the mock store's `get-push-command` reads from. Returns
/// the manifest and its snapshot id.
fn stage_single_file(staging: &Path) -> (Manifest, String) {
    let hasher = Blake3Hasher::new();
    let foo_sum = hasher.hash_hex(b"foo\n");
    let root_sum = directory_checksum([foo_sum.as_str()], &hasher);

    let mut manifest = Manifest::new();
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        4,
        "./",
    ));
    manifest.push(ManifestEntry::new(
        PathType::File,
        "600",
        foo_sum.clone(),
        4,
        "./foo",
    ));
    let id = snapshot_id(&manifest, &hasher);

    // Stage the object under its sharded path.
    let obj_path = staging.join(object_path(&foo_sum));
    fs::create_dir_all(obj_path.parent().unwrap()).unwrap();
    fs::write(&obj_path, b"foo\n").unwrap();

    // Stage the manifest under its sharded path.
    let man_path = staging.join(manifest_path(&id));
    fs::create_dir_all(man_path.parent().unwrap()).unwrap();
    fs::write(&man_path, manifest.to_string()).unwrap();

    (manifest, id)
}

fn store_url(store_root: &Path) -> String {
    format!("mock://{}", store_root.display())
}

#[test]
fn shim_push_writes_objects_before_manifest_then_get_manifest_roundtrips() {
    let staging = TempDir::new("stage");
    let store_root = TempDir::new("store");
    let (manifest, id) = stage_single_file(staging.path());

    let store = ExternalStore::with_binary(&store_url(store_root.path()), mock_store_bin());

    // Push: the mock emits object cp(s) BEFORE the manifest cp; the shim evals it.
    store.push(&manifest, staging.path()).expect("push");

    // The object and the manifest both landed in the store.
    let foo_sum = Blake3Hasher::new().hash_hex(b"foo\n");
    assert!(
        store_root.path().join(object_path(&foo_sum)).is_file(),
        "object should have been pushed"
    );
    assert!(
        store_root.path().join(manifest_path(&id)).is_file(),
        "manifest should have been pushed"
    );

    // get-manifest-command round-trips and id-verifies.
    let fetched = store.get_manifest(&id).expect("get_manifest");
    assert_eq!(fetched.to_string(), manifest.to_string());
}

#[test]
fn shim_get_manifest_missing_id_maps_to_manifest_not_found() {
    let store_root = TempDir::new("store-empty");
    let store = ExternalStore::with_binary(&store_url(store_root.path()), mock_store_bin());

    let missing = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    match store.get_manifest(missing) {
        Err(StoreError::ManifestNotFound { id }) => assert_eq!(id, missing),
        other => panic!("expected ManifestNotFound, got {other:?}"),
    }
}

#[test]
fn shim_fetch_files_pulls_objects_into_cache_dir() {
    let staging = TempDir::new("fstage");
    let store_root = TempDir::new("fstore");
    let cache = TempDir::new("fcache");
    let (manifest, _id) = stage_single_file(staging.path());

    let store = ExternalStore::with_binary(&store_url(store_root.path()), mock_store_bin());
    store.push(&manifest, staging.path()).expect("push");

    // get-fetch-files-command reads the manifest from stdin, emits per-object
    // fetch commands; the shim feeds the manifest and evals the script.
    store
        .fetch_files(&manifest, cache.path())
        .expect("fetch_files");

    let foo_sum = Blake3Hasher::new().hash_hex(b"foo\n");
    let fetched_obj = cache.path().join(object_path(&foo_sum));
    assert!(
        fetched_obj.is_file(),
        "object should have been fetched into cache"
    );
    assert_eq!(fs::read(&fetched_obj).unwrap(), b"foo\n");
}

#[test]
fn shim_fetch_files_surfaces_missing_object_error() {
    // Store has the manifest path but NOT the object: the emitted fetch script
    // prints `ERROR: missing object …` and exits non-zero; the shim must fail.
    let staging = TempDir::new("estage");
    let store_root = TempDir::new("estore");
    let cache = TempDir::new("ecache");
    let (manifest, id) = stage_single_file(staging.path());

    // Place only the manifest in the store (no object).
    let man_path = store_root.path().join(manifest_path(&id));
    fs::create_dir_all(man_path.parent().unwrap()).unwrap();
    fs::write(&man_path, manifest.to_string()).unwrap();

    let store = ExternalStore::with_binary(&store_url(store_root.path()), mock_store_bin());
    let err = store.fetch_files(&manifest, cache.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::Backend { .. }),
        "expected Backend error from failed fetch transaction, got {err:?}"
    );
}

#[test]
fn shim_push_is_noop_when_manifest_already_present() {
    let staging = TempDir::new("nstage");
    let store_root = TempDir::new("nstore");
    let (manifest, id) = stage_single_file(staging.path());

    let store = ExternalStore::with_binary(&store_url(store_root.path()), mock_store_bin());
    store.push(&manifest, staging.path()).expect("first push");

    // Remove the object from the store but keep the manifest: a second push
    // must be a no-op (skip-if-present), so it must NOT re-copy the object.
    let foo_sum = Blake3Hasher::new().hash_hex(b"foo\n");
    fs::remove_file(store_root.path().join(object_path(&foo_sum))).unwrap();

    store
        .push(&manifest, staging.path())
        .expect("second push (no-op)");
    assert!(
        !store_root.path().join(object_path(&foo_sum)).exists(),
        "second push should have been a no-op (manifest already present)"
    );
    assert!(store_root.path().join(manifest_path(&id)).is_file());
}
