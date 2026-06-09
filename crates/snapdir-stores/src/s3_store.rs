//! `S3Store`: the `s3://` storage backend, backed by the native AWS SDK.
//!
//! An [`S3Store`] targets an `s3://bucket/prefix` location and holds the frozen
//! content-addressable `.objects`/`.manifests` sharded layout, so a
//! bucket/prefix is interchangeable across conforming implementations:
//!
//! ```text
//! s3://<bucket>/<prefix>/.objects/<sharded checksum>     raw object bytes
//! s3://<bucket>/<prefix>/.manifests/<sharded snapshot id> manifest text
//! ```
//!
//! Sharding and the relative keys come straight from [`snapdir_core::store`]
//! ([`object_path`] / [`manifest_path`]); this module never reimplements them.
//!
//! # Credentials
//!
//! Authentication is delegated entirely to the standard AWS credential chain
//! via [`aws_config`] (environment variables, shared config/credentials
//! profiles, SSO, container/instance metadata, …). No bespoke snapdir
//! credential variables are introduced. An S3-compatible endpoint (`MinIO`,
//! `SeaweedFS`, …) can be selected with `SNAPDIR_S3_TEST_ENDPOINT` for the
//! gated live test, or by constructing the store with an explicit endpoint.
//!
//! # TLS provider (project-load-bearing)
//!
//! The shipped binary must statically link on musl, so the workspace
//! standardizes on the **`ring`** rustls provider; `aws-lc-rs` is banned. The
//! AWS SDK defaults to an aws-lc-rs-backed HTTP connector, so this module builds
//! the SDK's modern hyper-1.x HTTP client ([`aws_smithy_http_client`]) with its
//! `rustls`/**`ring`** TLS provider and hands it to the SDK as a custom
//! [`HttpClient`](aws_smithy_runtime_api::client::http::HttpClient). Native root
//! trust anchors stay on (the builder's default `TrustStore`).
//!
//! # Sync trait, async SDK
//!
//! The SDK is async. [`S3Store`] owns a private multi-thread `tokio` runtime and
//! bridges each [`Store`] method with `runtime.block_on(...)`, so no `async`
//! leaks into `snapdir-core` or the orchestrator (see [`snapdir_core::store`]).

use std::path::Path;
use std::sync::Arc;

use aws_config::BehaviorVersion;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::Client;
use aws_smithy_http_client::tls::rustls_provider::CryptoMode;
use aws_smithy_http_client::tls::Provider as TlsProvider;
use aws_smithy_http_client::Builder as HttpClientBuilder;
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use snapdir_core::manifest::Manifest;
use snapdir_core::merkle::{Blake3Hasher, Hasher};
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};
use snapdir_core::Meter;

use crate::fetch::fetch_files_concurrent;
use crate::push::{push_objects_concurrent, upload_object};
use crate::retry::{parse_retry_after, retry_network, Attempt, DefaultJitter, TokioSleeper};
use crate::stream::StreamStore;
use crate::transfer::{classify_error, RateLimiter, TransferConfig};

use std::error::Error as StdError;
use tokio::runtime::Runtime;

/// Number of times a fetch is retried when the downloaded bytes fail their
/// checksum, mirroring the oracle's `_SNAPDIR_S3_STORE_RETRIES` default of 5.
const MAX_FETCH_RETRIES: u32 = 5;

/// The parsed location an [`S3Store`] targets: an S3 bucket plus a key prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Location {
    /// The bucket name (first path segment of the `s3://` URL).
    pub bucket: String,
    /// The key prefix (remaining segments), with no leading or trailing slash.
    /// Empty when the store points at the bucket root.
    pub prefix: String,
}

