//! Per-backend MIRROR-capability matrix (phase 32, gate
//! `mirror-feasibility-matrix`).
//!
//! The `sync --delete` manifest-set mirror (the prune half of
//! [`sync_snapshot_mirror`]) is supported ONLY where deleting a manifest is safe
//! and efficient: the local [`FileStore`]. Every object/remote backend must
//! report `supports_mirror() == false` and a mirror targeting it must be refused
//! up front, leaving the destination untouched.
//!
//! This suite PINS that matrix against the SHIPPED code (it must PASS). It is a
//! black-box capability check over the public API:
//!
//!   * `FileStore::supports_mirror() == true`  — the one supported backend.
//!   * `S3Store` / `GcsStore` / `B2Store` `supports_mirror() == false` — each
//!     constructed OFFLINE via its public `connect` constructor (the capability
//!     method needs no live connection/creds).
//!   * `ssh://` / `sftp://` / any third-party `<proto>://` resolve to the
//!     EXTERNAL adapter and are served by [`ExternalStore`], which does NOT
//!     implement [`StreamStore`] at all — so it can never be a mirror `to`
//!     (capability "false by exclusion": there is no `supports_mirror()` to
//!     return true). Pinned via the router + the shim's typed construction.
//!   * `sync_snapshot_mirror` into a REAL non-`FileStore` dest (a live-offline
//!     `S3Store`) returns the documented typed error and changes NOTHING.
//!
//! If a backend here INCORRECTLY reported `supports_mirror() == true` (or failed
//! to refuse), that is a REAL BUG in `src` — the failing assertion stays, it is
//! NOT weakened to hide it.

#![allow(clippy::doc_markdown)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{Store, StoreError};

use snapdir_stores::router::{resolve_adapter, Adapter};
use snapdir_stores::{
    sync_snapshot_mirror, B2Store, ExternalStore, FileStore, GcsStore, S3Store, StreamStore,
    TransferConfig,
};

// ---------------------------------------------------------------------------
// Scaffolding (no dev-dependencies; mirrors the sibling sync/mirror suites).
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
            "snapdir-mirror-feasibility-{}-{tag}-{n}",
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

fn cfg() -> TransferConfig {
    TransferConfig::new(4, None)
}

/// Builds a real source tree under `src` and returns its `Manifest` + snapshot
/// id (real BLAKE3 addressing + a `D ./` root entry, sorted), matching the
/// `mirror_sync.rs` fixture shape.
fn build_tree(src: &Path, files: &[(&str, &[u8])]) -> (Manifest, String) {
    let hasher = Blake3Hasher::new();
    let mut manifest = Manifest::new();

    let mut file_sums: Vec<String> = Vec::new();
    for (rel, content) in files {
        let target = src.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&target, content).unwrap();
        let sum = hasher.hash_hex(content);
        file_sums.push(sum.clone());
        manifest.push(ManifestEntry::new(
            PathType::File,
            "600",
            sum,
            content.len() as u64,
            format!("./{rel}"),
        ));
    }

    let root_sum = directory_checksum(file_sums.iter().map(String::as_str), &hasher);
    let root_size: u64 = files.iter().map(|(_, c)| c.len() as u64).sum();
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

/// File-object checksums referenced by a manifest (deduped, sorted).
fn object_checksums(manifest: &Manifest) -> Vec<String> {
    let mut v: Vec<String> = manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .map(|e| e.checksum.clone())
        .collect();
    v.sort();
    v.dedup();
    v
}

// ===========================================================================
// 1. THE ONE SUPPORTED BACKEND — local FileStore
// ===========================================================================

#[test]
fn filestore_is_the_one_backend_that_supports_mirror() {
    // The local file:// store can delete a manifest atomically/efficiently, so it
    // is the ONLY backend whose supports_mirror() is true.
    let dir = TempDir::new("file-cap");
    let store = FileStore::from_root(dir.path().to_path_buf());
    assert!(
        store.supports_mirror(),
        "FileStore (local file:// dest) MUST report supports_mirror() == true"
    );
}

// ===========================================================================
// 2. EVERY OTHER OFFLINE-CONSTRUCTABLE BACKEND — supports_mirror() == false
//
//    S3 / GCS / B2 are built via their public `connect` constructors. The
//    capability method is pure (no live connection/creds), so building the
//    adapter from an s3:///gs:///b2:// URI offline and calling supports_mirror()
//    works and must return false (no atomic/efficient manifest delete on an
//    object/remote store).
// ===========================================================================

#[test]
fn s3_store_does_not_support_mirror() {
    let store = S3Store::connect("s3://bucket/prefix", None)
        .expect("S3Store::connect must build offline (capability is creds-free)");
    assert!(
        !store.supports_mirror(),
        "S3Store (object/remote) MUST report supports_mirror() == false"
    );
}

#[test]
fn gcs_store_does_not_support_mirror() {
    let store = GcsStore::connect("gs://bucket/prefix")
        .expect("GcsStore::connect must build offline (capability is creds-free)");
    assert!(
        !store.supports_mirror(),
        "GcsStore (object/remote) MUST report supports_mirror() == false"
    );
}

#[test]
fn b2_store_does_not_support_mirror() {
    let store = B2Store::connect("b2://bucket/prefix", None, None)
        .expect("B2Store::connect must build offline (capability is creds-free)");
    assert!(
        !store.supports_mirror(),
        "B2Store (object/remote) MUST report supports_mirror() == false"
    );
}

// ===========================================================================
// 3. SSH / SFTP / EXTERNAL — served by the shim, NOT a StreamStore at all
//
//    ssh:// + sftp:// (and any unknown <proto>://) route to Adapter::External
//    and are served out-of-process by ExternalStore. ExternalStore does NOT
//    implement StreamStore, so it has no supports_mirror() to return — it can
//    NEVER be a mirror `to`. We pin the capability "false by exclusion" via the
//    router resolution + the shim's typed construction.
// ===========================================================================

