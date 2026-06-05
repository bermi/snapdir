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

use std::sync::Arc;

use snapdir_core::manifest::{Manifest, PathType};
use snapdir_core::merkle::{Blake3Hasher, Hasher};
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};
use snapdir_core::Meter;

use crate::adaptive::{
    p95_object_size, AdaptiveGate, AdaptivePolicy as ControllerPolicy, ControllerDriver, OpResult,
    OpSample,
};
use crate::stream::StreamStore;
use crate::transfer::{classify_error, AdaptivePolicy, TransferConfig};
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
    config: TransferConfig,
    /// Optional progress meter; recorded into during transfers. `None` (the
    /// default from every constructor) means zero recording and byte-identical
    /// behavior. Set by the CLI via [`FileStore::with_meter`].
    meter: Option<Arc<Meter>>,
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

    /// Like [`new`](Self::new), but carries a [`TransferConfig`] for
    /// concurrency / bandwidth control.
    #[must_use]
    pub fn new_with_config(store: &str, config: TransferConfig) -> Self {
        Self::from_root_with_config(parse_store_dir(store), config)
    }

    /// Builds a store rooted at an already-resolved directory.
    #[must_use]
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self::from_root_with_config(root, TransferConfig::default())
    }

    /// Like [`from_root`](Self::from_root), but carries a [`TransferConfig`] for
    /// concurrency / bandwidth control. [`from_root`](Self::from_root) and
    /// [`new`](Self::new) delegate here with [`TransferConfig::default`].
    #[must_use]
    pub fn from_root_with_config(root: impl Into<PathBuf>, config: TransferConfig) -> Self {
        Self {
            root: root.into(),
            config,
            meter: None,
        }
    }

    /// Attaches (or clears) an optional progress [`Meter`], rides alongside
    /// [`config`](Self::transfer_config). The copy paths record bytes-in /
    /// bytes-out + per-object progress into it; `None` (the constructor default)
    /// means zero recording and byte-identical behavior. The CLI sets this after
    /// construction.
    #[must_use]
    pub fn with_meter(mut self, meter: Option<Arc<Meter>>) -> Self {
        self.meter = meter;
        self
    }

    /// Returns the store's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The [`TransferConfig`] (concurrency / bandwidth) this store was built
    /// with. Consumed by the transfer loops in later gates.
    #[must_use]
    pub fn transfer_config(&self) -> &TransferConfig {
        &self.config
    }

    /// Absolute on-disk path of an object given its checksum.
    fn object_disk_path(&self, checksum: &str) -> PathBuf {
        self.root.join(object_path(checksum))
    }

    /// Absolute on-disk path of a manifest given its snapshot id.
    fn manifest_disk_path(&self, id: &str) -> PathBuf {
        self.root.join(manifest_path(id))
    }

    /// Copies a batch of `(source, target, expected_checksum)` jobs through
    /// [`persist`] across a thread pool bounded by `self.config.concurrency`.
    ///
    /// Local copies have no network bandwidth concern, so the async
    /// rate-limited transfer driver does not apply here — only the concurrency
    /// cap. The first [`StoreError`] is propagated and stops scheduling further
    /// work (`try_for_each`). A `concurrency` of 1 yields a single-threaded
    /// sequential copy. Each task uses a fresh, cheap, stateless
    /// [`Blake3Hasher`] to sidestep any `Sync` concern.
    fn parallel_copy(&self, jobs: &[(PathBuf, PathBuf, String)]) -> Result<(), StoreError> {
        if jobs.is_empty() {
            return Ok(());
        }
        match self.config.adaptive {
            AdaptivePolicy::Off => self.parallel_copy_fixed(jobs),
            AdaptivePolicy::On { fraction, ceiling } => {
                self.parallel_copy_adaptive(jobs, fraction, ceiling)
            }
        }
    }

    /// Fixed-concurrency rayon copy: pool sized to `config.concurrency`, no gate
    /// or driver. The historical (byte-identical) local-copy path.
    fn parallel_copy_fixed(&self, jobs: &[(PathBuf, PathBuf, String)]) -> Result<(), StoreError> {
        use rayon::prelude::*;

        // `&Meter` is `Sync`, so it is shared across the rayon closures. `None`
        // means zero recording and byte-identical behavior.
        let meter = self.meter.as_deref();

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.config.concurrency.get())
            .build()
            .map_err(|err| StoreError::Backend {
                message: "failed to build copy thread pool".to_owned(),
                source: Some(Box::new(err)),
            })?;

        pool.install(|| {
            jobs.par_iter().try_for_each(|(source, target, expected)| {
                if let Some(m) = meter {
                    m.object_started();
                }
                // `persist` reads `source` and writes `target`. Record the
                // source size as both bytes-in (read) and bytes-out (written);
                // a missing source surfaces as the persist error below.
                let len = std::fs::metadata(source).map_or(0, |md| md.len());
                persist(source, target, expected, &Blake3Hasher::new())?;
                if let Some(m) = meter {
                    m.add_in(len);
                    m.add_out(len);
                    m.object_finished();
                }
                Ok(())
            })
        })
    }

    /// Adaptive rayon copy: pool sized to the policy `ceiling`, each job gated to
    /// the controller's live limit (effective concurrency ≤ ceiling), every copy
    /// timed + classified + recorded, with a background `std::thread` ticking the
    /// controller (~250ms) to resize the gate. Local FS has no network rate, so
    /// the controller drives concurrency only (no rate applier).
    ///
    /// The exact objects copied and the first-error-wins (`try_for_each`)
    /// semantics are identical to [`parallel_copy_fixed`]; only scheduling
    /// differs.
    fn parallel_copy_adaptive(
        &self,
        jobs: &[(PathBuf, PathBuf, String)],
        fraction: f64,
        ceiling: usize,
    ) -> Result<(), StoreError> {
        use rayon::prelude::*;

        let meter = self.meter.as_deref();

        let sizes: Vec<u64> = jobs
            .iter()
            .map(|(source, _, _)| std::fs::metadata(source).map_or(0, |md| md.len()))
            .collect();
        let p95 = p95_object_size(&sizes);
        let total_ram = snapdir_core::resources::total_ram_bytes().unwrap_or(0);
        let policy = ControllerPolicy::new(fraction, ceiling, total_ram, None);

        let gate = AdaptiveGate::new(self.config.concurrency.get(), ceiling);
        // No rate limiter for local copies (concurrency-only control).
        let driver = ControllerDriver::new(policy, gate.clone(), p95, None, self.meter.clone());

        // Background tick thread: stop it on the shared flag once the copy ends.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tick_driver = driver.clone();
        let tick_stop = Arc::clone(&stop);
        let ticker = std::thread::spawn(move || {
            while !tick_stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if tick_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                tick_driver.tick();
            }
        });

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(ceiling.max(1))
            .build()
            .map_err(|err| StoreError::Backend {
                message: "failed to build copy thread pool".to_owned(),
                source: Some(Box::new(err)),
            })?;

        let result = pool.install(|| {
            jobs.par_iter().try_for_each(|(source, target, expected)| {
                // Gate to the controller's live limit (effective concurrency).
                let _permit = gate.acquire_blocking();
                if let Some(m) = meter {
                    m.object_started();
                }
                let len = std::fs::metadata(source).map_or(0, |md| md.len());
                let started = std::time::Instant::now();
                let outcome = persist(source, target, expected, &Blake3Hasher::new());
                let latency = started.elapsed();
                let (bytes, op_result) = match &outcome {
                    Ok(()) => (len, OpResult::Ok),
                    Err(err) => (0, classify_error(err)),
                };
                driver.record_op(OpSample {
                    bytes,
                    latency,
                    result: op_result,
                });
                outcome?;
                if let Some(m) = meter {
                    m.add_in(len);
                    m.add_out(len);
                    m.object_finished();
                }
                Ok(())
            })
        });

        // Stop the tick thread and join it before returning.
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = ticker.join();
        result
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

        // First, SEQUENTIAL pass: materialize every directory and pre-create
        // each file's parent (so the parallel copies below never race on
        // `create_dir_all` of the same ancestor), short-circuit files that are
        // already present-and-verified (skip-if-present-and-verified — no object
        // read at all, so a populated dest succeeds even if the store object is
        // gone), and confirm the source object exists for the rest (preserving
        // the `ObjectNotFound` error when a needed source is missing). The file
        // entries that actually need copying are collected as `(source, target,
        // checksum)` jobs for the parallel phase.
        let mut jobs: Vec<(PathBuf, PathBuf, String)> = Vec::new();
        for entry in manifest.entries() {
            let rel = strip_leading_dot_slash(&entry.path);
            let target = dest.join(rel);
            match entry.path_type {
                PathType::Directory => {
                    fs::create_dir_all(&target)?;
                }
                PathType::File => {
                    // A destination file that already exists and whose content
                    // hashes to the manifest's checksum needs no copy. A
                    // mismatching/corrupt local file falls through and is
                    // repaired by the persist below.
                    if file_present_and_verified(&target, &entry.checksum, &hasher) {
                        // Skip-present: record it as skipped (advisory only).
                        if let Some(m) = self.meter.as_deref() {
                            m.add_skipped(1);
                        }
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
                    jobs.push((source, target, entry.checksum.clone()));
                }
            }
        }

        // Total to copy (bytes over the to-copy set), recorded so the bar can
        // track bytes. Advisory: no effect on what is copied. No-op w/o meter.
        if let Some(m) = self.meter.as_deref() {
            let total: u64 = jobs
                .iter()
                .map(|(source, _, _)| fs::metadata(source).map_or(0, |md| md.len()))
                .sum();
            m.set_total(total);
        }

        // Parallel copy phase, bounded by `config.concurrency`. `try_for_each`
        // propagates the first `StoreError` and stops scheduling new work. Each
        // task uses a fresh, cheap, stateless `Blake3Hasher`.
        self.parallel_copy(&jobs)
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

        // Collect every referenced object that is absent (skip-if-present per
        // object: an object already filed under its content address is trusted,
        // it is content-addressable). These are copied BEFORE the manifest.
        let mut jobs: Vec<(PathBuf, PathBuf, String)> = Vec::new();
        for entry in manifest.entries() {
            if entry.path_type != PathType::File {
                continue;
            }
            let object_target = self.object_disk_path(&entry.checksum);
            if object_target.exists() {
                // Skip-present per object: record it as skipped (advisory only).
                if let Some(m) = self.meter.as_deref() {
                    m.add_skipped(1);
                }
                continue;
            }
            let rel = strip_leading_dot_slash(&entry.path);
            let object_source = source.join(rel);
            jobs.push((object_source, object_target, entry.checksum.clone()));
        }

        // Total to push (bytes over the to-push set), recorded so the bar can
        // track bytes. Advisory: no effect on what is pushed. No-op w/o meter.
        if let Some(m) = self.meter.as_deref() {
            let total: u64 = jobs
                .iter()
                .map(|(src, _, _)| fs::metadata(src).map_or(0, |md| md.len()))
                .sum();
            m.set_total(total);
        }

        // Parallel copy phase, bounded by `config.concurrency`. ALL-OR-NOTHING:
        // any error returns immediately and NO manifest is written; the
        // manifest is written only after every object copy succeeds, preserving
        // the invariant that a present manifest implies present objects.
        self.parallel_copy(&jobs)?;

        // Write the manifest last, via the same verify/retry/atomic-rename
        // path, so a present manifest always implies present objects.
        write_manifest(manifest, &manifest_target, &id, &hasher)?;
        Ok(())
    }
}

