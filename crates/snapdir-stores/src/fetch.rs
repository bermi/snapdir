//! Shared concurrent fetch orchestrator for object-store backends.
//!
//! `S3Store::fetch_files` and `GcsStore::fetch_files` are byte-identical except
//! for the single per-object download call. [`fetch_files_concurrent`] factors
//! out that loop so both backends download objects concurrently (bounded by the
//! store's [`TransferConfig`] concurrency, throttled by a shared
//! [`RateLimiter`]) while each backend injects only its own download closure.
//!
//! The orchestrator preserves every Bash/Phase-12 invariant of the original
//! sequential loops:
//!
//! - **Skip-if-present-and-verified (Phase-12 pull-skip-existing):** a present
//!   destination file whose content already hashes to the manifest checksum is
//!   short-circuited *before any download* — the injected `download` closure is
//!   never called for it, so a fully-materialized tree does zero downloads.
//! - **Directories first, parents pre-created:** every `Directory` entry and
//!   every to-download `File` entry's parent directory is created in a
//!   sequential first pass, so the concurrent writers never race on
//!   `create_dir_all`.
//! - **Per-object verify + atomic write:** the `download` closure returns bytes
//!   already BLAKE3-verified against the entry checksum (each backend's
//!   `fetch_verified`, with its retry budget), then [`write_atomic`] renames a
//!   temp sibling into place. Distinct entries are content-addressed to distinct
//!   targets, so concurrent writes never collide.
//! - **First error wins:** [`run_concurrent`] propagates the first download or
//!   write error and cancels the rest.

use std::path::Path;

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::Blake3Hasher;
use snapdir_core::store::StoreError;
use snapdir_core::Meter;

use crate::transfer::{run_concurrent, RateLimiter, TransferConfig};
use crate::util::file_present_and_verified;

/// Strips a leading `./` and a trailing `/` from a manifest path so the
/// remainder can be joined onto a destination root (shared with the backends).
fn strip_leading_dot_slash(path: &str) -> &str {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    trimmed.strip_suffix('/').unwrap_or(trimmed)
}

/// Writes `bytes` to `target` via a temp sibling + atomic rename (same fs).
///
/// Shared by the concurrent fetch tasks: each entry has a unique
/// (content-addressed) target, so the per-write temp name only needs to be
/// unique within a single target's directory, which the atomic counter + pid
/// guarantee.
pub(crate) fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = match target.parent() {
        Some(parent) => parent.join(format!("{file_name}.{pid}.{n}.tmp")),
        None => std::path::PathBuf::from(format!("{file_name}.{pid}.{n}.tmp")),
    };
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, target)?;
    Ok(())
}

