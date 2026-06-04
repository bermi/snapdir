//! `B2Store`: the `b2://` storage backend, backed by Backblaze B2's
//! **S3-compatible endpoint** via the native AWS SDK.
//!
//! Backblaze B2 exposes an S3-compatible API at a per-region endpoint of the
//! form `https://s3.<region>.backblazeb2.com` (for example
//! `https://s3.us-west-004.backblazeb2.com`). A [`B2Store`] is therefore just an
//! [`S3Store`] pointed at that custom endpoint URL with path-style addressing,
//! so it reuses the entire S3 transfer path — the same core-sharded
//! `.objects`/`.manifests` keys, the same push (objects-before-manifest,
//! skip-if-present) and fetch (download → verify BLAKE3 → retry → atomic write)
//! discipline. This module adds only the `b2://` URL handling and the
//! endpoint/region derivation; it does **not** duplicate the store logic.
//!
//! ```text
//! b2://<bucket>/<prefix>/.objects/<sharded checksum>      raw object bytes
//! b2://<bucket>/<prefix>/.manifests/<sharded snapshot id> manifest text
//! ```
//!
//! # URL parsing (frozen contract)
//!
//! `b2://bucket/base/dir` parses exactly like `s3://...`
//! (`_snapdir_export_store_vars`): the bucket is the segment after the `//`
//! (`cut -d'/' -f3`) and the prefix is everything after it (`cut -d'/' -f4-`)
//! with a trailing slash stripped (matching `_snapdir_b2_store_get_remote_prefix`).
//! This reuses [`S3Location::parse`] verbatim, since the derivation is identical.
//!
//! # Credentials
//!
//! Authentication is delegated to the standard AWS credential chain (see
//! [`S3Store`]). The Backblaze **application key id** maps to the AWS access key
//! id and the **application key** maps to the AWS secret access key — i.e. the
//! usual `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` env vars, a profile, etc.
//! No bespoke snapdir credential variables are introduced. (The original
//! implementation shelled out to the `b2` CLI and read `B2_APPLICATION_KEY[_ID]`;
//! the native S3-compatible path uses the AWS chain instead, which is what the
//! SDK expects.)
//!
//! # Endpoint / region derivation
//!
//! The S3-compatible endpoint encodes the B2 region. It is resolved, in order:
//!
//! 1. an explicit `endpoint_url` passed to [`B2Store::connect`];
//! 2. the `SNAPDIR_B2_TEST_ENDPOINT` environment variable (used to point the
//!    gated live test at a real bucket or an S3 emulator);
//! 3. derived from a region — an explicit `region` argument, else the
//!    `SNAPDIR_B2_REGION` / `AWS_REGION` env vars — as
//!    `https://s3.<region>.backblazeb2.com`.
//!
//! When the endpoint is derived from a region the SDK is also told that region
//! so `SigV4` signing matches Backblaze's expectation.

use std::path::Path;

use std::sync::Arc;

use snapdir_core::manifest::Manifest;
use snapdir_core::store::{Store, StoreError};
use snapdir_core::Meter;

use crate::s3_store::{S3Location, S3Store};
use crate::stream::StreamStore;
use crate::transfer::TransferConfig;

/// The default Backblaze B2 region used when none is configured. Backblaze
/// requires a region in the S3-compatible endpoint host; `us-west-004` is a
/// common default, but a real deployment should set `SNAPDIR_B2_REGION` /
/// `AWS_REGION` (or pass one explicitly) to match its bucket's region.
const DEFAULT_B2_REGION: &str = "us-west-004";

/// Builds the Backblaze S3-compatible endpoint URL for a region, i.e.
/// `https://s3.<region>.backblazeb2.com`.
#[must_use]
pub fn endpoint_for_region(region: &str) -> String {
    format!("https://s3.{region}.backblazeb2.com")
}

/// A content-addressable store backed by Backblaze B2 via its S3-compatible
/// endpoint. Thin wrapper over [`S3Store`] configured with the B2 endpoint.
pub struct B2Store {
    inner: S3Store,
}

