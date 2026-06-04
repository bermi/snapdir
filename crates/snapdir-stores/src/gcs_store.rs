//! `GcsStore`: the `gs://` storage backend, backed by the native
//! `google-cloud-storage` SDK.
//!
//! A [`GcsStore`] targets a `gs://bucket/prefix` location and holds the frozen
//! content-addressable `.objects`/`.manifests` sharded layout, so a
//! bucket/prefix is interchangeable across conforming implementations:
//!
//! ```text
//! gs://<bucket>/<prefix>/.objects/<sharded checksum>     raw object bytes
//! gs://<bucket>/<prefix>/.manifests/<sharded snapshot id> manifest text
//! ```
//!
//! Sharding and the relative keys come straight from [`snapdir_core::store`]
//! ([`object_path`] / [`manifest_path`]); this module never reimplements them.
//!
//! # `gs://` parsing
//!
//! The oracle derives the bucket as `cut -d'/' -f3` of the URL
//! (`_snapdir_export_store_vars`) and the prefix with
//! `sed -E 's|^gs:/*[^/]*/?||'` then a trailing-slash strip
//! (`_snapdir_gcs_store_get_remote_prefix`). [`GcsLocation::parse`] reproduces
//! that exactly: bucket = first segment after the scheme, prefix = the
//! remainder with leading/trailing slashes removed.
//!
//! # Credentials
//!
//! Authentication is delegated entirely to the SDK's own credential chain
//! (Application Default Credentials): `GOOGLE_APPLICATION_CREDENTIALS`,
//! `GOOGLE_APPLICATION_CREDENTIALS_JSON`, `gcloud` user creds, and the GCE/GKE
//! metadata server. No bespoke snapdir credential variables are introduced.
//!
//! # TLS provider (project-load-bearing)
//!
//! The shipped binary must statically link on musl, so the workspace
//! standardizes on the **`ring`** rustls provider; `aws-lc-rs` is banned. The
//! `google-cloud-storage` default features pull `aws-lc-rs` in via
//! `google-cloud-auth` (both its id-token backend and its rustls provider), so
//! we depend on the crate with `default-features = false` and instead install
//! the **ring** [`CryptoProvider`](rustls_ring::crypto::CryptoProvider) as the
//! rustls *process default* (the SDK's `reqwest` is built with
//! `rustls-no-provider`, i.e. it consumes that process default). See the crate
//! `Cargo.toml` for the full rationale.
//!
//! # Sync trait, async SDK
//!
//! The SDK is async. [`GcsStore`] owns a private multi-thread `tokio` runtime
//! and bridges each [`Store`] method with `runtime.block_on(...)`, exactly like
//! [`S3Store`](crate::S3Store), so no `async` leaks into `snapdir-core`.

use std::path::Path;
use std::sync::Arc;

use google_cloud_gax::error::rpc::Code;
use google_cloud_gax::error::Error as GcsError;
use google_cloud_storage::client::{Storage, StorageControl};
use snapdir_core::manifest::Manifest;
use snapdir_core::merkle::{Blake3Hasher, Hasher};
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};

use crate::fetch::fetch_files_concurrent;
use crate::push::{push_objects_concurrent, upload_object};
use crate::stream::StreamStore;
use crate::transfer::{RateLimiter, TransferConfig};
use tokio::runtime::Runtime;

/// Number of times a fetch is retried when the downloaded bytes fail their
/// checksum, mirroring the oracle's `_SNAPDIR_GCS_STORE_RETRIES` default of 5.
const MAX_FETCH_RETRIES: u32 = 5;

/// The parsed location a [`GcsStore`] targets: a GCS bucket plus a key prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcsLocation {
    /// The bucket name (first path segment of the `gs://` URL).
    pub bucket: String,
    /// The object-name prefix (remaining segments), with no leading or trailing
    /// slash. Empty when the store points at the bucket root.
    pub prefix: String,
}