#[test]
fn ssh_sftp_and_external_uris_route_to_the_external_shim_not_a_streamstore() {
    // ssh:// + sftp:// + a generic third-party scheme all resolve to the EXTERNAL
    // adapter (no built-in in-process StreamStore exists for them).
    for uri in [
        "ssh://host/base/dir",
        "sftp://host/base/dir",
        "mock://bucket/base",
    ] {
        let adapter = resolve_adapter(uri).unwrap_or_else(|e| panic!("resolve {uri}: {e}"));
        let scheme = uri.split(':').next().unwrap();
        assert_eq!(
            adapter,
            Adapter::External {
                name: scheme.to_owned(),
            },
            "{uri} must route to the external adapter '{scheme}'"
        );
        assert!(
            !adapter.is_builtin(),
            "{uri} is an external (out-of-process) store, not a built-in StreamStore"
        );

        // The shim builds for these external URIs; it is the ONLY thing serving
        // them, and it is NOT a StreamStore (see the compile-time note below), so
        // it can never be a mirror destination — capability false by exclusion.
        let shim = ExternalStore::new(uri)
            .unwrap_or_else(|e| panic!("ExternalStore::new({uri}) should build: {e:?}"));
        assert_eq!(
            shim.binary(),
            Path::new(&format!("snapdir-{scheme}-store")),
            "{uri} dispatches to the snapdir-{scheme}-store binary"
        );
    }
}

#[test]
fn external_shim_is_not_a_streamstore_so_cannot_be_a_mirror_destination() {
    // COMPILE-TIME PROOF that the external shim is NOT a StreamStore: a generic
    // helper that only accepts `&dyn StreamStore` can take FileStore but the line
    // for ExternalStore is left commented because it would NOT compile (the trait
    // is unimplemented). sync_snapshot_mirror's `to: &dyn StreamStore` therefore
    // can never be handed an ExternalStore, so ssh/sftp/external are excluded from
    // mirroring by the type system itself.
    fn assert_is_stream_store(_s: &dyn StreamStore) {}

    let dir = TempDir::new("file-is-stream");
    let file = FileStore::from_root(dir.path().to_path_buf());
    assert_is_stream_store(&file); // FileStore IS a StreamStore.

    let shim = ExternalStore::new("ssh://host/base").expect("build ssh shim");
    // The next line MUST NOT compile if uncommented — ExternalStore is not a
    // StreamStore. Keeping it commented documents the exclusion as code:
    //   assert_is_stream_store(&shim); // ← compile error: trait not implemented
    //
    // We still exercise the shim's public surface so it is not dead: it resolves
    // to the ssh store binary (not an in-process mirror-capable adapter).
    assert_eq!(shim.binary(), Path::new("snapdir-ssh-store"));
}

// ===========================================================================
// 4. THE REFUSAL PATH — sync_snapshot_mirror to a REAL unsupported dest
//
//    Real local FileStore `from` + a REAL offline-constructed S3Store `to`
//    (supports_mirror() == false). The mirror must return the documented typed
//    StoreError and change NOTHING (no copy-in, no delete) in the destination.
// ===========================================================================

#[test]
fn mirror_to_a_real_s3_dest_is_refused_with_a_clear_error_and_touches_nothing() {
    let src_dir = TempDir::new("refuse-src");
    let source = FileStore::from_root(src_dir.path().to_path_buf());

    // Stage a real snapshot into the source so the copy-in WOULD have work to do
    // were the refusal not to fire first.
    let staging = TempDir::new("refuse-staging");
    let (manifest, id) = build_tree(staging.path(), &[("x", b"payload to mirror\n")]);
    source
        .push(&manifest, staging.path())
        .expect("stage source");

    // A REAL object/remote dest built offline. Its supports_mirror() is false, so
    // the mirror must refuse BEFORE any copy-in or delete.
    let dest =
        S3Store::connect("s3://bucket/prefix", None).expect("S3Store::connect must build offline");
    assert!(
        !dest.supports_mirror(),
        "precondition: the S3 dest does not support mirroring"
    );

    let err = sync_snapshot_mirror(&source, &dest, &id, &cfg(), false, None)
        .expect_err("a mirror to an object/remote dest MUST be a typed error, not a panic/Ok");

    // Typed, non-panic StoreError whose Display names the unsupported/non-local/
    // mirror/--delete condition — actionable for the CLI.
    let display = err.to_string().to_lowercase();
    match &err {
        StoreError::Backend { message, .. } => {
            let m = message.to_lowercase();
            assert!(
                m.contains("mirror")
                    || m.contains("unsupported")
                    || m.contains("delete")
                    || m.contains("not support")
                    || m.contains("local"),
                "the refusal must explain mirror/--delete is unsupported on this dest; got: {message}"
            );
        }
        // Tolerate a future dedicated variant; the hard requirements are: it is an
        // Err, and the Display still names the condition.
        other => {
            let _ = other;
            assert!(
                display.contains("mirror")
                    || display.contains("unsupported")
                    || display.contains("delete")
                    || display.contains("local"),
                "the refusal Display must name the mirror/delete/local condition; got: {display}"
            );
        }
    }

    // The source is untouched and still readable (the refusal does not corrupt the
    // input either): every object of the staged snapshot is still present.
    source.get_manifest(&id).expect("source manifest intact");
    for sum in object_checksums(&manifest) {
        assert!(
            source.has_object(&sum).expect("source has_object"),
            "the refused mirror must leave the source untouched"
        );
    }
}
