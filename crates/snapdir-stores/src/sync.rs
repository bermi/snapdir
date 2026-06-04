//! Streaming store-to-store snapshot copy.
//!
//! [`sync_snapshot`] copies ONE snapshot — its manifest plus every raw object
//! it references — directly from a source [`StreamStore`] to a destination
//! [`StreamStore`], **through memory only**. There is no local filesystem
//! staging: the function signature deliberately takes **no
//! [`Path`](std::path::Path)** anywhere, so a blob read out of the source can
//! only ever flow into the destination (it never touches scratch/cache on disk).
//!
//! # Sync methods, rayon threads — not the async driver
//!
//! [`StreamStore`]'s methods are **synchronous**: the network backends drive
//! their async SDK calls on an internal runtime via `block_on`. Driving them
//! from the async [`run_concurrent`](crate::transfer::run_concurrent) /
//! [`RateLimiter`](crate::transfer::RateLimiter) would nest one tokio runtime
//! inside another and panic. So this orchestrator parallelizes object copies
//! across a **rayon [`ThreadPool`](rayon::ThreadPool)** sized to
//! [`TransferConfig::concurrency`] — exactly the pattern
//! [`FileStore::parallel_copy`](crate::file_store::FileStore). Rayon workers are
//! plain OS threads, so each one may safely call the `block_on`-ing sync
//! `get_object`/`put_object`. Bandwidth is throttled by the **synchronous**
//! [`BlockingRateLimiter`] (one shared bucket via [`Arc`]), never the async
//! [`RateLimiter`](crate::transfer::RateLimiter).
//!
//! # Invariants
//!
//! - **Skip-present / incremental:** an object the destination already
//!   [`has_object`](StreamStore::has_object) is not re-copied.
//! - **Manifest-last / all-or-nothing:** the destination manifest is written
//!   only after every referenced object has landed. On the first object error
//!   the copy stops and NO manifest is written, so a destination manifest always
//!   implies its objects are present (mirroring
//!   [`push`](snapdir_core::store::Store::push)).
//! - **Verified:** every blob is BLAKE3-verified by the underlying
//!   [`StreamStore`] on both read and write, and the source manifest is verified
//!   to hash to `id` by [`get_manifest`](snapdir_core::store::Store::get_manifest).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use snapdir_core::manifest::PathType;
use snapdir_core::store::StoreError;
use snapdir_core::{Meter, Phase};

use crate::stream::StreamStore;
use crate::transfer::{BlockingRateLimiter, TransferConfig};

/// Outcome of a [`sync_snapshot`] call.
///
/// When `dry_run` is `true`, `objects_copied` is the number of objects that
/// *would* be copied (those absent from the destination) and `bytes_copied`
/// stays `0` — nothing is read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Objects actually copied source → dest (or, in a dry run, that would be
    /// copied).
    pub objects_copied: usize,
    /// Objects skipped because the destination already held them.
    pub objects_skipped: usize,
    /// Total bytes copied into the destination (always `0` for a dry run).
    pub bytes_copied: u64,
    /// Whether this was a dry run (no reads, writes, or manifest).
    pub dry_run: bool,
}