impl S3Location {
    /// Parses an `s3://bucket/prefix` URL into its bucket and prefix.
    ///
    /// Matches the frozen URL derivation
    /// (`_snapdir_export_store_vars`): splitting the store URL on `/`, the
    /// bucket is `cut -f3` (the segment after `s3://`) and the base dir is
    /// `cut -f4-` (everything after). The prefix has any trailing slash
    /// stripped, matching `_snapdir_s3_store_get_remote_prefix`.
    ///
    /// The `s3://` scheme is optional; a bare `bucket/prefix` is accepted too.
    #[must_use]
    pub fn parse(store_url: &str) -> Self {
        // Drop the scheme (`s3://`, or any `<proto>://`) if present. The oracle
        // splits the full URL on `/` and takes field 3 as the bucket, which for
        // `s3://bucket/...` is exactly the segment after the `//`.
        let without_scheme = match store_url.find("://") {
            Some(idx) => &store_url[idx + 3..],
            None => store_url,
        };
        let mut parts = without_scheme.splitn(2, '/');
        let bucket = parts.next().unwrap_or("").to_owned();
        let prefix = parts.next().unwrap_or("");
        // Strip a trailing slash (and any leading slash from an empty first
        // segment edge case); the prefix is joined back with a single `/`.
        let prefix = prefix
            .trim_end_matches('/')
            .trim_start_matches('/')
            .to_owned();
        Self { bucket, prefix }
    }

    /// Returns the full S3 object key for a content object given its checksum,
    /// i.e. `<prefix>/.objects/<sharded>` (no leading slash).
    #[must_use]
    pub fn object_key(&self, checksum: &str) -> String {
        self.key_for(&object_path(checksum))
    }

    /// Returns the full S3 object key for a manifest given its snapshot id,
    /// i.e. `<prefix>/.manifests/<sharded>` (no leading slash).
    #[must_use]
    pub fn manifest_key(&self, id: &str) -> String {
        self.key_for(&manifest_path(id))
    }

    /// Joins the store prefix with a store-relative path (`.objects/...` or
    /// `.manifests/...`), producing a leading-slash-free S3 key. Mirrors the
    /// oracle's `${_SNAPDIR_STORE_BASE_DIR%/}/${source_path#/}` with the
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

/// A content-addressable store backed by an S3 (or S3-compatible) bucket.
///
/// Construct one with [`S3Store::connect`] (resolves the standard AWS
/// credential chain) or [`S3Store::from_client`] (an already-built SDK client,
/// e.g. for tests against an emulator).
pub struct S3Store {
    client: Client,
    location: S3Location,
    runtime: Arc<Runtime>,
    config: TransferConfig,
    /// Per-call request-rate limiter (one token per SDK call), built from
    /// [`TransferConfig::max_requests_per_sec`]. Unlimited (a no-op) by default.
    /// Shared (clone) so every call paces against one aggregate request budget.
    req_limiter: RateLimiter,
    /// Optional progress meter; recorded into during transfers. `None` (the
    /// default from every constructor) means zero recording and byte-identical
    /// behavior. Set by the CLI via [`S3Store::with_meter`].
    meter: Option<Arc<Meter>>,
}

impl S3Store {
    /// Connects to the `s3://bucket/prefix` store, resolving credentials and
    /// region via the standard AWS chain ([`aws_config::load_defaults`]).
    ///
    /// The HTTP client is pinned to the `ring` rustls provider (see the module
    /// docs). An optional `endpoint_url` selects an S3-compatible backend
    /// (path-style addressing is enabled when an endpoint is given, as
    /// emulators rarely support virtual-host addressing).
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] if the tokio runtime cannot be created or the
    /// AWS configuration cannot be loaded.
    pub fn connect(store_url: &str, endpoint_url: Option<&str>) -> Result<Self, StoreError> {
        Self::connect_with(store_url, endpoint_url, TransferConfig::default())
    }