impl GcsLocation {
    /// Parses a `gs://bucket/prefix` URL into its bucket and prefix.
    ///
    /// Matches the oracle exactly: the bucket is `cut -d'/' -f3` of the store
    /// URL (the segment immediately after `gs://`), and the prefix is
    /// `sed -E 's|^gs:/*[^/]*/?||'` with a trailing slash stripped
    /// (`_snapdir_gcs_store_get_remote_prefix`).
    ///
    /// The `gs://` scheme is optional; a bare `bucket/prefix` is accepted too.
    #[must_use]
    pub fn parse(store_url: &str) -> Self {
        // Drop the scheme (`gs://`, or any `<proto>://`) if present. The oracle
        // splits the full URL on `/` and takes field 3 as the bucket, which for
        // `gs://bucket/...` is exactly the segment after the `//`.
        let without_scheme = match store_url.find("://") {
            Some(idx) => &store_url[idx + 3..],
            None => store_url,
        };
        let mut parts = without_scheme.splitn(2, '/');
        let bucket = parts.next().unwrap_or("").to_owned();
        let prefix = parts.next().unwrap_or("");
        // Strip a leading slash (empty-first-segment edge case) and the trailing
        // slash; the prefix is joined back with a single `/`.
        let prefix = prefix
            .trim_end_matches('/')
            .trim_start_matches('/')
            .to_owned();
        Self { bucket, prefix }
    }

    /// The GCS resource name for this bucket, `projects/_/buckets/<bucket>`, as
    /// required by the `google-cloud-storage` v1 API.
    #[must_use]
    pub fn bucket_resource(&self) -> String {
        format!("projects/_/buckets/{}", self.bucket)
    }

    /// Returns the full GCS object name for a content object given its checksum,
    /// i.e. `<prefix>/.objects/<sharded>` (no leading slash).
    #[must_use]
    pub fn object_key(&self, checksum: &str) -> String {
        self.key_for(&object_path(checksum))
    }

    /// Returns the full GCS object name for a manifest given its snapshot id,
    /// i.e. `<prefix>/.manifests/<sharded>` (no leading slash).
    #[must_use]
    pub fn manifest_key(&self, id: &str) -> String {
        self.key_for(&manifest_path(id))
    }

    /// Joins the store prefix with a store-relative path (`.objects/...` or
    /// `.manifests/...`), producing a leading-slash-free object name. Mirrors
    /// the oracle's `${_SNAPDIR_STORE_BASE_DIR%/}/${source_path#/}` with the
    /// leading slash trimmed.
    fn key_for(&self, rel: &str) -> String {
        let rel = rel.trim_start_matches('/');
        if self.prefix.is_empty() {
            rel.to_owned()
        } else {
            format!("{}/{rel}", self.prefix)
        }
    }
}

/// A content-addressable store backed by a Google Cloud Storage bucket.
///
/// Construct one with [`GcsStore::connect`] (resolves Application Default
/// Credentials via the SDK).
pub struct GcsStore {
    /// Data-plane client (object read/write).
    storage: Storage,
    /// Control-plane client (object metadata, used for HEAD-like existence
    /// checks without downloading the body).
    control: StorageControl,
    location: GcsLocation,
    runtime: Arc<Runtime>,
    config: TransferConfig,
}

