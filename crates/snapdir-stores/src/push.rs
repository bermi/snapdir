//! Shared concurrent push orchestrator for object-store backends.
//!
//! `S3Store::push` and `GcsStore::push` (and, via S3, `B2Store`) are
//! byte-identical except for the per-object existence check, the per-object
//! upload, and the final manifest write. [`push_objects_concurrent`] factors
//! out that loop so both backends upload objects concurrently (bounded by the
//! store's [`TransferConfig`] concurrency, throttled by a shared
//! [`RateLimiter`]) while each backend injects only those store-specific calls.
//!
//! The orchestrator preserves every Bash/oracle invariant of the original
//! sequential push loops:
//!
//! - **Per-object content-addressed skip:** the injected `key_exists` closure
//!   is consulted for every `File` entry; a present object is skipped with no
//!   read and no upload (content-addressed, so a present object is already the
//!   right bytes).
//! - **Invalid-source guard (verify-before-upload):** for an absent object the
//!   source file at `source.join(rel)` is read and its BLAKE3 is verified
//!   against the manifest checksum *before* the upload; a mismatch is a
//!   [`StoreError::Integrity`] and no manifest is written. The read + verify is
//!   shared here so S3 and GCS never duplicate it.
//! - **Manifest-last / all-or-nothing:** the injected `write_manifest` closure
//!   is called **exactly once and only after every object upload has returned
//!   `Ok`**. If any object upload errors, [`run_concurrent`] returns that error
//!   and the orchestrator returns it *without* writing the manifest — a failed
//!   push leaves no manifest, so a present manifest always implies all of its
//!   objects are present.
//! - **First error wins:** [`run_concurrent`] propagates the first upload error
//!   and cancels the rest.
//!
//! The skip-if-manifest-present early return stays in each store's `push`
//! (a cheap pre-check) *before* this orchestrator is called.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{Blake3Hasher, Hasher};
use snapdir_core::store::StoreError;
use snapdir_core::Meter;

use crate::adaptive::{
    p95_object_size, AdaptiveGate, AdaptivePolicy as ControllerPolicy, ControllerDriver, OpResult,
    OpSample,
};
use crate::transfer::{
    classify_error, run_adaptive, run_concurrent, AdaptivePolicy, RateLimiter, TransferConfig,
};

/// Strips a leading `./` and a trailing `/` from a manifest path so the
/// remainder can be joined onto a source root (shared with the backends).
fn strip_leading_dot_slash(path: &str) -> &str {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    trimmed.strip_suffix('/').unwrap_or(trimmed)
}

/// Reads the source file for `entry` under `source`, verifying its BLAKE3
/// against the manifest checksum (the oracle's invalid-source guard) and
/// acquiring rate-limit budget for its byte length before returning the bytes
/// ready to upload.
///
/// Factored out so S3/GCS upload closures inject only `key_exists` +
/// `put_bytes` and never duplicate the read/verify/throttle logic.
///
/// # Errors
///
/// - I/O errors reading `source.join(rel)`.
/// - [`StoreError::Integrity`] when the source bytes no longer hash to the
///   entry's checksum.
async fn read_verified(
    entry: &ManifestEntry,
    source: &Path,
    rate_limiter: &RateLimiter,
    meter: Option<&Meter>,
) -> Result<Vec<u8>, StoreError> {
    let rel = strip_leading_dot_slash(&entry.path);
    let object_source = source.join(rel);
    let bytes = std::fs::read(&object_source)?;
    let actual = Blake3Hasher::new().hash_hex(&bytes);
    if actual != entry.checksum {
        return Err(StoreError::Integrity {
            address: object_source.display().to_string(),
            expected: entry.checksum.clone(),
            actual,
        });
    }
    // Source bytes read (bytes-in). Advisory only.
    if let Some(m) = meter {
        m.add_in(bytes.len() as u64);
    }
    // Throttle by the (verified) object size before the upload.
    rate_limiter.acquire(bytes.len() as u64).await;
    Ok(bytes)
}