impl B2Store {
    /// Connects to a `b2://bucket/prefix` store using Backblaze's
    /// S3-compatible API.
    ///
    /// `endpoint_url` overrides the endpoint outright (handy for emulators or
    /// an already-known regional host). When `None`, the endpoint is taken from
    /// `SNAPDIR_B2_TEST_ENDPOINT`, and failing that derived from `region` (or
    /// the `SNAPDIR_B2_REGION` / `AWS_REGION` env vars, else
    /// [`DEFAULT_B2_REGION`]) as `https://s3.<region>.backblazeb2.com`.
    ///
    /// Credentials and signing are handled by the standard AWS chain; the B2
    /// application key id/secret map to the AWS access-key/secret-key.
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] if the tokio runtime cannot be created or the
    /// AWS configuration cannot be loaded (propagated from [`S3Store::connect`]).
    pub fn connect(
        store_url: &str,
        endpoint_url: Option<&str>,
        region: Option<&str>,
    ) -> Result<Self, StoreError> {
        Self::connect_with(store_url, endpoint_url, region, TransferConfig::default())
    }

    /// Like [`connect`](Self::connect), but carries a [`TransferConfig`] for
    /// concurrency / bandwidth control. The config lives on the wrapped
    /// [`S3Store`]; [`connect`](Self::connect) delegates here with
    /// [`TransferConfig::default`].
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] if the tokio runtime cannot be created or the
    /// AWS configuration cannot be loaded (propagated from
    /// [`S3Store::connect_with`]).
    pub fn connect_with(
        store_url: &str,
        endpoint_url: Option<&str>,
        region: Option<&str>,
        config: TransferConfig,
    ) -> Result<Self, StoreError> {
        let endpoint = resolve_endpoint(endpoint_url, region);
        // S3Store::connect parses the URL with S3Location::parse, which derives
        // bucket/prefix identically for b2:// and s3:// (oracle cut -f3 / -f4-).
        let inner = S3Store::connect_with(store_url, Some(endpoint.as_str()), config)?;
        Ok(Self { inner })
    }

    /// Builds a `B2Store` from an already-configured [`S3Store`] (intended for
    /// tests wiring a client at an emulator/B2 endpoint).
    #[must_use]
    pub fn from_s3_store(inner: S3Store) -> Self {
        Self { inner }
    }

    /// Attaches (or clears) an optional progress [`Meter`]. B2 has no transfer
    /// path of its own — it delegates entirely to the wrapped [`S3Store`] — so
    /// this forwards to [`S3Store::with_meter`]. `None` (the constructor default)
    /// means zero recording and byte-identical behavior.
    #[must_use]
    pub fn with_meter(mut self, meter: Option<Arc<Meter>>) -> Self {
        self.inner = self.inner.with_meter(meter);
        self
    }

    /// The parsed bucket/prefix this store targets (shared with [`S3Store`]).
    #[must_use]
    pub fn location(&self) -> &S3Location {
        self.inner.location()
    }

    /// The [`TransferConfig`] (concurrency / bandwidth) this store was built
    /// with, carried on the wrapped [`S3Store`]. Consumed by the transfer loops
    /// in later gates.
    #[must_use]
    pub fn transfer_config(&self) -> &TransferConfig {
        self.inner.transfer_config()
    }
}

impl Store for B2Store {
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

impl StreamStore for B2Store {
    fn has_object(&self, checksum: &str) -> Result<bool, StoreError> {
        self.inner.has_object(checksum)
    }

    fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
        self.inner.get_object(checksum)
    }

    fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        self.inner.put_object(checksum, bytes)
    }

    fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        self.inner.put_manifest(id, manifest)
    }
}

/// Resolves the S3-compatible endpoint to use, applying the precedence
/// documented on [`B2Store::connect`]: explicit endpoint > `SNAPDIR_B2_TEST_ENDPOINT`
/// > endpoint derived from the resolved region.
fn resolve_endpoint(endpoint_url: Option<&str>, region: Option<&str>) -> String {
    if let Some(ep) = endpoint_url {
        return ep.to_owned();
    }
    if let Ok(ep) = std::env::var("SNAPDIR_B2_TEST_ENDPOINT") {
        if !ep.is_empty() {
            return ep;
        }
    }
    endpoint_for_region(&resolve_region(region))
}