impl GcsStore {
    /// Connects to the `gs://bucket/prefix` store, resolving credentials via the
    /// SDK's Application Default Credentials chain.
    ///
    /// The `ring` rustls crypto provider is installed as the process default
    /// (idempotent) before the clients are built, keeping `aws-lc-rs` out of the
    /// graph (see the module docs).
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] if the tokio runtime cannot be created or the SDK
    /// clients cannot be built (e.g. credentials cannot be resolved).
    pub fn connect(store_url: &str) -> Result<Self, StoreError> {
        Self::connect_with(store_url, TransferConfig::default())
    }

    /// Like [`connect`](Self::connect), but carries a [`TransferConfig`] for
    /// concurrency / bandwidth control. The existing [`connect`](Self::connect)
    /// delegates here with [`TransferConfig::default`].
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] if the tokio runtime cannot be created or the SDK
    /// clients cannot be built (e.g. credentials cannot be resolved).
    pub fn connect_with(store_url: &str, config: TransferConfig) -> Result<Self, StoreError> {
        let location = GcsLocation::parse(store_url);
        let runtime = build_runtime()?;
        install_ring_provider();

        let (storage, control) = runtime.block_on(async {
            let storage = Storage::builder()
                .build()
                .await
                .map_err(|e| backend("building GCS Storage client", e))?;
            let control = StorageControl::builder()
                .build()
                .await
                .map_err(|e| backend("building GCS StorageControl client", e))?;
            Ok::<_, StoreError>((storage, control))
        })?;

        Ok(Self {
            storage,
            control,
            location,
            runtime: Arc::new(runtime),
            config,
        })
    }

    /// Builds a store from already-configured SDK clients and a parsed location,
    /// owning a fresh tokio runtime for the sync bridge. Intended for tests.
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] if the tokio runtime cannot be created.
    pub fn from_clients(
        storage: Storage,
        control: StorageControl,
        location: GcsLocation,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            storage,
            control,
            location,
            runtime: Arc::new(build_runtime()?),
            config: TransferConfig::default(),
        })
    }

    /// The parsed bucket/prefix this store targets.
    #[must_use]
    pub fn location(&self) -> &GcsLocation {
        &self.location
    }

    /// The [`TransferConfig`] (concurrency / bandwidth) this store was built
    /// with. Consumed by the transfer loops in later gates.
    #[must_use]
    pub fn transfer_config(&self) -> &TransferConfig {
        &self.config
    }

    /// Metadata HEAD on an object key; `Ok(true)` if it exists, `Ok(false)` if
    /// absent (see [`is_not_found`] for what counts as absent).
    async fn key_exists(&self, key: &str) -> Result<bool, StoreError> {
        match self
            .control
            .get_object()
            .set_bucket(self.location.bucket_resource())
            .set_object(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(err) => {
                if is_not_found(&err) {
                    Ok(false)
                } else {
                    Err(backend("GCS get_object metadata failed", err))
                }
            }
        }
    }

    /// GET an object key's full body, draining the read stream, or `None` if it
    /// is absent (see [`is_not_found`] for what counts as absent).
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let mut resp = match self
            .storage
            .read_object(self.location.bucket_resource(), key)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                if is_not_found(&err) {
                    return Ok(None);
                }
                return Err(backend("GCS read_object failed", err));
            }
        };

        let mut buf = Vec::new();
        while let Some(chunk) = resp.next().await {
            let chunk = chunk.map_err(|e| backend("reading GCS object body", e))?;
            buf.extend_from_slice(&chunk);
        }
        Ok(Some(buf))
    }

    /// PUT `bytes` at `key`. GCS object writes are atomic (the new object is only
    /// visible once fully uploaded), so no temp-key dance is needed — matching
    /// the oracle's reliance on `gcloud storage cp` atomicity.
    async fn put_bytes(&self, key: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        // The SDK's upload future is large (>30 KiB); box it so it does not
        // bloat the enclosing `push` future (clippy::large_futures).
        let upload = self
            .storage
            .write_object(
                self.location.bucket_resource(),
                key,
                bytes::Bytes::from(bytes),
            )
            .send_buffered();
        Box::pin(upload)
            .await
            .map_err(|e| backend("GCS write_object failed", e))?;
        Ok(())
    }

    /// Downloads `key`, verifying its BLAKE3 against `expected`, retrying up to
    /// [`MAX_FETCH_RETRIES`] times. Mirrors `_snapdir_gcs_fetch_to_cache`.
    async fn fetch_verified(&self, key: &str, expected: &str) -> Result<Vec<u8>, StoreError> {
        let hasher = Blake3Hasher::new();
        let mut attempts_left = MAX_FETCH_RETRIES;
        loop {
            match self.get_bytes(key).await? {
                Some(bytes) => {
                    let actual = hasher.hash_hex(&bytes);
                    if actual == expected {
                        return Ok(bytes);
                    }
                    // Mismatched checksum after fetching: retry (the oracle
                    // decrements its retry budget on the same condition).
                    attempts_left = attempts_left.saturating_sub(1);
                    if attempts_left == 0 {
                        return Err(StoreError::Integrity {
                            address: format!("gs://{}/{key}", self.location.bucket),
                            expected: expected.to_owned(),
                            actual,
                        });
                    }
                }
                None => {
                    // Treat a missing key as not-found rather than spinning.
                    return Err(StoreError::ObjectNotFound {
                        checksum: expected.to_owned(),
                    });
                }
            }
        }
    }
}