/// Uploads every absent object referenced by `manifest` concurrently, then —
/// only if every upload succeeded — writes the manifest via `write_manifest`.
///
/// `upload_one` is the per-object closure each backend injects: given a `File`
/// [`ManifestEntry`], it performs the store-specific existence check + upload.
/// It is handed [`read_verified`] (as the `read`-and-verify step) so each
/// backend only injects `key_exists` + `put_bytes`. `write_manifest` is the
/// backend's manifest-write closure (its own verify + `put_bytes`).
///
/// The orchestrator owns bounded concurrency and the manifest-last /
/// all-or-nothing ordering.
///
/// # Errors
///
/// Propagates the first [`StoreError`] from any object upload (remaining
/// in-flight uploads are cancelled and **no** manifest is written), or the
/// error from `write_manifest`.
pub(crate) async fn push_objects_concurrent<'a, U, UFut, W, WFut>(
    manifest: &'a Manifest,
    config: &TransferConfig,
    rate_limiter: &RateLimiter,
    meter: Option<&Meter>,
    meter_arc: Option<Arc<Meter>>,
    upload_one: U,
    write_manifest: W,
) -> Result<(), StoreError>
where
    U: Fn(&'a ManifestEntry) -> UFut,
    UFut: std::future::Future<Output = Result<(), StoreError>>,
    W: FnOnce() -> WFut,
    WFut: std::future::Future<Output = Result<(), StoreError>>,
{
    // Only `File` entries carry object bytes; directories live only in the
    // manifest text. Order is irrelevant (content-addressed).
    let files: Vec<&ManifestEntry> = manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .collect();

    // Total to push (bytes over the file set), recorded so the bar can track
    // bytes. Advisory: no effect on what is uploaded. No-op without a meter.
    if let Some(m) = meter {
        let total: u64 = files.iter().map(|e| e.size).sum();
        m.set_total(total);
    }

    match config.adaptive {
        AdaptivePolicy::Off => {
            // Concurrent object pass: each task runs the injected per-object work
            // (existence check, then read+verify+upload for absent objects). The
            // first error is propagated and the rest cancelled.
            run_concurrent(files, config.concurrency, upload_one).await?;
        }
        AdaptivePolicy::On { fraction, ceiling } => {
            run_adaptive_objects(
                &files,
                config,
                rate_limiter,
                meter_arc,
                fraction,
                ceiling,
                upload_one,
            )
            .await?;
        }
    }

    // Manifest-last / all-or-nothing: only after every object upload returned
    // Ok do we write the manifest. A failed push (an error above) returns
    // early and leaves no manifest.
    write_manifest().await
}

/// The adaptive sibling of the fixed-concurrency object pass: sizes the in-flight
/// window to the policy ceiling, gates each upload to the controller's live
/// limit, times + classifies + records every op, and runs a background tick
/// driver that resizes the gate and retunes the shared rate limiter. The exact
/// objects uploaded and the first-error-wins semantics are identical to the
/// `Off` path — only scheduling/rate differ.
#[allow(clippy::too_many_arguments)]
async fn run_adaptive_objects<'a, U, UFut>(
    files: &[&'a ManifestEntry],
    config: &TransferConfig,
    rate_limiter: &RateLimiter,
    meter_arc: Option<Arc<Meter>>,
    fraction: f64,
    ceiling: usize,
    upload_one: U,
) -> Result<(), StoreError>
where
    U: Fn(&'a ManifestEntry) -> UFut,
    UFut: std::future::Future<Output = Result<(), StoreError>>,
{
    let gate = AdaptiveGate::new(config.concurrency.get(), ceiling);
    let driver = build_push_driver(
        files,
        config,
        rate_limiter,
        meter_arc,
        fraction,
        ceiling,
        &gate,
    );

    // Background tick driver: ~250ms cadence (well below the controller's 15s
    // cooldown/re-probe constants) so the gate/limiter track the controller live.
    let tick_driver = driver.clone();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let tick_handle = tokio::spawn(async move {
        loop {
            ticker.tick().await;
            tick_driver.tick();
        }
    });

    let result = run_adaptive(files.iter().copied(), &gate, |entry| {
        let upload_one = &upload_one;
        let driver = &driver;
        async move {
            let started = Instant::now();
            let outcome = upload_one(entry).await;
            let latency = started.elapsed();
            let (bytes, op_result) = match &outcome {
                Ok(()) => (entry.size, OpResult::Ok),
                Err(err) => (0, classify_error(err)),
            };
            driver.record_op(OpSample {
                bytes,
                latency,
                result: op_result,
            });
            outcome
        }
    })
    .await;

    tick_handle.abort();
    result.map(|_| ())
}

/// Builds the [`ControllerDriver`] for the async push path: a controller policy
/// derived from the config (ceiling + fraction, machine RAM for the memory
/// budget, `max_bytes_per_sec` as the rate cap), wired to retune the async
/// [`RateLimiter`] and mirror the limit/rate into the display meter.
fn build_push_driver(
    files: &[&ManifestEntry],
    config: &TransferConfig,
    rate_limiter: &RateLimiter,
    meter_arc: Option<Arc<Meter>>,
    fraction: f64,
    ceiling: usize,
    gate: &AdaptiveGate,
) -> ControllerDriver {
    let sizes: Vec<u64> = files.iter().map(|e| e.size).collect();
    let p95 = p95_object_size(&sizes);
    let total_ram = snapdir_core::resources::total_ram_bytes().unwrap_or(0);
    let policy = ControllerPolicy::new(fraction, ceiling, total_ram, config.max_bytes_per_sec);

    // The async limiter's `set_rate` is async; bridge it to the driver's sync
    // applier by spawning the retune on the current runtime (best-effort,
    // advisory). Cloning shares the same bucket.
    let limiter = rate_limiter.clone();
    let rate_applier: Arc<dyn Fn(Option<u64>) + Send + Sync> = Arc::new(move |rate| {
        let limiter = limiter.clone();
        tokio::spawn(async move {
            limiter.set_rate(rate).await;
        });
    });

    ControllerDriver::new(policy, gate.clone(), p95, Some(rate_applier), meter_arc)
}

/// The shared per-object upload step S3/GCS inject into
/// [`push_objects_concurrent`]: skip if the object key already exists,
/// otherwise read+verify the source (via [`read_verified`]) and upload it.
///
/// Backends supply `key_exists` and `put_bytes` closures over their own
/// `&self`; `object_key` is the backend's content-addressed key for the entry.
///
/// # Errors
///
/// Propagates errors from `key_exists`, the source read/verify, or `put_bytes`.
pub(crate) async fn upload_object<KFut, PFut>(
    entry: &ManifestEntry,
    object_key: String,
    source: &Path,
    rate_limiter: &RateLimiter,
    meter: Option<&Meter>,
    key_exists: impl FnOnce(String) -> KFut,
    put_bytes: impl FnOnce(String, Vec<u8>) -> PFut,
) -> Result<(), StoreError>
where
    KFut: std::future::Future<Output = Result<bool, StoreError>>,
    PFut: std::future::Future<Output = Result<(), StoreError>>,
{
    // Per-object content-addressed skip: a present object is already the right
    // bytes, so no read and no upload.
    if key_exists(object_key.clone()).await? {
        if let Some(m) = meter {
            m.add_skipped(1);
        }
        return Ok(());
    }
    if let Some(m) = meter {
        m.object_started();
    }
    // `read_verified` records bytes-in (the verified source bytes).
    let bytes = read_verified(entry, source, rate_limiter, meter).await?;
    let len = bytes.len() as u64;
    put_bytes(object_key, bytes).await?;
    // Upload succeeded: bytes-out + object done.
    if let Some(m) = meter {
        m.add_out(len);
        m.object_finished();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapdir_core::merkle::Hasher;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Multi-thread runtime with time enabled so concurrent uploads genuinely
    /// overlap and the in-flight high-water mark is meaningful.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_time()
            .build()
            .expect("build tokio runtime")
    }

    /// A temp-dir helper (avoids a dev-dependency) that removes itself on drop.
    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("snapdir-push-test-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn checksum_of(bytes: &[u8]) -> String {
        Blake3Hasher::new().hash_hex(bytes)
    }

    /// Builds a manifest (a root dir entry + one `File` per `(rel, contents)`)
    /// and writes each file's contents under `src` so read+verify is real.
    fn manifest_and_source(files: &[(&str, &[u8])], src: &Path) -> Manifest {
        let mut m = Manifest::new();
        m.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            "0".repeat(64),
            0,
            "./",
        ));
        for (path, contents) in files {
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(src.join(parent)).unwrap();
            }
            std::fs::write(src.join(path), contents).unwrap();
            m.push(ManifestEntry::new(
                PathType::File,
                "600",
                checksum_of(contents),
                contents.len() as u64,
                format!("./{path}"),
            ));
        }
        Manifest::from_entries(m.entries().to_vec())
    }

    /// A fake S3/GCS-like backend: tracks which keys "exist", records uploaded
    /// checksums, the in-flight high-water mark, and whether the manifest was
    /// written (and that it was written after the uploads).
    struct FakeStore {
        /// Object keys that already exist (skip-present source of truth).
        present: HashSet<String>,
        /// Checksums actually uploaded (in completion order).
        uploaded: Mutex<Vec<String>>,
        /// Currently in-flight uploads.
        in_flight: AtomicUsize,
        /// Peak simultaneous in-flight uploads.
        high_water: AtomicUsize,
        /// Set once the manifest write closure runs.
        manifest_written: AtomicBool,
        /// How many uploads had completed when the manifest was written.
        uploads_done_at_manifest: AtomicUsize,
        /// Checksum whose upload must fail (all-or-nothing test); empty = none.
        fail_checksum: String,
    }

    impl FakeStore {
        fn new(present: &[&str], fail_checksum: &str) -> Arc<Self> {
            Arc::new(Self {
                present: present.iter().map(|s| (*s).to_owned()).collect(),
                uploaded: Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                high_water: AtomicUsize::new(0),
                manifest_written: AtomicBool::new(false),
                uploads_done_at_manifest: AtomicUsize::new(0),
                fail_checksum: fail_checksum.to_owned(),
            })
        }

        // `async` (despite no await) to model the real backends' async
        // existence check the orchestrator awaits.
        #[allow(clippy::unused_async)]
        async fn key_exists(&self, key: String) -> Result<bool, StoreError> {
            Ok(self.present.contains(&key))
        }

        async fn put_bytes(&self, _key: String, bytes: Vec<u8>) -> Result<(), StoreError> {
            let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.high_water.fetch_max(cur, Ordering::SeqCst);
            // Hold the slot so concurrent uploads actually overlap.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let sum = checksum_of(&bytes);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            if !self.fail_checksum.is_empty() && sum == self.fail_checksum {
                return Err(StoreError::Backend {
                    message: "upload blew up".to_owned(),
                    source: None,
                });
            }
            self.uploaded.lock().unwrap().push(sum);
            Ok(())
        }

        #[allow(clippy::unused_async)]
        async fn write_manifest(&self) -> Result<(), StoreError> {
            self.uploads_done_at_manifest
                .store(self.uploaded.lock().unwrap().len(), Ordering::SeqCst);
            self.manifest_written.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Runs the orchestrator over `manifest`/`src` with `fake` injected.
    fn run_push(
        fake: &Arc<FakeStore>,
        manifest: &Manifest,
        src: &Path,
        concurrency: usize,
    ) -> Result<(), StoreError> {
        let cfg = TransferConfig::new(concurrency, None);
        let limiter = RateLimiter::new(None);
        let rt = runtime();
        rt.block_on(async {
            push_objects_concurrent(
                manifest,
                &cfg,
                &limiter,
                None,
                None,
                |entry| {
                    let fake = Arc::clone(fake);
                    let limiter = &limiter;
                    async move {
                        // Use the entry's checksum as the object key so the
                        // fake's `present` set keys on checksums directly.
                        upload_object(
                            entry,
                            entry.checksum.clone(),
                            src,
                            limiter,
                            None,
                            |key| {
                                let fake = Arc::clone(&fake);
                                async move { fake.key_exists(key).await }
                            },
                            |key, bytes| {
                                let fake = Arc::clone(&fake);
                                async move { fake.put_bytes(key, bytes).await }
                            },
                        )
                        .await
                    }
                },
                || {
                    let fake = Arc::clone(fake);
                    async move { fake.write_manifest().await }
                },
            )
            .await
        })
    }

    #[test]
    fn concurrent_upload_all_objects_then_manifest() {
        let files: &[(&str, &[u8])] = &[
            ("a.txt", b"alpha" as &[u8]),
            ("nested/b.txt", b"bravo"),
            ("nested/deep/c.txt", b"charlie"),
            ("d.txt", b"delta"),
        ];

        for concurrency in [1usize, 4] {
            let src = TempDir::new();
            let manifest = manifest_and_source(files, src.path());
            let fake = FakeStore::new(&[], "");

            run_push(&fake, &manifest, src.path(), concurrency).expect("push must succeed");

            // Every absent object was uploaded exactly once.
            let mut uploaded = fake.uploaded.lock().unwrap().clone();
            uploaded.sort();
            let mut expected: Vec<String> = files.iter().map(|(_, c)| checksum_of(c)).collect();
            expected.sort();
            assert_eq!(uploaded, expected, "all absent objects must be uploaded");

            // Concurrency bound: peak in-flight == min(concurrency, n), == 1 at
            // concurrency 1 (strictly sequential).
            let hw = fake.high_water.load(Ordering::SeqCst);
            let want = concurrency.min(files.len());
            assert_eq!(
                hw, want,
                "concurrency={concurrency}: peak in-flight {hw} != expected {want}"
            );

            // Manifest written EXACTLY once, and only AFTER all uploads.
            assert!(
                fake.manifest_written.load(Ordering::SeqCst),
                "manifest must be written"
            );
            assert_eq!(
                fake.uploads_done_at_manifest.load(Ordering::SeqCst),
                files.len(),
                "manifest must be written only after every object upload completed"
            );
        }
    }

    #[test]
    fn adaptive_upload_all_objects_then_manifest_within_ceiling() {
        // An adaptive push (policy On, ceiling 2) uploads every absent object,
        // writes the manifest last, and caps effective concurrency at the ceiling.
        use crate::transfer::AdaptivePolicy;

        let files: &[(&str, &[u8])] = &[
            ("a.txt", b"alpha" as &[u8]),
            ("nested/b.txt", b"bravo"),
            ("nested/deep/c.txt", b"charlie"),
            ("d.txt", b"delta"),
            ("e.txt", b"echo"),
        ];
        let src = TempDir::new();
        let manifest = manifest_and_source(files, src.path());
        let src_path = src.path().to_path_buf();
        let fake = FakeStore::new(&[], "");

        let cfg = TransferConfig::new(4, None).with_adaptive(AdaptivePolicy::On {
            fraction: 0.8,
            ceiling: 2,
        });
        let limiter = RateLimiter::new(None);
        let rt = runtime();
        let fake_ref = Arc::clone(&fake);
        rt.block_on(async {
            push_objects_concurrent(
                &manifest,
                &cfg,
                &limiter,
                None,
                None,
                |entry| {
                    let fake = Arc::clone(&fake_ref);
                    let limiter = &limiter;
                    let src_path = &src_path;
                    async move {
                        upload_object(
                            entry,
                            entry.checksum.clone(),
                            src_path,
                            limiter,
                            None,
                            |key| {
                                let fake = Arc::clone(&fake);
                                async move { fake.key_exists(key).await }
                            },
                            |key, bytes| {
                                let fake = Arc::clone(&fake);
                                async move { fake.put_bytes(key, bytes).await }
                            },
                        )
                        .await
                    }
                },
                || {
                    let fake = Arc::clone(&fake_ref);
                    async move { fake.write_manifest().await }
                },
            )
            .await
        })
        .expect("adaptive push must succeed");

        // All objects uploaded exactly once.
        let mut uploaded = fake.uploaded.lock().unwrap().clone();
        uploaded.sort();
        let mut expected: Vec<String> = files.iter().map(|(_, c)| checksum_of(c)).collect();
        expected.sort();
        assert_eq!(uploaded, expected, "all objects uploaded under adaptive");

        // Effective concurrency capped at the ceiling (2).
        let hw = fake.high_water.load(Ordering::SeqCst);
        assert!(
            hw <= 2,
            "effective concurrency must not exceed ceiling 2, got {hw}"
        );

        // Manifest written exactly once, after every upload.
        assert!(fake.manifest_written.load(Ordering::SeqCst));
        assert_eq!(
            fake.uploads_done_at_manifest.load(Ordering::SeqCst),
            files.len(),
            "manifest written only after every object upload"
        );
    }

    #[test]
    fn concurrent_upload_skips_present_objects() {
        let files: &[(&str, &[u8])] = &[
            ("present.txt", b"already-here" as &[u8]),
            ("missing.txt", b"needs-upload"),
        ];
        let src = TempDir::new();
        let manifest = manifest_and_source(files, src.path());

        // Mark `present.txt`'s object key (its checksum) as already present.
        let present_sum = checksum_of(b"already-here");
        let fake = FakeStore::new(&[present_sum.as_str()], "");

        run_push(&fake, &manifest, src.path(), 4).expect("push must succeed");

        let uploaded = fake.uploaded.lock().unwrap().clone();
        let missing_sum = checksum_of(b"needs-upload");
        assert!(
            !uploaded.contains(&present_sum),
            "present object must never be uploaded"
        );
        assert_eq!(
            uploaded,
            vec![missing_sum],
            "only the absent object should be uploaded"
        );
        assert!(fake.manifest_written.load(Ordering::SeqCst));
    }

    #[test]
    fn concurrent_upload_all_or_nothing_on_failure() {
        let files: &[(&str, &[u8])] = &[
            ("ok1.txt", b"one" as &[u8]),
            ("boom.txt", b"two"),
            ("ok2.txt", b"three"),
        ];
        let src = TempDir::new();
        let manifest = manifest_and_source(files, src.path());

        // The upload of `boom.txt`'s object fails.
        let boom_sum = checksum_of(b"two");
        let fake = FakeStore::new(&[], boom_sum.as_str());

        let result = run_push(&fake, &manifest, src.path(), 4);

        let err = result.expect_err("a failing object upload must surface");
        assert!(
            matches!(err, StoreError::Backend { ref message, .. } if message == "upload blew up"),
            "unexpected error: {err:?}"
        );
        // THE invariant: a failed push writes NO manifest.
        assert!(
            !fake.manifest_written.load(Ordering::SeqCst),
            "write_manifest must NEVER be called when an object upload fails"
        );
    }

    #[test]
    fn concurrent_upload_rejects_corrupt_source() {
        let files: &[(&str, &[u8])] = &[("good.txt", b"good" as &[u8]), ("bad.txt", b"bad")];
        let src = TempDir::new();
        let manifest = manifest_and_source(files, src.path());

        // Corrupt `bad.txt` on disk so its bytes no longer match the manifest
        // checksum: the verify-before-upload guard must fire.
        std::fs::write(src.path().join("bad.txt"), b"tampered").unwrap();
        let fake = FakeStore::new(&[], "");

        let result = run_push(&fake, &manifest, src.path(), 4);

        let err = result.expect_err("a corrupt source must surface an Integrity error");
        assert!(
            matches!(err, StoreError::Integrity { .. }),
            "unexpected error: {err:?}"
        );
        assert!(
            !fake.manifest_written.load(Ordering::SeqCst),
            "a corrupt source must leave no manifest"
        );
    }
}