    /// Like [`connect`](Self::connect), but carries a [`TransferConfig`] for
    /// concurrency / bandwidth control. The existing [`connect`](Self::connect)
    /// delegates here with [`TransferConfig::default`].
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] if the tokio runtime cannot be created or the
    /// AWS configuration cannot be loaded.
    pub fn connect_with(
        store_url: &str,
        endpoint_url: Option<&str>,
        config: TransferConfig,
    ) -> Result<Self, StoreError> {
        let location = S3Location::parse(store_url);
        let runtime = build_runtime()?;

        let http_client = ring_https_client();
        let endpoint = endpoint_url.map(ToOwned::to_owned);
        let client = runtime.block_on(async move {
            let mut loader = aws_config::defaults(BehaviorVersion::latest())
                .http_client(http_client.clone())
                // Disable the SDK's own retry loop so snapdir's RetryPolicy is
                // the single backoff authority (no SDK-3x × ours-5x compounding).
                .retry_config(aws_config::retry::RetryConfig::disabled());
            if let Some(ep) = endpoint.as_deref() {
                loader = loader.endpoint_url(ep);
            }
            let shared = loader.load().await;
            let mut builder = aws_sdk_s3::config::Builder::from(&shared);
            if endpoint.is_some() {
                // S3-compatible emulators generally require path-style keys.
                builder = builder.force_path_style(true);
            }
            // Some emulators / configs leave the region unset; S3 still
            // requires a value to sign requests, so default it.
            if shared.region().is_none() {
                builder = builder.region(Region::new("us-east-1"));
            }
            Client::from_conf(builder.build())
        });

        let req_limiter = RateLimiter::new(config.max_requests_per_sec);
        Ok(Self {
            client,
            location,
            runtime: Arc::new(runtime),
            config,
            req_limiter,
            meter: None,
        })
    }

    /// Builds a store from an already-configured SDK [`Client`] and a parsed
    /// location, owning a fresh tokio runtime for the sync bridge. Intended for
    /// tests (e.g. wiring a client at an emulator endpoint).
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] if the tokio runtime cannot be created.
    pub fn from_client(client: Client, location: S3Location) -> Result<Self, StoreError> {
        let config = TransferConfig::default();
        let req_limiter = RateLimiter::new(config.max_requests_per_sec);
        Ok(Self {
            client,
            location,
            runtime: Arc::new(build_runtime()?),
            config,
            req_limiter,
            meter: None,
        })
    }

    /// Attaches (or clears) an optional progress [`Meter`], rides alongside
    /// [`config`](Self::transfer_config). The transfer paths record bytes-in /
    /// bytes-out + per-object progress into it; `None` (the constructor default)
    /// means zero recording and byte-identical behavior. The CLI sets this after
    /// construction.
    #[must_use]
    pub fn with_meter(mut self, meter: Option<Arc<Meter>>) -> Self {
        self.meter = meter;
        self
    }

    /// The parsed bucket/prefix this store targets.
    #[must_use]
    pub fn location(&self) -> &S3Location {
        &self.location
    }

    /// The [`TransferConfig`] (concurrency / bandwidth) this store was built
    /// with. Consumed by the transfer loops in later gates.
    #[must_use]
    pub fn transfer_config(&self) -> &TransferConfig {
        &self.config
    }

    /// HEAD an object key; `Ok(true)` if it exists, `Ok(false)` if absent.
    ///
    /// The single SDK call is wrapped in [`retry_network`]: it acquires one
    /// request-rate token, then retries a TRANSIENT failure under the store's
    /// [`RetryPolicy`]. A `404`/not-found is a normal `Ok(false)` outcome (not a
    /// retry); any other error is classified via [`s3_attempt_from_err`].
    async fn key_exists(&self, key: &str) -> Result<bool, StoreError> {
        retry_network(
            &self.config.retry,
            &self.req_limiter,
            &TokioSleeper,
            &DefaultJitter::new(),
            || async {
                match self
                    .client
                    .head_object()
                    .bucket(&self.location.bucket)
                    .key(key)
                    .send()
                    .await
                {
                    Ok(_) => Ok(true),
                    Err(err) => {
                        // A genuine "absent" is a successful outcome of the op,
                        // not a retry: peek the concrete service error first.
                        if err.as_service_error().is_some_and(
                            aws_sdk_s3::operation::head_object::HeadObjectError::is_not_found,
                        ) {
                            return Ok(false);
                        }
                        Err(s3_attempt_from_err("S3 HEAD object failed", err))
                    }
                }
            },
        )
        .await
    }

    /// GET an object key's full body, or `None` if it is absent.
    ///
    /// The SDK call is wrapped in [`retry_network`] exactly like
    /// [`key_exists`](Self::key_exists). A `NoSuchKey` is a normal `Ok(None)`
    /// outcome; transient failures retry under the store's [`RetryPolicy`](crate::retry::RetryPolicy).
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        retry_network(
            &self.config.retry,
            &self.req_limiter,
            &TokioSleeper,
            &DefaultJitter::new(),
            || async {
                match self
                    .client
                    .get_object()
                    .bucket(&self.location.bucket)
                    .key(key)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        // Draining the streamed body can itself fail transiently
                        // (connection reset mid-download); classify it too.
                        let data = resp.body.collect().await.map_err(|e| {
                            let err = backend("reading S3 object body", e);
                            let transient =
                                matches!(classify_error(&err), crate::adaptive::OpResult::Throttle);
                            Attempt {
                                transient,
                                retry_after: None,
                                err,
                            }
                        })?;
                        Ok(Some(data.into_bytes().to_vec()))
                    }
                    Err(err) => {
                        if err.as_service_error().is_some_and(
                            aws_sdk_s3::operation::get_object::GetObjectError::is_no_such_key,
                        ) {
                            return Ok(None);
                        }
                        Err(s3_attempt_from_err("S3 GET object failed", err))
                    }
                }
            },
        )
        .await
    }

    /// PUT `bytes` at `key`. S3 PUT is atomic, so no temp-key dance is needed
    /// (the oracle relies on the same atomicity for manifests/objects).
    ///
    /// Wrapped in [`retry_network`]: each (re)try re-sends the full body, so the
    /// content-addressed bytes that land are unchanged — only transient failures
    /// retry under the store's [`RetryPolicy`](crate::retry::RetryPolicy).
    async fn put_bytes(&self, key: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        retry_network(
            &self.config.retry,
            &self.req_limiter,
            &TokioSleeper,
            &DefaultJitter::new(),
            || {
                let bytes = bytes.clone();
                async move {
                    self.client
                        .put_object()
                        .bucket(&self.location.bucket)
                        .key(key)
                        .body(bytes.into())
                        .send()
                        .await
                        .map(|_| ())
                        .map_err(|err| s3_attempt_from_err("S3 PUT object failed", err))
                }
            },
        )
        .await
    }

    /// Downloads `key`, verifying its BLAKE3 against `expected`, retrying up to
    /// [`MAX_FETCH_RETRIES`] times. Mirrors `_snapdir_s3_fetch_to_cache`.
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
                            address: format!("s3://{}/{key}", self.location.bucket),
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