impl Store for GcsStore {
    fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError> {
        let key = self.location.manifest_key(id);
        let bytes = self.runtime.block_on(async {
            match self.get_bytes(&key).await? {
                Some(b) => Ok(b),
                None => Err(StoreError::ManifestNotFound { id: id.to_owned() }),
            }
        })?;

        let text = String::from_utf8(bytes).map_err(|err| StoreError::Backend {
            message: format!("manifest {id} is not valid UTF-8"),
            source: Some(Box::new(err)),
        })?;
        let manifest = Manifest::parse(&text)?;

        // Verify the stored manifest hashes back to its snapshot id before
        // trusting it (oracle: the id check on fetch).
        let actual = snapdir_core::merkle::snapshot_id(&manifest, &Blake3Hasher::new());
        if actual != id {
            return Err(StoreError::Integrity {
                address: self.location.manifest_key(id),
                expected: id.to_owned(),
                actual,
            });
        }
        Ok(manifest)
    }

    fn fetch_files(&self, manifest: &Manifest, dest: &Path) -> Result<(), StoreError> {
        // Concurrent download via the shared orchestrator: it owns the
        // skip-if-present-and-verified short-circuit, directory creation, the
        // bounded-concurrency pass, the per-object rate limit, and the atomic
        // write. GCS only injects the per-object download, preserving the
        // BLAKE3-verify + retry discipline of `fetch_verified`.
        let limiter = RateLimiter::new(self.config.max_bytes_per_sec);
        self.runtime.block_on(async {
            fetch_files_concurrent(manifest, dest, &self.config, &limiter, |entry| async {
                let key = self.location.object_key(&entry.checksum);
                self.fetch_verified(&key, &entry.checksum).await
            })
            .await
        })
    }

    fn push(&self, manifest: &Manifest, source: &Path) -> Result<(), StoreError> {
        let hasher = Blake3Hasher::new();
        let id = snapdir_core::merkle::snapshot_id(manifest, &hasher);

        // Concurrent upload via the shared orchestrator: it owns the bounded
        // per-object pass and the manifest-last / all-or-nothing ordering. GCS
        // injects the per-object skip-present + upload (via `upload_object`,
        // which also owns the shared read+verify) and the manifest-write
        // closure. A failed push writes NO manifest.
        let limiter = RateLimiter::new(self.config.max_bytes_per_sec);
        self.runtime.block_on(async {
            // Skip-if-manifest-present pre-check: a present manifest implies all
            // its objects are present (we always write the manifest last).
            let manifest_key = self.location.manifest_key(&id);
            if self.key_exists(&manifest_key).await? {
                return Ok(());
            }

            push_objects_concurrent(
                manifest,
                &self.config,
                |entry| {
                    let object_key = self.location.object_key(&entry.checksum);
                    upload_object(
                        entry,
                        object_key,
                        source,
                        &limiter,
                        |key| async move { self.key_exists(&key).await },
                        |key, bytes| async move { self.put_bytes(&key, bytes).await },
                    )
                },
                || async {
                    // Write the manifest last (verified to hash back to its id),
                    // exactly as the oracle stores the manifest text.
                    let mut text = manifest.to_string();
                    text.push('\n');
                    let manifest_actual = hasher.hash_hex(text.as_bytes());
                    if manifest_actual != id {
                        return Err(StoreError::Integrity {
                            address: manifest_key.clone(),
                            expected: id.clone(),
                            actual: manifest_actual,
                        });
                    }
                    self.put_bytes(&manifest_key, text.into_bytes()).await
                },
            )
            .await
        })
    }
}

impl StreamStore for GcsStore {
    fn has_object(&self, checksum: &str) -> Result<bool, StoreError> {
        let key = self.location.object_key(checksum);
        self.runtime.block_on(async { self.key_exists(&key).await })
    }

    fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
        let key = self.location.object_key(checksum);
        let bytes = self.runtime.block_on(async {
            self.get_bytes(&key)
                .await?
                .ok_or_else(|| StoreError::ObjectNotFound {
                    checksum: checksum.to_owned(),
                })
        })?;