/// Copies one snapshot's manifest + raw objects directly from `from` to `to`,
/// through memory only (no local filesystem staging).
///
/// See the [module docs](crate::sync) for the rayon-pool / sync-rate-limiter
/// design and the manifest-last invariant. The function takes **no
/// [`Path`](std::path::Path)** — that is the structural guarantee nothing is
/// staged on local disk.
///
/// # Fast path
///
/// If the destination already has the manifest for `id`, the snapshot is fully
/// mirrored and a zero-transfer [`SyncReport`] is returned without touching the
/// source's objects.
///
/// # Errors
///
/// Returns the first [`StoreError`] from any manifest/object operation. On an
/// object error NO destination manifest is written.
pub fn sync_snapshot(
    from: &(dyn StreamStore + Sync),
    to: &(dyn StreamStore + Sync),
    id: &str,
    config: &TransferConfig,
    dry_run: bool,
    meter: Option<&Meter>,
) -> Result<SyncReport, StoreError> {
    // Fast path: a destination manifest implies all its objects are present, so
    // an already-mirrored snapshot needs no work (and no source reads).
    if to.get_manifest(id).is_ok() {
        return Ok(SyncReport {
            objects_copied: 0,
            objects_skipped: 0,
            bytes_copied: 0,
            dry_run,
        });
    }

    // Verifies the source manifest hashes back to `id` before we trust it.
    let manifest = from.get_manifest(id)?;

    // Sync moves content objects, not the directory tree, so only File entries
    // carry an object to copy.
    let files: Vec<&str> = manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .map(|e| e.checksum.as_str())
        .collect();

    // Advisory progress: we are entering the transfer phase, and the total is
    // the sum of the to-copy object sizes (the File entries' manifest sizes).
    // No effect on what is copied; a no-op without a meter.
    if let Some(m) = meter {
        m.set_phase(Phase::Transfer);
        let total: u64 = manifest
            .entries()
            .iter()
            .filter(|e| e.path_type == PathType::File)
            .map(|e| e.size)
            .sum();
        m.set_total(total);
    }

    let copied = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let bytes = AtomicU64::new(0);

    // One shared synchronous token bucket across all rayon workers (Arc so the
    // closure can be Sync/shared). Unlimited when max_bytes_per_sec is None/0.
    let limiter = Arc::new(BlockingRateLimiter::new(config.max_bytes_per_sec));

    if !files.is_empty() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(config.concurrency.get())
            .build()
            .map_err(|err| StoreError::Backend {
                message: "failed to build sync thread pool".to_owned(),
                source: Some(Box::new(err)),
            })?;

        pool.install(|| {
            use rayon::prelude::*;
            files.par_iter().try_for_each(|checksum| {
                if to.has_object(checksum)? {
                    skipped.fetch_add(1, Ordering::Relaxed);
                    if let Some(m) = meter {
                        m.add_skipped(1);
                    }
                    return Ok(());
                }
                if dry_run {
                    // Count as "would copy"; never read or write anything.
                    copied.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
                // Bytes live only in memory: read from source, throttle, write
                // to dest. Never written to any path.
                if let Some(m) = meter {
                    m.object_started();
                }
                let blob = from.get_object(checksum)?;
                let len = blob.len() as u64;
                // Read from source (bytes-in).
                if let Some(m) = meter {
                    m.add_in(len);
                }
                limiter.acquire_blocking(len);
                to.put_object(checksum, blob)?;
                // Written to dest (bytes-out), object done.
                if let Some(m) = meter {
                    m.add_out(len);
                    m.object_finished();
                }
                copied.fetch_add(1, Ordering::Relaxed);
                bytes.fetch_add(len, Ordering::Relaxed);
                Ok::<(), StoreError>(())
            })
        })?;
    }

    // Manifest-last / all-or-nothing: only after every object copy succeeded
    // (and never in a dry run) do we write the destination manifest, so a
    // present manifest always implies present objects.
    if !dry_run {
        to.put_manifest(id, &manifest)?;
    }

    Ok(SyncReport {
        objects_copied: copied.into_inner(),
        objects_skipped: skipped.into_inner(),
        bytes_copied: bytes.into_inner(),
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_store::FileStore;
    use snapdir_core::manifest::{Manifest, ManifestEntry};
    use snapdir_core::merkle::{Blake3Hasher, Hasher};
    use snapdir_core::store::Store;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// A tiny temp-dir helper so tests don't pull in a dev-dependency. Creates a
    /// unique directory under the system temp dir and removes it on drop.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::AtomicU64;
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "snapdir-sync-test-{}-{tag}-{n}",
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

    /// Builds a small multi-file source tree (`a`, `b`, `c`) under `source` and
    /// returns its manifest + snapshot id. Checksums are the real BLAKE3 of the
    /// file bytes so store verification passes.
    fn make_source(source: &Path) -> (Manifest, String) {
        let hasher = Blake3Hasher::new();
        let files: [(&str, &[u8]); 3] = [("a", b"alpha\n"), ("b", b"bravo\n"), ("c", b"charlie\n")];
        let mut sums: Vec<(String, String, u64)> = Vec::new();
        for (name, bytes) in files {
            fs::write(source.join(name), bytes).unwrap();
            sums.push((
                (*name).to_owned(),
                hasher.hash_hex(bytes),
                bytes.len() as u64,
            ));
        }
        let root_sum = snapdir_core::merkle::directory_checksum(
            sums.iter().map(|(_, s, _)| s.as_str()),
            &hasher,
        );

        let mut entries = vec![ManifestEntry::new(
            PathType::Directory,
            "700",
            root_sum,
            0,
            "./",
        )];
        for (name, sum, size) in &sums {
            entries.push(ManifestEntry::new(
                PathType::File,
                "600",
                sum.clone(),
                *size,
                format!("./{name}"),
            ));
        }
        let manifest = Manifest::from_entries(entries);
        let id = snapdir_core::merkle::snapshot_id(&manifest, &hasher);
        (manifest, id)
    }

    /// The number of File objects in `manifest`.
    fn object_count(manifest: &Manifest) -> usize {
        manifest
            .entries()
            .iter()
            .filter(|e| e.path_type == PathType::File)
            .count()
    }

    fn cfg() -> TransferConfig {
        TransferConfig::new(4, None)
    }

    #[test]
    fn sync_snapshot_mirrors_snapshot() {
        let a_dir = TempDir::new("a");
        let b_dir = TempDir::new("b");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_source(src_dir.path());
        let n = object_count(&manifest);

        let a = FileStore::from_root(a_dir.path());
        let b = FileStore::from_root(b_dir.path());
        a.push(&manifest, src_dir.path()).expect("stage into A");

        let report = sync_snapshot(&a, &b, &id, &cfg(), false, None).expect("sync ok");

        assert_eq!(report.objects_copied, n);
        assert_eq!(report.objects_skipped, 0);
        assert!(!report.dry_run);
        // B has the manifest and every object.
        b.get_manifest(&id).expect("B has manifest");
        for entry in manifest.entries() {
            if entry.path_type == PathType::File {
                assert!(
                    b.has_object(&entry.checksum).expect("has_object ok"),
                    "B missing object {}",
                    entry.checksum
                );
            }
        }
    }

    #[test]
    fn meter_records_sync() {
        // A multi-object snapshot synced A -> empty B records bytes-in ==
        // bytes-out == total object bytes, objects_done == N, skipped == 0; a
        // second sync into the now-populated B records the fast-path /
        // skip-everything outcome (no copies).
        let a_dir = TempDir::new("a");
        let b_dir = TempDir::new("b");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_source(src_dir.path());
        let n = object_count(&manifest);

        // Total File-object bytes from the manifest sizes.
        let total_bytes: u64 = manifest
            .entries()
            .iter()
            .filter(|e| e.path_type == PathType::File)
            .map(|e| e.size)
            .sum();

        let a = FileStore::from_root(a_dir.path());
        let b = FileStore::from_root(b_dir.path());
        a.push(&manifest, src_dir.path()).expect("stage into A");

        let meter = Arc::new(Meter::new());
        let report =
            sync_snapshot(&a, &b, &id, &cfg(), false, Some(&meter)).expect("first meter sync");
        assert_eq!(report.objects_copied, n);

        let snap = meter.snapshot();
        assert_eq!(snap.bytes_in, total_bytes, "bytes_in == total object bytes");
        assert_eq!(
            snap.bytes_out, total_bytes,
            "bytes_out == total object bytes"
        );
        assert_eq!(snap.objects_done, n as u64, "objects_done == N");
        assert_eq!(snap.objects_skipped, 0, "nothing skipped on a fresh dest");
        assert_eq!(snap.objects_total, total_bytes, "total == bytes total");
        assert_eq!(snap.in_flight, 0, "no objects left in flight");
        assert_eq!(snap.phase, Phase::Transfer, "phase set to Transfer");

        // Second sync into the now-fully-mirrored B. The fast path (dest has the
        // manifest) short-circuits, so this records no new copies. Pre-seed every
        // object into a fresh B' WITHOUT its manifest to exercise the per-object
        // skip branch and assert objects_skipped == N, objects_done == 0.
        let seed_dir = TempDir::new("seed");
        let seeded = FileStore::from_root(seed_dir.path());
        for entry in manifest.entries() {
            if entry.path_type == PathType::File {
                let blob = a.get_object(&entry.checksum).expect("get from A");
                seeded.put_object(&entry.checksum, blob).expect("seed dest");
            }
        }
        let later = Arc::new(Meter::new());
        let later_report = sync_snapshot(&a, &seeded, &id, &cfg(), false, Some(&later))
            .expect("second meter sync");
        assert_eq!(
            later_report.objects_skipped, n,
            "all objects already present"
        );
        let later_snap = later.snapshot();
        assert_eq!(later_snap.objects_skipped, n as u64, "meter skipped == N");
        assert_eq!(later_snap.objects_done, 0, "no objects copied");
        assert_eq!(later_snap.bytes_in, 0, "no bytes read");
        assert_eq!(later_snap.bytes_out, 0, "no bytes written");
    }

    #[test]
    fn sync_snapshot_skip_present_is_incremental() {
        let a_dir = TempDir::new("a");
        let b_dir = TempDir::new("b");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_source(src_dir.path());
        let n = object_count(&manifest);

        let a = FileStore::from_root(a_dir.path());
        let b = FileStore::from_root(b_dir.path());
        a.push(&manifest, src_dir.path()).expect("stage into A");

        let first = sync_snapshot(&a, &b, &id, &cfg(), false, None).expect("first sync");
        assert_eq!(first.objects_copied, n);

        // Second run: destination already mirrored → fast path returns a
        // zero-transfer report; B is unchanged.
        let second = sync_snapshot(&a, &b, &id, &cfg(), false, None).expect("second sync");
        assert_eq!(second.objects_copied, 0);
        assert_eq!(second.objects_skipped, 0);
        assert_eq!(second.bytes_copied, 0);
        b.get_manifest(&id).expect("B still has manifest");
    }

    #[test]
    fn sync_snapshot_skip_present_per_object() {
        // Pre-seed one object into B (but NOT B's manifest), so the fast path
        // does not trigger and we exercise the per-object skip branch.
        let a_dir = TempDir::new("a");
        let b_dir = TempDir::new("b");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_source(src_dir.path());
        let n = object_count(&manifest);

        let a = FileStore::from_root(a_dir.path());
        let b = FileStore::from_root(b_dir.path());
        a.push(&manifest, src_dir.path()).expect("stage into A");

        // Copy one object from A into B directly.
        let first_obj = manifest
            .entries()
            .iter()
            .find(|e| e.path_type == PathType::File)
            .unwrap();
        let blob = a.get_object(&first_obj.checksum).expect("get from A");
        b.put_object(&first_obj.checksum, blob).expect("seed B");

        let report = sync_snapshot(&a, &b, &id, &cfg(), false, None).expect("sync ok");
        assert_eq!(report.objects_copied, n - 1);
        assert_eq!(report.objects_skipped, 1);
        b.get_manifest(&id).expect("B has manifest after sync");
    }

    /// A dest store that wraps a [`FileStore`] but fails `put_object` for one
    /// chosen checksum, to drive the all-or-nothing path.
    struct FailingPutStore {
        inner: FileStore,
        fail_on: String,
        // Records which checksums were attempted, for sanity.
        attempted: Mutex<Vec<String>>,
    }

    impl Store for FailingPutStore {
        fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError> {
            self.inner.get_manifest(id)
        }
        fn fetch_files(&self, manifest: &Manifest, dest: &Path) -> Result<(), StoreError> {
            self.inner.fetch_files(manifest, dest)
        }
        fn push(&self, manifest: &Manifest, source: &Path) -> Result<(), StoreError> {
            self.inner.push(manifest, source)
        }
    }

    impl StreamStore for FailingPutStore {
        fn has_object(&self, checksum: &str) -> Result<bool, StoreError> {
            self.inner.has_object(checksum)
        }
        fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
            self.inner.get_object(checksum)
        }
        fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
            self.attempted.lock().unwrap().push(checksum.to_owned());
            if checksum == self.fail_on {
                return Err(StoreError::Backend {
                    message: "synthetic put_object failure".to_owned(),
                    source: None,
                });
            }
            self.inner.put_object(checksum, bytes)
        }
        fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
            self.inner.put_manifest(id, manifest)
        }
    }

    #[test]
    fn sync_snapshot_all_or_nothing() {
        let a_dir = TempDir::new("a");
        let b_dir = TempDir::new("b");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_source(src_dir.path());

        let a = FileStore::from_root(a_dir.path());
        a.push(&manifest, src_dir.path()).expect("stage into A");

        // Pick a checksum to fail on.
        let fail_on = manifest
            .entries()
            .iter()
            .find(|e| e.path_type == PathType::File)
            .unwrap()
            .checksum
            .clone();

        let b = FailingPutStore {
            inner: FileStore::from_root(b_dir.path()),
            fail_on,
            attempted: Mutex::new(Vec::new()),
        };

        // Concurrency 1 keeps the failure deterministic.
        let one = TransferConfig::new(1, None);
        let err =
            sync_snapshot(&a, &b, &id, &one, false, None).expect_err("must surface put error");
        assert!(
            matches!(err, StoreError::Backend { ref message, .. } if message.contains("synthetic")),
            "unexpected error: {err:?}"
        );
        // NO manifest written to the dest.
        assert!(
            b.get_manifest(&id).is_err(),
            "dest must have no manifest after a failed sync"
        );
    }

    #[test]
    fn sync_snapshot_dry_run_writes_nothing() {
        let a_dir = TempDir::new("a");
        let b_dir = TempDir::new("b");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_source(src_dir.path());
        let n = object_count(&manifest);

        let a = FileStore::from_root(a_dir.path());
        let b = FileStore::from_root(b_dir.path());
        a.push(&manifest, src_dir.path()).expect("stage into A");

        let report = sync_snapshot(&a, &b, &id, &cfg(), true, None).expect("dry run ok");
        assert!(report.dry_run);
        assert_eq!(report.objects_copied, n, "would-copy count is N");
        assert_eq!(report.objects_skipped, 0);
        assert_eq!(report.bytes_copied, 0);

        // B has NO manifest and NO objects.
        assert!(b.get_manifest(&id).is_err(), "dry run wrote a manifest");
        for entry in manifest.entries() {
            if entry.path_type == PathType::File {
                assert!(
                    !b.has_object(&entry.checksum).expect("has_object ok"),
                    "dry run wrote an object"
                );
            }
        }
    }

    #[test]
    fn sync_snapshot_no_local_fs() {
        // Hold A and B under one parent tempdir and assert sync creates NOTHING
        // outside A's and B's store dirs (no scratch/cache). The structural
        // guarantee is that sync_snapshot takes no &Path; this test backs it up.
        let parent = TempDir::new("parent");
        let a_root = parent.path().join("store-a");
        let b_root = parent.path().join("store-b");
        let src = parent.path().join("src");
        fs::create_dir_all(&a_root).unwrap();
        fs::create_dir_all(&b_root).unwrap();
        fs::create_dir_all(&src).unwrap();

        let (manifest, id) = make_source(&src);

        let a = FileStore::from_root(&a_root);
        let b = FileStore::from_root(&b_root);
        a.push(&manifest, &src).expect("stage into A");

        // Snapshot the set of top-level entries under parent before sync.
        let before: std::collections::BTreeSet<PathBuf> = fs::read_dir(parent.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();

        sync_snapshot(&a, &b, &id, &cfg(), false, None).expect("sync ok");

        let after: std::collections::BTreeSet<PathBuf> = fs::read_dir(parent.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();

        assert_eq!(
            before,
            after,
            "sync_snapshot created an entry outside the store dirs: {:?}",
            after.difference(&before).collect::<Vec<_>>()
        );
    }
}