impl Store for S3Store {
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
        // write. S3 only injects the per-object download, preserving the
        // BLAKE3-verify + retry discipline of `fetch_verified`.
        let limiter = RateLimiter::new(self.config.max_bytes_per_sec);
        let meter = self.meter.as_deref();
        let meter_arc = self.meter.clone();
        self.runtime.block_on(async {
            fetch_files_concurrent(
                manifest,
                dest,
                &self.config,
                &limiter,
                meter,
                meter_arc,
                |entry| async {
                    let key = self.location.object_key(&entry.checksum);
                    self.fetch_verified(&key, &entry.checksum).await
                },
            )
            .await
        })
    }

    fn push(&self, manifest: &Manifest, source: &Path) -> Result<(), StoreError> {
        let hasher = Blake3Hasher::new();
        let id = snapdir_core::merkle::snapshot_id(manifest, &hasher);

        // Concurrent upload via the shared orchestrator: it owns the bounded
        // per-object pass and the manifest-last / all-or-nothing ordering. S3
        // injects the per-object skip-present + upload (via `upload_object`,
        // which also owns the shared read+verify) and the manifest-write
        // closure. A failed push writes NO manifest.
        let limiter = RateLimiter::new(self.config.max_bytes_per_sec);
        let meter = self.meter.as_deref();
        let meter_arc = self.meter.clone();
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
                &limiter,
                meter,
                meter_arc,
                |entry| {
                    let object_key = self.location.object_key(&entry.checksum);
                    upload_object(
                        entry,
                        object_key,
                        source,
                        &limiter,
                        meter,
                        |key| async move { self.key_exists(&key).await },
                        |key, bytes| async move { self.put_bytes(&key, bytes).await },
                    )
                },
                || async {
                    // Write the manifest last (verified to hash back to its id),
                    // exactly as the oracle stores `echo "${manifest}"` text.
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

impl StreamStore for S3Store {
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
                address: format!("s3://{}/{key}", self.location.bucket),
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
        .map_err(|e| backend("creating tokio runtime for S3Store", e))
}

/// Builds the AWS-SDK hyper-1.x HTTP client backed by `rustls` using the
/// **`ring`** crypto provider, with native-root trust anchors (the builder's
/// default `TrustStore` enables them). This is the load-bearing piece that keeps
/// `aws-lc-rs` (and the legacy hyper-0.14 TLS island) out of the dependency
/// graph.
fn ring_https_client() -> aws_smithy_runtime_api::client::http::SharedHttpClient {
    HttpClientBuilder::new()
        .tls_provider(TlsProvider::Rustls(CryptoMode::Ring))
        .build_https()
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

/// Builds a retry [`Attempt`] from a concrete aws-sdk-s3 [`SdkError`], at the
/// boundary where the SDK error still carries its raw HTTP response.
///
/// - `transient`: true for the retryable signal set — an HTTP `429`/`503`
///   status on the raw response, the throttle error codes
///   (`SlowDown`/`Throttling`/`RequestTimeout`/`ServiceUnavailable`/…), and the
///   transport/timeout classes — folded through the shared
///   [`classify_error`](crate::classify_error) on the mapped [`StoreError`].
///   Conservative: an unknown error is NOT transient.
/// - `retry_after`: extracted from the raw response's `Retry-After` header (the
///   delta-seconds form) when present, else `None`.
/// - `err`: the mapped [`StoreError::Backend`] (the same value the non-retry
///   path used to surface).
fn s3_attempt_from_err<E>(message: &str, err: SdkError<E, HttpResponse>) -> Attempt
where
    E: ProvideErrorMetadata + StdError + Send + Sync + 'static,
{
    // Extract status code + Retry-After off the raw HTTP response (present on
    // ServiceError / ResponseError variants) BEFORE we consume the error.
    let (http_status, retry_after) = match err.raw_response() {
        Some(resp) => {
            let status = resp.status().as_u16();
            let hint = resp
                .headers()
                .get("retry-after")
                .and_then(parse_retry_after);
            (Some(status), hint)
        }
        None => (None, None),
    };

    // The SDK error code (e.g. "SlowDown", "ServiceUnavailable") rides in the
    // error metadata; fold it (plus the status) into the mapped StoreError's
    // text so the shared classifier sees the transient signals.
    let code = err.code().unwrap_or_default().to_owned();
    let store_err = backend(message, err);

    let transient = http_status.is_some_and(|s| s == 429 || s == 503)
        || matches!(
            classify_error(&store_err),
            crate::adaptive::OpResult::Throttle
        )
        || {
            let c = code.to_ascii_lowercase();
            c.contains("slowdown")
                || c.contains("throttl")
                || c.contains("requesttimeout")
                || c.contains("serviceunavailable")
                || c.contains("internalerror")
        };

    Attempt {
        transient,
        retry_after,
        err: store_err,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::error::ErrorMetadata;
    use aws_sdk_s3::operation::head_object::HeadObjectError;
    use aws_sdk_s3::primitives::SdkBody;
    use aws_smithy_runtime_api::http::StatusCode;
    use snapdir_core::manifest::PathType;
    use std::time::Duration;

    /// Builds a raw S3 [`HttpResponse`] with the given status and optional
    /// `Retry-After` header, for exercising [`s3_attempt_from_err`] without a
    /// live SDK call.
    fn raw_response(status: u16, retry_after: Option<&str>) -> HttpResponse {
        let mut resp = HttpResponse::new(
            StatusCode::try_from(status).expect("valid status"),
            SdkBody::empty(),
        );
        if let Some(v) = retry_after {
            resp.headers_mut().insert("retry-after", v.to_owned());
        }
        resp
    }

    /// Wraps an S3 service error (carrying `code`) plus a raw response into the
    /// concrete `SdkError` the store methods see, so the extractor runs on a
    /// real SDK error shape.
    fn s3_service_error(code: &str, status: u16, retry_after: Option<&str>) -> Attempt {
        let meta = ErrorMetadata::builder().code(code).build();
        let svc = HeadObjectError::generic(meta);
        let err = SdkError::service_error(svc, raw_response(status, retry_after));
        s3_attempt_from_err("S3 op failed", err)
    }

    #[test]
    fn backoff_wire_s3_extract_503_retry_after_is_transient_with_hint() {
        // A 503 SlowDown carrying `Retry-After: 12` => transient, hint = 12s.
        let attempt = s3_service_error("SlowDown", 503, Some("12"));
        assert!(attempt.transient, "503/SlowDown must be transient");
        assert_eq!(
            attempt.retry_after,
            Some(Duration::from_secs(12)),
            "the Retry-After delta-seconds header must be extracted"
        );
    }

    #[test]
    fn backoff_wire_s3_extract_429_without_header_is_transient_no_hint() {
        // A 429 with no Retry-After header => still transient, but no hint.
        let attempt = s3_service_error("Throttling", 429, None);
        assert!(attempt.transient, "429 must be transient");
        assert_eq!(
            attempt.retry_after, None,
            "absent Retry-After header => None (backoff handles the delay)"
        );
    }

    #[test]
    fn backoff_wire_s3_extract_404_is_not_transient() {
        // A 404 NoSuchKey-style hard error => NOT transient (conservative).
        let attempt = s3_service_error("NoSuchKey", 404, None);
        assert!(
            !attempt.transient,
            "a 404/not-found must never be classified transient"
        );
        assert_eq!(attempt.retry_after, None);
    }

    /// Strips a leading `./` and a trailing `/` from a manifest path. Kept as a
    /// test-only assertion of the path normalization the orchestrator
    /// (`crate::push`) performs.
    fn strip_leading_dot_slash(path: &str) -> &str {
        let trimmed = path.strip_prefix("./").unwrap_or(path);
        trimmed.strip_suffix('/').unwrap_or(trimmed)
    }

    // The canonical content-addressable fixtures from the s3 store test suite.
    const FOO_CHECKSUM: &str = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
    const FOO_SHARDED: &str = "49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
    const MANIFEST_ID: &str = "aa91e498f401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d";
    const MANIFEST_SHARDED: &str =
        "aa9/1e4/98f/401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d";

    #[test]
    fn s3_store_parses_bucket_and_prefix() {
        let loc = S3Location::parse("s3://my-bucket/long/term/storage");
        assert_eq!(loc.bucket, "my-bucket");
        assert_eq!(loc.prefix, "long/term/storage");
    }

    #[test]
    fn s3_store_parse_matches_oracle_cut_fields() {
        // Oracle: bucket = `cut -d'/' -f3`, base_dir = `cut -d'/' -f4-`.
        // For "s3://bucket/a/b/c": fields are [s3:,"",bucket,a,b,c].
        let loc = S3Location::parse("s3://bucket/a/b/c");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "a/b/c");
    }

    #[test]
    fn s3_store_parse_strips_trailing_slash() {
        // `_snapdir_s3_store_get_remote_prefix` strips the trailing slash.
        let loc = S3Location::parse("s3://bucket/prefix/");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "prefix");
    }

    #[test]
    fn s3_store_parse_bucket_root_has_empty_prefix() {
        let loc = S3Location::parse("s3://bucket");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "");

        let loc_slash = S3Location::parse("s3://bucket/");
        assert_eq!(loc_slash.bucket, "bucket");
        assert_eq!(loc_slash.prefix, "");
    }

    #[test]
    fn s3_store_parse_accepts_bare_bucket_prefix_without_scheme() {
        let loc = S3Location::parse("bucket/some/prefix");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "some/prefix");
    }

    #[test]
    fn s3_store_object_key_matches_sharded_scheme() {
        let loc = S3Location::parse("s3://b/long/term/storage");
        assert_eq!(
            loc.object_key(FOO_CHECKSUM),
            format!("long/term/storage/.objects/{FOO_SHARDED}")
        );
    }

    #[test]
    fn s3_store_manifest_key_matches_sharded_scheme() {
        let loc = S3Location::parse("s3://b/long/term/storage");
        assert_eq!(
            loc.manifest_key(MANIFEST_ID),
            format!("long/term/storage/.manifests/{MANIFEST_SHARDED}")
        );
    }

    #[test]
    fn s3_store_keys_have_no_leading_slash_at_bucket_root() {
        // With an empty prefix the keys are just `.objects/...` / `.manifests/...`.
        let loc = S3Location::parse("s3://bucket");
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
    fn s3_store_object_key_uses_core_object_path() {
        // Cross-check that we delegate to the frozen core sharding helper rather
        // than reimplementing it: the key tail must equal `object_path` output.
        let loc = S3Location::parse("s3://b");
        assert_eq!(loc.object_key(FOO_CHECKSUM), object_path(FOO_CHECKSUM));
    }

    #[test]
    fn s3_store_strip_leading_dot_slash() {
        assert_eq!(strip_leading_dot_slash("./foo"), "foo");
        assert_eq!(strip_leading_dot_slash("./a/b/c"), "a/b/c");
        assert_eq!(strip_leading_dot_slash("./a/"), "a");
        assert_eq!(strip_leading_dot_slash("./"), "");
    }

    // --- Live round-trip, skipped by default --------------------------------
    //
    // Requires an S3-compatible endpoint (e.g. MinIO/SeaweedFS) plus AWS
    // credentials in the environment. Gated behind `SNAPDIR_S3_TEST_ENDPOINT`
    // and `SNAPDIR_S3_TEST_STORE` (an `s3://bucket/prefix` URL) so it is skipped
    // unless explicitly configured. Real emulator round-trips are exercised by
    // the later `remote-interop` gate.
    #[test]
    fn s3_store_live_round_trip_when_configured() {
        use snapdir_core::manifest::ManifestEntry;

        let (Ok(endpoint), Ok(store)) = (
            std::env::var("SNAPDIR_S3_TEST_ENDPOINT"),
            std::env::var("SNAPDIR_S3_TEST_STORE"),
        ) else {
            eprintln!(
                "skipping s3_store live round-trip: set SNAPDIR_S3_TEST_ENDPOINT \
                 and SNAPDIR_S3_TEST_STORE (s3://bucket/prefix) to run it"
            );
            return;
        };

        let hasher = Blake3Hasher::new();

        // Build a tiny source tree + matching manifest.
        let src = std::env::temp_dir().join(format!("snapdir-s3-live-{}", std::process::id()));
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

        let s3 = S3Store::connect(&store, Some(&endpoint)).expect("connect");
        s3.push(&manifest, &src).expect("push");
        let read_back = s3.get_manifest(&id).expect("get_manifest");
        assert_eq!(read_back, manifest);

        let dest = std::env::temp_dir().join(format!("snapdir-s3-dest-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();
        s3.fetch_files(&read_back, &dest).expect("fetch_files");
        assert_eq!(std::fs::read(dest.join("foo")).unwrap(), b"foo\n");

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dest);
    }
}