        // Verify the downloaded blob hashes back to its content-address before
        // returning it (corruption surfaces as `Integrity`, never bad bytes).
        let actual = Blake3Hasher::new().hash_hex(&bytes);
        if actual != checksum {
            return Err(StoreError::Integrity {
                address: format!("gs://{}/{key}", self.location.bucket),
                expected: checksum.to_owned(),
                actual,
            });
        }
        Ok(bytes)
    }

    fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        // Verify BEFORE uploading: a blob whose bytes do not hash to `checksum`
        // must never land at that content-address (nothing is stored).
        let actual = Blake3Hasher::new().hash_hex(&bytes);
        if actual != checksum {
            return Err(StoreError::Integrity {
                address: self.location.object_key(checksum),
                expected: checksum.to_owned(),
                actual,
            });
        }
        let key = self.location.object_key(checksum);
        self.runtime
            .block_on(async { self.put_bytes(&key, bytes).await })
    }

    fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        let key = self.location.manifest_key(id);
        // Mirror the manifest-write tail of `push`: render the oracle's
        // `echo "${manifest}"` bytes, verify they hash back to `id`, then PUT.
        let mut text = manifest.to_string();
        text.push('\n');
        let actual = Blake3Hasher::new().hash_hex(text.as_bytes());
        if actual != id {
            return Err(StoreError::Integrity {
                address: key,
                expected: id.to_owned(),
                actual,
            });
        }
        self.runtime
            .block_on(async { self.put_bytes(&key, text.into_bytes()).await })
    }
}

/// Builds the multi-thread tokio runtime that backs the sync bridge.
fn build_runtime() -> Result<Runtime, StoreError> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| backend("creating tokio runtime for GcsStore", e))
}

/// Installs the **`ring`** rustls [`CryptoProvider`] as the process default if
/// none is set yet. Idempotent: a second call (or a default installed by another
/// store) is a harmless no-op. This is the load-bearing piece that keeps
/// `aws-lc-rs` out of the dependency graph — the SDK's `reqwest`
/// (`rustls-no-provider`) consumes whatever process-default provider is set.
fn install_ring_provider() {
    // Ignore the error: it only means a provider was already installed, which is
    // exactly the state we want.
    let _ = rustls_ring::crypto::ring::default_provider().install_default();
}

/// Wraps any backend error into [`StoreError::Backend`] with a message.
fn backend<E>(message: &str, source: E) -> StoreError
where
    E: std::error::Error + Send + Sync + 'static,
{
    StoreError::Backend {
        message: message.to_owned(),
        source: Some(Box::new(source)),
    }
}