impl StreamStore for FileStore {
    fn has_object(&self, checksum: &str) -> Result<bool, StoreError> {
        Ok(self.object_disk_path(checksum).exists())
    }

    fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.object_disk_path(checksum);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::ObjectNotFound {
                    checksum: checksum.to_owned(),
                });
            }
            Err(err) => return Err(StoreError::Io(err)),
        };

        // Verify the stored blob hashes back to its content-address before
        // returning it — corruption must surface as `Integrity`, never as bad
        // bytes handed to a store-to-store copy.
        let actual = Blake3Hasher::new().hash_hex(&bytes);
        if actual != checksum {
            return Err(StoreError::Integrity {
                address: path.display().to_string(),
                expected: checksum.to_owned(),
                actual,
            });
        }
        Ok(bytes)
    }

    fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        // Verify BEFORE writing: a blob whose bytes do not hash to `checksum`
        // must never land at that content-address (nothing is stored).
        let actual = Blake3Hasher::new().hash_hex(&bytes);
        if actual != checksum {
            return Err(StoreError::Integrity {
                address: object_path(checksum),
                expected: checksum.to_owned(),
                actual,
            });
        }

        let target = self.object_disk_path(checksum);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // Temp sibling + atomic rename, the same write discipline `persist`
        // uses, so a partially-written object is never visible at its address.
        let tmp = temp_sibling(&target);
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &target)?;
        Ok(())
    }

    fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        write_manifest(
            manifest,
            &self.manifest_disk_path(id),
            id,
            &Blake3Hasher::new(),
        )
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

    /// Builds a small nested tree under `source` (several files across nested
    /// directories) and returns its manifest + snapshot id, with real BLAKE3
    /// checksums so store verification passes. Layout:
    ///
    /// ```text
    /// ./a.txt            "a contents\n"
    /// ./b.txt            "b contents\n"
    /// ./sub/             (dir)
    /// ./sub/c.txt        "c contents\n"
    /// ./sub/deep/        (dir)
    /// ./sub/deep/d.txt   "d contents\n"
    /// ```
    fn make_nested_source(source: &Path) -> (Manifest, String) {
        let hasher = Blake3Hasher::new();
        let files: &[(&str, &[u8])] = &[
            ("a.txt", b"a contents\n"),
            ("b.txt", b"b contents\n"),
            ("sub/c.txt", b"c contents\n"),
            ("sub/deep/d.txt", b"d contents\n"),
        ];

        fs::create_dir_all(source.join("sub/deep")).unwrap();
        for (rel, bytes) in files {
            fs::write(source.join(rel), bytes).unwrap();
        }

        let mut manifest = Manifest::new();
        // Directory entries first; their checksums/sizes are not verified on
        // fetch (only files are content-addressed), so placeholder values are
        // fine for re-materialization. The snapshot id derivation in core hashes
        // the rendered text regardless, and we round-trip through it below.
        manifest.push(ManifestEntry::new(PathType::Directory, "700", "x", 0, "./"));
        manifest.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            "x",
            0,
            "./sub/",
        ));
        manifest.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            "x",
            0,
            "./sub/deep/",
        ));
        for (rel, bytes) in files {
            let sum = hasher.hash_hex(bytes);
            #[allow(clippy::cast_possible_truncation)]
            manifest.push(ManifestEntry::new(
                PathType::File,
                "600",
                sum,
                bytes.len() as u64,
                format!("./{rel}"),
            ));
        }

        let manifest = Manifest::from_entries(manifest.entries().to_vec());
        let id = snapdir_core::merkle::snapshot_id(&manifest, &hasher);
        (manifest, id)
    }

    /// Asserts the four nested files re-materialized byte-identically at `dest`.
    fn assert_nested_dest(dest: &Path) {
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"a contents\n");
        assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"b contents\n");
        assert_eq!(fs::read(dest.join("sub/c.txt")).unwrap(), b"c contents\n");
        assert_eq!(
            fs::read(dest.join("sub/deep/d.txt")).unwrap(),
            b"d contents\n"
        );
    }

    #[test]
    fn filestore_parallel_roundtrip_byte_identical() {
        // A multi-threaded (concurrency=4) push+fetch round-trip of a nested
        // tree must re-materialize byte-identically, and a sequential
        // (concurrency=1) run must produce the identical store + dest.
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_nested_source(src_dir.path());

        // Parallel run.
        let par_store_dir = TempDir::new("store-par");
        let par_dest_dir = TempDir::new("dest-par");
        let par_store =
            FileStore::from_root_with_config(par_store_dir.path(), TransferConfig::new(4, None));
        par_store.push(&manifest, src_dir.path()).expect("par push");
        let par_manifest = par_store.get_manifest(&id).expect("par get manifest");
        assert_eq!(par_manifest, manifest, "round-tripped manifest matches");
        par_store
            .fetch_files(&par_manifest, par_dest_dir.path())
            .expect("par fetch");
        assert_nested_dest(par_dest_dir.path());

        // Sequential run into a fresh store/dest.
        let seq_store_dir = TempDir::new("store-seq");
        let seq_dest_dir = TempDir::new("dest-seq");
        let seq_store =
            FileStore::from_root_with_config(seq_store_dir.path(), TransferConfig::new(1, None));
        seq_store.push(&manifest, src_dir.path()).expect("seq push");
        let seq_id = snapdir_core::merkle::snapshot_id(&manifest, &Blake3Hasher::new());
        assert_eq!(seq_id, id, "snapshot id is concurrency-independent");
        seq_store
            .fetch_files(&manifest, seq_dest_dir.path())
            .expect("seq fetch");
        assert_nested_dest(seq_dest_dir.path());

        // Both stores landed every object at the identical sharded key with
        // identical bytes.
        for entry in manifest.entries() {
            if entry.path_type != PathType::File {
                continue;
            }
            let key = object_path(&entry.checksum);
            let par_obj = par_store_dir.path().join(&key);
            let seq_obj = seq_store_dir.path().join(&key);
            assert!(par_obj.exists(), "par object {key} present");
            assert!(seq_obj.exists(), "seq object {key} present");
            assert_eq!(
                fs::read(&par_obj).unwrap(),
                fs::read(&seq_obj).unwrap(),
                "par and seq object bytes identical"
            );
        }
    }

    #[test]
    fn filestore_adaptive_push_fetch_same_snapshot_id_and_bytes() {
        // INVARIANT: an adaptive (policy On, low ceiling) push+fetch produces a
        // byte-identical store + dest and re-ids to the SAME snapshot id as the
        // non-adaptive (Off) path over the same input. Adaptive only changes
        // scheduling, never what is transferred.
        use crate::transfer::AdaptivePolicy;

        let src_dir = TempDir::new("src");
        let (manifest, id) = make_nested_source(src_dir.path());

        // Off path.
        let off_store_dir = TempDir::new("store-off");
        let off_dest_dir = TempDir::new("dest-off");
        let off =
            FileStore::from_root_with_config(off_store_dir.path(), TransferConfig::new(4, None));
        off.push(&manifest, src_dir.path()).expect("off push");
        let off_manifest = off.get_manifest(&id).expect("off manifest");
        off.fetch_files(&off_manifest, off_dest_dir.path())
            .expect("off fetch");

        // Adaptive path (low ceiling = 2).
        let on_store_dir = TempDir::new("store-on");
        let on_dest_dir = TempDir::new("dest-on");
        let on_cfg = TransferConfig::new(4, None).with_adaptive(AdaptivePolicy::On {
            fraction: 0.8,
            ceiling: 2,
        });
        let on = FileStore::from_root_with_config(on_store_dir.path(), on_cfg);
        on.push(&manifest, src_dir.path()).expect("adaptive push");
        let on_manifest = on.get_manifest(&id).expect("adaptive manifest");
        on.fetch_files(&on_manifest, on_dest_dir.path())
            .expect("adaptive fetch");

        // Same snapshot id, re-derived from the round-tripped manifest.
        let off_id = snapdir_core::merkle::snapshot_id(&off_manifest, &Blake3Hasher::new());
        let on_id = snapdir_core::merkle::snapshot_id(&on_manifest, &Blake3Hasher::new());
        assert_eq!(off_id, on_id, "adaptive must re-id to the same snapshot");
        assert_eq!(on_id, id, "and it matches the original id");
        assert_eq!(off_manifest, on_manifest, "round-tripped manifests equal");

        // Byte-identical objects at identical sharded keys + identical dest trees.
        for entry in manifest.entries() {
            if entry.path_type != PathType::File {
                continue;
            }
            let key = object_path(&entry.checksum);
            let off_obj = off_store_dir.path().join(&key);
            let on_obj = on_store_dir.path().join(&key);
            assert!(on_obj.exists(), "adaptive object {key} present");
            assert_eq!(
                fs::read(&off_obj).unwrap(),
                fs::read(&on_obj).unwrap(),
                "Off vs On object bytes identical for {key}"
            );
        }
        assert_nested_dest(off_dest_dir.path());
        assert_nested_dest(on_dest_dir.path());
    }

    #[test]
    fn filestore_parallel_concurrency_one_sequential() {
        // The concurrency=1 (single-thread pool) path is a correct sequential
        // copy: round-trips byte-identically.
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let dest_dir = TempDir::new("dest");
        let (manifest, id) = make_nested_source(src_dir.path());

        let store =
            FileStore::from_root_with_config(store_dir.path(), TransferConfig::new(1, None));
        store.push(&manifest, src_dir.path()).expect("push");
        let fetched = store.get_manifest(&id).expect("get manifest");
        store.fetch_files(&fetched, dest_dir.path()).expect("fetch");
        assert_nested_dest(dest_dir.path());
    }

    #[test]
    fn filestore_parallel_all_or_nothing_bad_object() {
        // A source file whose bytes do not match its manifest checksum must make
        // push fail with `Integrity` AND write NO manifest (all-or-nothing:
        // manifest is written only after every parallel object copy succeeds).
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_nested_source(src_dir.path());

        // Corrupt one source file so its bytes no longer hash to the manifest
        // checksum; persist's source-verify trips and push returns Integrity.
        fs::write(src_dir.path().join("sub/c.txt"), b"TAMPERED\n").unwrap();

        let store =
            FileStore::from_root_with_config(store_dir.path(), TransferConfig::new(4, None));
        match store.push(&manifest, src_dir.path()) {
            Err(StoreError::Integrity { .. }) => {}
            other => panic!("expected Integrity from bad source object, got {other:?}"),
        }

        // ALL-OR-NOTHING: the manifest must NOT have been written.
        let man_path = store.manifest_disk_path(&id);
        assert!(
            !man_path.exists(),
            "manifest must not be written when an object copy fails"
        );
    }

    #[test]
    fn filestore_parallel_large_n_round_trips() {
        // Exercise the concurrency bound with N >> concurrency files.
        let store_dir = TempDir::new("store");
        let src_dir = TempDir::new("src");
        let dest_dir = TempDir::new("dest");
        let hasher = Blake3Hasher::new();

        let mut manifest = Manifest::new();
        manifest.push(ManifestEntry::new(PathType::Directory, "700", "x", 0, "./"));
        let n = 50usize;
        for i in 0..n {
            let name = format!("file-{i:03}.txt");
            let contents = format!("contents of file {i}\n");
            fs::write(src_dir.path().join(&name), contents.as_bytes()).unwrap();
            let sum = hasher.hash_hex(contents.as_bytes());
            #[allow(clippy::cast_possible_truncation)]
            manifest.push(ManifestEntry::new(
                PathType::File,
                "600",
                sum,
                contents.len() as u64,
                format!("./{name}"),
            ));
        }
        let manifest = Manifest::from_entries(manifest.entries().to_vec());
        let id = snapdir_core::merkle::snapshot_id(&manifest, &hasher);

        let store =
            FileStore::from_root_with_config(store_dir.path(), TransferConfig::new(4, None));
        store.push(&manifest, src_dir.path()).expect("push N files");
        let fetched = store.get_manifest(&id).expect("get manifest");
        store
            .fetch_files(&fetched, dest_dir.path())
            .expect("fetch N files");

        for i in 0..n {
            let name = format!("file-{i:03}.txt");
            let expected = format!("contents of file {i}\n");
            assert_eq!(
                fs::read(dest_dir.path().join(&name)).unwrap(),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn meter_records_filestore_push_fetch() {
        // A FileStore wired `with_meter` doing push(manifest, src) then
        // fetch_files(manifest, dest) records add_in/add_out + objects_done
        // matching the object set. Push and fetch each touch every object once,
        // so over the two operations bytes_in/out == 2 * total bytes and
        // objects_done == 2 * N.
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_nested_source(src_dir.path());

        let n = manifest
            .entries()
            .iter()
            .filter(|e| e.path_type == PathType::File)
            .count() as u64;
        let total_bytes: u64 = manifest
            .entries()
            .iter()
            .filter(|e| e.path_type == PathType::File)
            .map(|e| e.size)
            .sum();

        let store_dir = TempDir::new("store");
        let dest_dir = TempDir::new("dest");
        let meter = Arc::new(Meter::new());
        let store = FileStore::from_root(store_dir.path()).with_meter(Some(Arc::clone(&meter)));

        store.push(&manifest, src_dir.path()).expect("push");
        let after_push = meter.snapshot();
        assert_eq!(after_push.bytes_in, total_bytes, "push read every object");
        assert_eq!(after_push.bytes_out, total_bytes, "push wrote every object");
        assert_eq!(after_push.objects_done, n, "push finished N objects");
        assert_eq!(after_push.objects_skipped, 0, "fresh store skips nothing");
        assert_eq!(after_push.objects_total, total_bytes, "push set byte total");
        assert_eq!(after_push.in_flight, 0, "nothing left in flight");

        let fetched = store.get_manifest(&id).expect("get manifest");
        store
            .fetch_files(&fetched, dest_dir.path())
            .expect("fetch_files");
        let after_fetch = meter.snapshot();
        assert_eq!(
            after_fetch.bytes_in,
            2 * total_bytes,
            "fetch read every object again"
        );
        assert_eq!(
            after_fetch.bytes_out,
            2 * total_bytes,
            "fetch wrote every object again"
        );
        assert_eq!(after_fetch.objects_done, 2 * n, "push + fetch = 2N objects");
        assert_eq!(after_fetch.in_flight, 0, "nothing left in flight");

        // Dest materialized correctly.
        assert_nested_dest(dest_dir.path());
    }

    #[test]
    fn meter_records_none_is_identical() {
        // The same push+fetch with NO meter produces byte-identical store/dest
        // contents and the same snapshot id as a metered run — recording changes
        // nothing.
        let src_dir = TempDir::new("src");
        let (manifest, id) = make_nested_source(src_dir.path());

        // Metered run.
        let metered_store_dir = TempDir::new("store-metered");
        let metered_dest_dir = TempDir::new("dest-metered");
        let meter = Arc::new(Meter::new());
        let metered =
            FileStore::from_root(metered_store_dir.path()).with_meter(Some(Arc::clone(&meter)));
        metered
            .push(&manifest, src_dir.path())
            .expect("metered push");
        let metered_id = snapdir_core::merkle::snapshot_id(&manifest, &Blake3Hasher::new());
        let metered_manifest = metered.get_manifest(&id).expect("metered manifest");
        metered
            .fetch_files(&metered_manifest, metered_dest_dir.path())
            .expect("metered fetch");

        // Unmetered run (meter is None — the constructor default).
        let plain_store_dir = TempDir::new("store-plain");
        let plain_dest_dir = TempDir::new("dest-plain");
        let plain = FileStore::from_root(plain_store_dir.path());
        plain.push(&manifest, src_dir.path()).expect("plain push");
        let plain_id = snapdir_core::merkle::snapshot_id(&manifest, &Blake3Hasher::new());
        let plain_manifest = plain.get_manifest(&id).expect("plain manifest");
        plain
            .fetch_files(&plain_manifest, plain_dest_dir.path())
            .expect("plain fetch");

        // Same snapshot id.
        assert_eq!(metered_id, plain_id, "snapshot id unaffected by the meter");
        assert_eq!(metered_id, id);

        // Byte-identical objects at identical sharded keys.
        for entry in manifest.entries() {
            if entry.path_type != PathType::File {
                continue;
            }
            let key = object_path(&entry.checksum);
            let metered_obj = metered_store_dir.path().join(&key);
            let plain_obj = plain_store_dir.path().join(&key);
            assert!(metered_obj.exists(), "metered object {key} present");
            assert!(plain_obj.exists(), "plain object {key} present");
            assert_eq!(
                fs::read(&metered_obj).unwrap(),
                fs::read(&plain_obj).unwrap(),
                "metered and unmetered object bytes identical"
            );
        }

        // Byte-identical dest trees.
        assert_nested_dest(metered_dest_dir.path());
        assert_nested_dest(plain_dest_dir.path());
    }

    #[test]
    fn file_store_strip_leading_dot_slash() {
        assert_eq!(strip_leading_dot_slash("./foo"), "foo");
        assert_eq!(strip_leading_dot_slash("./a/b/c"), "a/b/c");
        assert_eq!(strip_leading_dot_slash("./a/"), "a");
        assert_eq!(strip_leading_dot_slash("./"), "");
        assert_eq!(strip_leading_dot_slash("/abs/path"), "/abs/path");
    }

    // --- StreamStore (object/manifest-level streaming) -------------------
    //
    // Hermetic: FileStore is local, so these exercise the verify discipline
    // (BLAKE3 round-trip + corruption rejection) without any cloud creds.

    #[test]
    fn stream_store_filestore_object_roundtrip() {
        let store_dir = TempDir::new("stream-roundtrip");
        let store = FileStore::from_root(store_dir.path());

        let bytes = b"hello stream store\n".to_vec();
        let checksum = Blake3Hasher::new().hash_hex(&bytes);

        // Absent before the write.
        assert!(!store.has_object(&checksum).unwrap());

        store.put_object(&checksum, bytes.clone()).expect("put ok");

        // Present after, and the round-tripped bytes are identical.
        assert!(store.has_object(&checksum).unwrap());
        assert_eq!(store.get_object(&checksum).unwrap(), bytes);

        // It landed at the exact sharded content-address.
        assert!(store_dir.path().join(object_path(&checksum)).exists());
    }

    #[test]
    fn stream_store_get_object_rejects_corruption() {
        let store_dir = TempDir::new("stream-corrupt");
        let store = FileStore::from_root(store_dir.path());

        // Address a blob under `checksum` but write DIFFERENT bytes directly
        // to its on-disk path, simulating a corrupt/tampered object.
        let good = b"the real object bytes\n".to_vec();
        let checksum = Blake3Hasher::new().hash_hex(&good);
        let target = store_dir.path().join(object_path(&checksum));
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"TAMPERED bytes that do not hash to the address\n").unwrap();

        match store.get_object(&checksum) {
            Err(StoreError::Integrity {
                expected, actual, ..
            }) => {
                assert_eq!(expected, checksum);
                assert_ne!(actual, checksum, "actual must differ from the address");
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
    }

    #[test]
    fn stream_store_put_object_rejects_wrong_checksum() {
        let store_dir = TempDir::new("stream-wrong-checksum");
        let store = FileStore::from_root(store_dir.path());

        let bytes = b"some payload\n".to_vec();
        // A syntactically-valid but WRONG content-address.
        let wrong = "dead".repeat(16); // 64 hex chars, not the real hash.
        assert_ne!(wrong, Blake3Hasher::new().hash_hex(&bytes));

        match store.put_object(&wrong, bytes) {
            Err(StoreError::Integrity { expected, .. }) => assert_eq!(expected, wrong),
            other => panic!("expected Integrity, got {other:?}"),
        }

        // Nothing was stored at the bogus address.
        assert!(!store.has_object(&wrong).unwrap());
        assert!(!store_dir.path().join(object_path(&wrong)).exists());
    }

    #[test]
    fn stream_store_put_manifest_roundtrips() {
        let store_dir = TempDir::new("stream-manifest");
        let src_dir = TempDir::new("stream-manifest-src");
        let store = FileStore::from_root(store_dir.path());

        let (manifest, id) = make_foo_bar_source(src_dir.path());

        store.put_manifest(&id, &manifest).expect("put_manifest ok");

        // get_manifest reads it back, re-verifies the id, and yields an equal
        // manifest.
        let back = store.get_manifest(&id).expect("get_manifest ok");
        assert_eq!(back.entries(), manifest.entries());
        assert_eq!(
            snapdir_core::merkle::snapshot_id(&back, &Blake3Hasher::new()),
            id
        );
    }
}