/// Materializes `manifest` under `dest`, downloading the missing file objects
/// concurrently.
///
/// `download` is the per-object download closure each backend injects: given a
/// [`ManifestEntry`], it returns its already-verified object bytes (e.g.
/// `S3Store::fetch_verified` / `GcsStore::fetch_verified`, which apply the
/// per-object BLAKE3 verify + retry budget). The orchestrator owns the
/// skip-present short-circuit, directory creation, rate limiting, bounded
/// concurrency, and the atomic write.
///
/// # Errors
///
/// Propagates the first [`StoreError`] from directory creation, a `download`
/// call, or an atomic write; remaining in-flight downloads are cancelled.
pub(crate) async fn fetch_files_concurrent<'a, F, Fut>(
    manifest: &'a Manifest,
    dest: &Path,
    config: &TransferConfig,
    rate_limiter: &RateLimiter,
    meter: Option<&Meter>,
    download: F,
) -> Result<(), StoreError>
where
    F: Fn(&'a ManifestEntry) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, StoreError>>,
{
    let hasher = Blake3Hasher::new();

    // First pass (sequential): create every directory, pre-create the parent of
    // every file, and collect the file entries that still need downloading.
    // Skip-if-present-and-verified short-circuits here, BEFORE any download.
    let mut to_download: Vec<(&ManifestEntry, std::path::PathBuf)> = Vec::new();
    for entry in manifest.entries() {
        let rel = strip_leading_dot_slash(&entry.path);
        let target = dest.join(rel);
        match entry.path_type {
            PathType::Directory => {
                std::fs::create_dir_all(&target)?;
            }
            PathType::File => {
                // A present, checksum-matching destination needs no download.
                if file_present_and_verified(&target, &entry.checksum, &hasher) {
                    // Skip-present: record it as skipped (advisory only).
                    if let Some(m) = meter {
                        m.add_skipped(1);
                    }
                    continue;
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                to_download.push((entry, target));
            }
        }
    }

    // Total to download (bytes), recorded so the bar can track bytes. Advisory:
    // does not affect what is downloaded. No-op when there is no meter.
    if let Some(m) = meter {
        let total: u64 = to_download.iter().map(|(entry, _)| entry.size).sum();
        m.set_total(total);
    }

    // Concurrent pass: download + atomically write each missing object, bounded
    // by `config.concurrency` and throttled by the shared rate limiter.
    run_concurrent(to_download, config.concurrency, |(entry, target)| {
        let download = &download;
        let rate_limiter = &rate_limiter;
        async move {
            // Throttle by the manifest-declared object size before fetching.
            rate_limiter.acquire(entry.size).await;
            if let Some(m) = meter {
                m.object_started();
            }
            let bytes = download(entry).await?;
            // The download returned verified bytes (bytes-in).
            if let Some(m) = meter {
                m.add_in(bytes.len() as u64);
            }
            write_atomic(&target, &bytes)?;
            // Bytes landed on disk (bytes-out), object done.
            if let Some(m) = meter {
                m.add_out(bytes.len() as u64);
                m.object_finished();
            }
            Ok(())
        }
    })
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapdir_core::merkle::Hasher;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Builds a multi-thread tokio runtime with time enabled, so the concurrent
    /// downloads can genuinely overlap and the high-water mark is meaningful.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_time()
            .build()
            .expect("build tokio runtime")
    }

    /// A tiny temp-dir helper so tests don't pull in a dev-dependency. Creates a
    /// unique directory under the system temp dir and removes it on drop.
    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("snapdir-fetch-test-{}-{n}", std::process::id()));
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

    /// BLAKE3 hex of `bytes`, used both to build manifest entries and to
    /// pre-seed already-present destination files in the skip test.
    fn checksum_of(bytes: &[u8]) -> String {
        Blake3Hasher::new().hash_hex(bytes)
    }

    /// Builds a tiny manifest: a root dir entry plus one `File` entry per
    /// `(rel_path, contents)`, with each file's checksum/size derived from its
    /// contents so the skip-present check and rate-limit accounting are real.
    fn manifest_for(files: &[(&str, &[u8])]) -> Manifest {
        let mut m = Manifest::new();
        m.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            "0".repeat(64),
            0,
            "./",
        ));
        for (path, contents) in files {
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

    /// A fake download closure factory: returns canned bytes keyed by checksum,
    /// records which checksums were requested, and tracks the high-water mark of
    /// concurrently in-flight downloads.
    struct FakeDownloader {
        /// checksum -> canned bytes to return.
        contents: HashMap<String, Vec<u8>>,
        /// Checksums for which the closure was invoked (in call order).
        called: Mutex<Vec<String>>,
        /// Currently in-flight downloads.
        in_flight: AtomicUsize,
        /// Peak simultaneous in-flight downloads.
        high_water: AtomicUsize,
    }

    impl FakeDownloader {
        fn new(files: &[(&str, &[u8])]) -> Arc<Self> {
            let contents = files
                .iter()
                .map(|(_, c)| (checksum_of(c), c.to_vec()))
                .collect();
            Arc::new(Self {
                contents,
                called: Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                high_water: AtomicUsize::new(0),
            })
        }

        async fn download(&self, entry: &ManifestEntry) -> Result<Vec<u8>, StoreError> {
            self.called.lock().unwrap().push(entry.checksum.clone());
            let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.high_water.fetch_max(cur, Ordering::SeqCst);
            // Hold the slot briefly so concurrent calls actually overlap.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            self.contents
                .get(&entry.checksum)
                .cloned()
                .ok_or_else(|| StoreError::ObjectNotFound {
                    checksum: entry.checksum.clone(),
                })
        }
    }

    #[test]
    fn concurrent_download_orchestrator_materializes_all() {
        let files: &[(&str, &[u8])] = &[
            ("a.txt", b"alpha" as &[u8]),
            ("nested/b.txt", b"bravo"),
            ("nested/deep/c.txt", b"charlie"),
            ("d.txt", b"delta"),
        ];
        let manifest = manifest_for(files);

        for concurrency in [1usize, 4] {
            let dest = TempDir::new();
            let fake = FakeDownloader::new(files);
            let cfg = TransferConfig::new(concurrency, None);
            let limiter = RateLimiter::new(None);

            let rt = runtime();
            let fake_ref = Arc::clone(&fake);
            rt.block_on(async {
                fetch_files_concurrent(&manifest, dest.path(), &cfg, &limiter, None, |entry| {
                    let fake = Arc::clone(&fake_ref);
                    async move { fake.download(entry).await }
                })
                .await
            })
            .expect("orchestrator must succeed");

            // Every file landed at the right path with the right bytes.
            for (path, contents) in files {
                let got = std::fs::read(dest.path().join(path))
                    .unwrap_or_else(|e| panic!("missing {path}: {e}"));
                assert_eq!(&got, contents, "wrong bytes for {path}");
            }
            // Directory was created.
            assert!(dest.path().join("nested/deep").is_dir());

            // High-water mark == min(concurrency, n) for >1, == 1 at concurrency 1.
            let hw = fake.high_water.load(Ordering::SeqCst);
            let expected = concurrency.min(files.len());
            assert_eq!(
                hw, expected,
                "concurrency={concurrency}: peak in-flight {hw} != expected {expected}"
            );
        }
    }

    #[test]
    fn concurrent_download_skips_present_and_verified() {
        let files: &[(&str, &[u8])] = &[
            ("present.txt", b"already-here" as &[u8]),
            ("missing.txt", b"needs-download"),
        ];
        let manifest = manifest_for(files);

        let dest = TempDir::new();
        // Pre-create `present.txt` with the exact verified contents.
        std::fs::write(dest.path().join("present.txt"), b"already-here").unwrap();

        let fake = FakeDownloader::new(files);
        let cfg = TransferConfig::new(4, None);
        let limiter = RateLimiter::new(None);

        let rt = runtime();
        let fake_ref = Arc::clone(&fake);
        rt.block_on(async {
            fetch_files_concurrent(&manifest, dest.path(), &cfg, &limiter, None, |entry| {
                let fake = Arc::clone(&fake_ref);
                async move { fake.download(entry).await }
            })
            .await
        })
        .expect("orchestrator must succeed");

        // The present+verified file triggered ZERO downloads; only the missing
        // file's checksum was requested.
        let called = fake.called.lock().unwrap().clone();
        let present_sum = checksum_of(b"already-here");
        let missing_sum = checksum_of(b"needs-download");
        assert!(
            !called.contains(&present_sum),
            "present+verified file must not be downloaded"
        );
        assert_eq!(
            called,
            vec![missing_sum],
            "only the missing file should be downloaded"
        );

        // The missing file was still materialized.
        assert_eq!(
            std::fs::read(dest.path().join("missing.txt")).unwrap(),
            b"needs-download"
        );
    }

    #[test]
    fn concurrent_download_propagates_error() {
        let files: &[(&str, &[u8])] = &[
            ("ok1.txt", b"one" as &[u8]),
            ("boom.txt", b"two"),
            ("ok2.txt", b"three"),
        ];
        let manifest = manifest_for(files);
        let boom_sum = checksum_of(b"two");

        let dest = TempDir::new();
        let cfg = TransferConfig::new(4, None);
        let limiter = RateLimiter::new(None);

        let rt = runtime();
        let boom = boom_sum.clone();
        let result = rt.block_on(async {
            fetch_files_concurrent(&manifest, dest.path(), &cfg, &limiter, None, |entry| {
                let boom = boom.clone();
                async move {
                    if entry.checksum == boom {
                        return Err(StoreError::Backend {
                            message: "download blew up".to_owned(),
                            source: None,
                        });
                    }
                    Ok(b"unused".to_vec())
                }
            })
            .await
        });

        let err = result.expect_err("the failing download must surface");
        assert!(
            matches!(err, StoreError::Backend { ref message, .. } if message == "download blew up"),
            "unexpected error: {err:?}"
        );
    }
}