/// Classifies a `google-cloud-storage` SDK error as "object is absent".
///
/// The SDK reports a missing object two different ways, and the absent-object
/// paths (`key_exists` -> `Ok(false)`, `get_bytes` -> `Ok(None)`) must treat
/// BOTH as not-found:
///
/// 1. A plain **HTTP 404** (`http_status_code() == Some(404)`), e.g. from a
///    proxy/load balancer ahead of the service.
/// 2. A **service-level gRPC-style error** whose `status().code` is
///    [`Code::NotFound`] but whose `http_status_code()` is `None`. This is what
///    the v1.x SDK actually returns for `get_object`/`read_object` on a missing
///    object, and the form the original `== Some(404)`-only check misclassified
///    as a fatal backend error (it aborted `push` before the first upload).
///
/// This mirrors the SDK's own internal classification
/// (`e.status().is_some_and(|s| s.code == Code::NotFound)`), the GCS analogue of
/// the aws-sdk's `is_not_found()` used by [`S3Store`](crate::S3Store).
fn is_not_found(err: &GcsError) -> bool {
    err.http_status_code() == Some(404)
        || err
            .status()
            .is_some_and(|status| status.code == Code::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapdir_core::manifest::PathType;

    /// Strips a leading `./` and a trailing `/` from a manifest path. Kept as a
    /// test-only assertion of the path normalization the orchestrator
    /// (`crate::push`) performs.
    fn strip_leading_dot_slash(path: &str) -> &str {
        let trimmed = path.strip_prefix("./").unwrap_or(path);
        trimmed.strip_suffix('/').unwrap_or(trimmed)
    }

    // The canonical content-addressable fixtures (shared across the s3/gcs
    // store test suites).
    const FOO_CHECKSUM: &str = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
    const FOO_SHARDED: &str = "49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
    const MANIFEST_ID: &str = "aa91e498f401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d";
    const MANIFEST_SHARDED: &str =
        "aa9/1e4/98f/401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d";

    #[test]
    fn gcs_store_parses_bucket_and_prefix() {
        let loc = GcsLocation::parse("gs://my-bucket/long/term/storage");
        assert_eq!(loc.bucket, "my-bucket");
        assert_eq!(loc.prefix, "long/term/storage");
    }

    #[test]
    fn gcs_store_parse_matches_oracle_cut_and_sed() {
        // Oracle: bucket = `cut -d'/' -f3`; prefix = `sed -E 's|^gs:/*[^/]*/?||'`
        // then trailing-slash strip. For "gs://bucket/a/b/c": bucket=bucket,
        // prefix=a/b/c.
        let loc = GcsLocation::parse("gs://bucket/a/b/c");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "a/b/c");
    }

    #[test]
    fn gcs_store_parse_strips_trailing_slash() {
        // `_snapdir_gcs_store_get_remote_prefix` strips the trailing slash.
        let loc = GcsLocation::parse("gs://bucket/prefix/");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "prefix");
    }

    #[test]
    fn gcs_store_parse_bucket_root_has_empty_prefix() {
        let loc = GcsLocation::parse("gs://bucket");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "");

        let loc_slash = GcsLocation::parse("gs://bucket/");
        assert_eq!(loc_slash.bucket, "bucket");
        assert_eq!(loc_slash.prefix, "");
    }

    #[test]
    fn gcs_store_parse_accepts_bare_bucket_prefix_without_scheme() {
        let loc = GcsLocation::parse("bucket/some/prefix");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "some/prefix");
    }

    #[test]
    fn gcs_store_bucket_resource_uses_projects_underscore_form() {
        let loc = GcsLocation::parse("gs://my-bucket/x");
        assert_eq!(loc.bucket_resource(), "projects/_/buckets/my-bucket");
    }

    #[test]
    fn gcs_store_object_key_matches_sharded_scheme() {
        let loc = GcsLocation::parse("gs://b/long/term/storage");
        assert_eq!(
            loc.object_key(FOO_CHECKSUM),
            format!("long/term/storage/.objects/{FOO_SHARDED}")
        );
    }

    #[test]
    fn gcs_store_manifest_key_matches_sharded_scheme() {
        let loc = GcsLocation::parse("gs://b/long/term/storage");
        assert_eq!(
            loc.manifest_key(MANIFEST_ID),
            format!("long/term/storage/.manifests/{MANIFEST_SHARDED}")
        );
    }

    #[test]
    fn gcs_store_keys_have_no_leading_slash_at_bucket_root() {
        // With an empty prefix the keys are just `.objects/...` / `.manifests/...`.
        let loc = GcsLocation::parse("gs://bucket");
        assert_eq!(
            loc.object_key(FOO_CHECKSUM),
            format!(".objects/{FOO_SHARDED}")
        );
        assert_eq!(
            loc.manifest_key(MANIFEST_ID),
            format!(".manifests/{MANIFEST_SHARDED}")
        );
    }

    #[test]
    fn gcs_store_object_key_uses_core_object_path() {
        // Cross-check that we delegate to the frozen core sharding helper rather
        // than reimplementing it: at the bucket root the key equals the core
        // `object_path` output verbatim.
        let loc = GcsLocation::parse("gs://b");
        assert_eq!(loc.object_key(FOO_CHECKSUM), object_path(FOO_CHECKSUM));
        assert_eq!(loc.manifest_key(MANIFEST_ID), manifest_path(MANIFEST_ID));
    }

    #[test]
    fn gcs_store_strip_leading_dot_slash() {
        assert_eq!(strip_leading_dot_slash("./foo"), "foo");
        assert_eq!(strip_leading_dot_slash("./a/b/c"), "a/b/c");
        assert_eq!(strip_leading_dot_slash("./a/"), "a");
        assert_eq!(strip_leading_dot_slash("./"), "");
    }

    #[test]
    fn gcs_store_is_not_found_classifies_service_level_not_found_as_absent() {
        // Regression guard for the push-abort bug: the v1.x SDK reports a
        // missing object as a *service-level* gRPC error with code NOT_FOUND and
        // NO HTTP status code (`http_status_code() == None`). The original
        // `== Some(404)`-only check misclassified this as a fatal backend error,
        // so `key_exists` errored and `push` aborted before uploading anything.
        use google_cloud_gax::error::rpc::Status;

        let status = Status::default()
            .set_code(Code::NotFound)
            .set_message("No such object: bucket/.manifests/...");
        let err = GcsError::service(status);
        // This is the load-bearing assertion: the real-world shape carries no
        // HTTP code, so a 404-only check would (and did) miss it.
        assert_eq!(err.http_status_code(), None);
        assert!(
            is_not_found(&err),
            "service-level NOT_FOUND must be classified as object-absent"
        );
    }

    #[test]
    fn gcs_store_is_not_found_classifies_http_404_as_absent() {
        // The other absent shape: a plain HTTP 404 (e.g. from a proxy/LB ahead
        // of the service). Must also count as not-found.
        let err = GcsError::http(404, http::HeaderMap::new(), bytes::Bytes::new());
        assert!(is_not_found(&err), "HTTP 404 must be classified as absent");
    }

    #[test]
    fn gcs_store_is_not_found_does_not_swallow_other_errors() {
        // Guard the inverse: a non-not-found service error (e.g. PERMISSION
        // DENIED) and a non-404 HTTP error must NOT be treated as absent, so
        // real failures still surface instead of being silently skipped.
        use google_cloud_gax::error::rpc::Status;

        let denied = GcsError::service(Status::default().set_code(Code::PermissionDenied));
        assert!(!is_not_found(&denied), "PERMISSION_DENIED is not absence");

        let server_err = GcsError::http(503, http::HeaderMap::new(), bytes::Bytes::new());
        assert!(!is_not_found(&server_err), "HTTP 503 is not absence");
    }

    #[test]
    fn gcs_store_install_ring_provider_is_idempotent() {
        // Installing the ring provider twice must not panic; the second call is a
        // harmless no-op (a provider is already the process default).
        install_ring_provider();
        install_ring_provider();
    }

    // --- Live round-trip, skipped by default --------------------------------
    //
    // Requires real Google Cloud credentials (ADC) plus a writable bucket.
    // Gated behind `SNAPDIR_GCS_TEST_STORE` (a `gs://bucket/prefix` URL) so it is
    // skipped unless explicitly configured. Real round-trips are exercised by
    // the later `remote-interop` gate.
    #[test]
    fn gcs_store_live_round_trip_when_configured() {
        use snapdir_core::manifest::ManifestEntry;

        let Ok(store) = std::env::var("SNAPDIR_GCS_TEST_STORE") else {
            eprintln!(
                "skipping gcs_store live round-trip: set SNAPDIR_GCS_TEST_STORE \
                 (gs://bucket/prefix) plus ADC credentials to run it"
            );
            return;
        };

        let hasher = Blake3Hasher::new();

        // Build a tiny source tree + matching manifest.
        let src = std::env::temp_dir().join(format!("snapdir-gcs-live-{}", std::process::id()));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("foo"), b"foo\n").unwrap();
        let foo_sum = hasher.hash_hex(b"foo\n");
        let root_sum = snapdir_core::merkle::directory_checksum([foo_sum.as_str()], &hasher);
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
            foo_sum,
            4,
            "./foo",
        ));
        let manifest = Manifest::from_entries(manifest.entries().to_vec());
        let id = snapdir_core::merkle::snapshot_id(&manifest, &hasher);

        let gcs = GcsStore::connect(&store).expect("connect");
        gcs.push(&manifest, &src).expect("push");
        let read_back = gcs.get_manifest(&id).expect("get_manifest");
        assert_eq!(read_back, manifest);

        let dest = std::env::temp_dir().join(format!("snapdir-gcs-dest-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();
        gcs.fetch_files(&read_back, &dest).expect("fetch_files");
        assert_eq!(std::fs::read(dest.join("foo")).unwrap(), b"foo\n");

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dest);
    }
}