/// Resolves the B2 region: an explicit argument, else `SNAPDIR_B2_REGION`, else
/// `AWS_REGION`, else [`DEFAULT_B2_REGION`].
fn resolve_region(region: Option<&str>) -> String {
    if let Some(r) = region {
        if !r.is_empty() {
            return r.to_owned();
        }
    }
    for var in ["SNAPDIR_B2_REGION", "AWS_REGION"] {
        if let Ok(r) = std::env::var(var) {
            if !r.is_empty() {
                return r;
            }
        }
    }
    DEFAULT_B2_REGION.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The canonical content-addressable fixtures from the b2 store test suite.
    const FOO_CHECKSUM: &str = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
    const FOO_SHARDED: &str = "49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
    const MANIFEST_ID: &str = "aa91e498f401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d";
    const MANIFEST_SHARDED: &str =
        "aa9/1e4/98f/401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d";

    #[test]
    fn b2_store_parses_bucket_and_prefix_like_oracle() {
        // Oracle (`_snapdir_export_store_vars`): bucket = cut -f3,
        // base_dir = cut -f4-. For "b2://my-bucket/my/directory".
        let loc = S3Location::parse("b2://my-bucket/my/directory");
        assert_eq!(loc.bucket, "my-bucket");
        assert_eq!(loc.prefix, "my/directory");
    }

    #[test]
    fn b2_store_parse_strips_trailing_slash() {
        // `_snapdir_b2_store_get_remote_prefix` strips the trailing slash.
        let loc = S3Location::parse("b2://bucket/long/term/storage/");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "long/term/storage");
    }

    #[test]
    fn b2_store_parse_bucket_root_has_empty_prefix() {
        let loc = S3Location::parse("b2://bucket");
        assert_eq!(loc.bucket, "bucket");
        assert_eq!(loc.prefix, "");

        let loc_slash = S3Location::parse("b2://bucket/");
        assert_eq!(loc_slash.bucket, "bucket");
        assert_eq!(loc_slash.prefix, "");
    }

    #[test]
    fn b2_store_object_key_matches_sharded_scheme() {
        // Key layout must be byte-identical to the frozen S3 sharded scheme so
        // the bucket is interchangeable across tools.
        let loc = S3Location::parse("b2://b/long/term/storage");
        assert_eq!(
            loc.object_key(FOO_CHECKSUM),
            format!("long/term/storage/.objects/{FOO_SHARDED}")
        );
    }

    #[test]
    fn b2_store_manifest_key_matches_sharded_scheme() {
        let loc = S3Location::parse("b2://b/long/term/storage");
        assert_eq!(
            loc.manifest_key(MANIFEST_ID),
            format!("long/term/storage/.manifests/{MANIFEST_SHARDED}")
        );
    }

    #[test]
    fn b2_store_keys_have_no_leading_slash_at_bucket_root() {
        let loc = S3Location::parse("b2://bucket");
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
    fn b2_store_endpoint_for_region_uses_backblaze_host() {
        assert_eq!(
            endpoint_for_region("us-west-004"),
            "https://s3.us-west-004.backblazeb2.com"
        );
        assert_eq!(
            endpoint_for_region("eu-central-003"),
            "https://s3.eu-central-003.backblazeb2.com"
        );
    }

    #[test]
    fn b2_store_explicit_endpoint_takes_precedence() {
        let ep = resolve_endpoint(Some("https://emulator.local:9000"), Some("us-west-004"));
        assert_eq!(ep, "https://emulator.local:9000");
    }

    #[test]
    fn b2_store_endpoint_derived_from_explicit_region() {
        // With no explicit endpoint and no SNAPDIR_B2_TEST_ENDPOINT set, the
        // endpoint is derived from the explicit region argument.
        std::env::remove_var("SNAPDIR_B2_TEST_ENDPOINT");
        let ep = resolve_endpoint(None, Some("us-west-002"));
        assert_eq!(ep, "https://s3.us-west-002.backblazeb2.com");
    }

    #[test]
    fn b2_store_region_resolution_prefers_explicit_then_default() {
        // Explicit region wins.
        assert_eq!(resolve_region(Some("eu-central-003")), "eu-central-003");
        // Empty explicit region falls through; with no env override set we get
        // the documented default (guard the env to keep the test hermetic).
        let saved_b2 = std::env::var("SNAPDIR_B2_REGION").ok();
        let saved_aws = std::env::var("AWS_REGION").ok();
        std::env::remove_var("SNAPDIR_B2_REGION");
        std::env::remove_var("AWS_REGION");
        assert_eq!(resolve_region(Some("")), DEFAULT_B2_REGION);
        assert_eq!(resolve_region(None), DEFAULT_B2_REGION);
        if let Some(v) = saved_b2 {
            std::env::set_var("SNAPDIR_B2_REGION", v);
        }
        if let Some(v) = saved_aws {
            std::env::set_var("AWS_REGION", v);
        }
    }

    // --- Live round-trip, skipped by default --------------------------------
    //
    // Requires a Backblaze B2 (or S3-compatible) endpoint plus AWS credentials
    // (the B2 application key id/secret as AWS access-key/secret-key) in the
    // environment. Gated behind `SNAPDIR_B2_TEST_ENDPOINT` and
    // `SNAPDIR_B2_TEST_STORE` (a `b2://bucket/prefix` URL) so it is skipped
    // unless explicitly configured. Real Backblaze round-trips are exercised by
    // the later `remote-interop` gate.
    #[test]
    fn b2_store_live_round_trip_when_configured() {
        use snapdir_core::manifest::{ManifestEntry, PathType};
        use snapdir_core::merkle::{Blake3Hasher, Hasher};

        let (Ok(endpoint), Ok(store)) = (
            std::env::var("SNAPDIR_B2_TEST_ENDPOINT"),
            std::env::var("SNAPDIR_B2_TEST_STORE"),
        ) else {
            eprintln!(
                "skipping b2_store live round-trip: set SNAPDIR_B2_TEST_ENDPOINT \
                 and SNAPDIR_B2_TEST_STORE (b2://bucket/prefix) to run it"
            );
            return;
        };

        let hasher = Blake3Hasher::new();

        let src = std::env::temp_dir().join(format!("snapdir-b2-live-{}", std::process::id()));
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

        let b2 = B2Store::connect(&store, Some(&endpoint), None).expect("connect");
        b2.push(&manifest, &src).expect("push");
        let read_back = b2.get_manifest(&id).expect("get_manifest");
        assert_eq!(read_back, manifest);

        let dest = std::env::temp_dir().join(format!("snapdir-b2-dest-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();
        b2.fetch_files(&read_back, &dest).expect("fetch_files");
        assert_eq!(std::fs::read(dest.join("foo")).unwrap(), b"foo\n");

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dest);
    }
}
