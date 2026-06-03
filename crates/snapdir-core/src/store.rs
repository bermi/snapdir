//! Storage backend abstraction and the content-addressable path layout.
//!
//! A snapdir *store* is any backing location that holds two kinds of
//! content-addressable blobs:
//!
//! - **objects** — the raw bytes of each file, addressed by their content
//!   checksum, under `.objects/`.
//! - **manifests** — the snapshot manifest text, addressed by its snapshot id
//!   (the BLAKE3 of the comment-stripped manifest), under `.manifests/`.
//!
//! Both use the same three-level sharded layout, slicing the hex address into
//! `3 / 3 / 3 / rest` segments to keep any single directory small. This layout
//! is a **frozen interop contract**: it must match the Bash oracle
//! (`snapdir`'s `_snapdir_get_object_rel_path` /
//! `_snapdir_get_manifest_rel_path`) byte-for-byte so that a store written by
//! either implementation is readable by the other.
//!
//! ```text
//! .objects/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>
//! .manifests/<id[0..3]>/<id[3..6]>/<id[6..9]>/<id[9..]>
//! ```
//!
//! # Sync trait, async implementations
//!
//! [`Store`] is a **synchronous, object-safe** trait. The orchestrator's walk
//! and hash stages are synchronous, and the on-disk [`FileStore`] (a later
//! gate) is naturally synchronous, so a sync surface keeps the common path
//! allocation-light and dyn-dispatchable (`&dyn Store`).
//!
//! Network stores (S3, B2, GCS) use async native SDKs. They satisfy this sync
//! trait by owning a private `tokio` runtime and bridging each method with
//! `runtime.block_on(async { … })`. That bridge lives entirely inside the
//! concrete store crate; it never leaks `async`/`await` or a runtime
//! requirement into `snapdir-core` or the orchestrator. This is deliberate:
//! making the trait `async` would force a runtime onto the otherwise-sync
//! `FileStore` and the CLI, and would cost object-safety without `async_trait`.
//!
//! [`FileStore`]: https://docs.rs/snapdir-file-store

use std::path::Path;

use thiserror::Error;

use crate::manifest::Manifest;

/// Top-level directory under a store that holds content objects.
pub const OBJECTS_DIR: &str = ".objects";

/// Top-level directory under a store that holds snapshot manifests.
pub const MANIFESTS_DIR: &str = ".manifests";

/// Returns the relative, sharded path of a content object given its hex
/// checksum.
///
/// The layout is `.objects/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>`, matching
/// the oracle's `_snapdir_get_object_rel_path`. The returned path always uses
/// forward slashes (the on-disk separator the oracle emits); a store targeting
/// a native filesystem can feed it straight to [`Path`], and an object-store
/// backend uses it verbatim as a key.
///
/// The checksum is used as-is; callers are expected to pass a lowercase hex
/// digest as produced by the [`crate::merkle`] hashers. Inputs shorter than
/// nine characters degrade gracefully (the missing shard segments and/or the
/// trailing component are simply empty), but that is never a valid snapdir
/// checksum.
///
/// # Examples
///
/// ```
/// use snapdir_core::store::object_path;
///
/// let h = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
/// assert_eq!(
///     object_path(h),
///     ".objects/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92"
/// );
/// ```
#[must_use]
pub fn object_path(checksum: &str) -> String {
    sharded_path(OBJECTS_DIR, checksum)
}

/// Returns the relative, sharded path of a manifest given its snapshot id.
///
/// The layout is `.manifests/<id[0..3]>/<id[3..6]>/<id[6..9]>/<id[9..]>`,
/// matching the oracle's `_snapdir_get_manifest_rel_path`. See [`object_path`]
/// for separator and input conventions.
///
/// # Examples
///
/// ```
/// use snapdir_core::store::manifest_path;
///
/// let id = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
/// assert_eq!(
///     manifest_path(id),
///     ".manifests/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92"
/// );
/// ```
#[must_use]
pub fn manifest_path(snapshot_id: &str) -> String {
    sharded_path(MANIFESTS_DIR, snapshot_id)
}

/// Shared three-level sharding used by both objects and manifests.
///
/// Slices `hex` into `[0..3] / [3..6] / [6..9] / [9..]` and joins them under
/// `prefix` with `/`. Mirrors the oracle's `${id:0:3}` / `${id:3:3}` /
/// `${id:6:3}` / `${id:9}` expansion exactly, including its behavior on short
/// inputs (Bash substring expansion past the end yields an empty string rather
/// than panicking, which `char_slice` reproduces).
fn sharded_path(prefix: &str, hex: &str) -> String {
    let s0 = char_slice(hex, 0, 3);
    let s1 = char_slice(hex, 3, 6);
    let s2 = char_slice(hex, 6, 9);
    let rest = char_slice(hex, 9, hex.len());
    format!("{prefix}/{s0}/{s1}/{s2}/{rest}")
}

/// Byte-range slice that clamps to the string's length instead of panicking,
/// matching Bash `${var:start:len}` semantics for the ASCII-hex inputs snapdir
/// uses. (snapdir addresses are hex, so byte and char offsets coincide.)
fn char_slice(s: &str, start: usize, end: usize) -> &str {
    let len = s.len();
    let start = start.min(len);
    let end = end.min(len);
    &s[start..end]
}

/// Errors a [`Store`] backend can surface.
///
/// Backends wrap their own failure types (filesystem I/O, HTTP/SDK errors,
/// integrity mismatches) into these variants. The orchestrator matches on the
/// variant, not the wrapped cause, so behavior stays backend-agnostic.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The requested manifest (by snapshot id) was not present in the store.
    #[error("manifest not found: {id}")]
    ManifestNotFound {
        /// The snapshot id that was looked up.
        id: String,
    },

    /// A content object referenced by a manifest was not present in the store.
    #[error("object not found: {checksum}")]
    ObjectNotFound {
        /// The object checksum that was looked up.
        checksum: String,
    },

    /// Stored bytes did not hash to the address they were filed under (object
    /// checksum or manifest snapshot id mismatch) — the blob is corrupt or
    /// tampered.
    #[error("integrity check failed for {address}: expected {expected}, got {actual}")]
    Integrity {
        /// The address (object path or manifest id) being verified.
        address: String,
        /// The checksum/id the address claims.
        expected: String,
        /// The checksum/id actually computed over the bytes.
        actual: String,
    },

    /// A manifest's text could not be parsed into a [`Manifest`].
    #[error("failed to parse manifest: {0}")]
    Parse(#[from] crate::manifest::ParseError),

    /// An underlying I/O failure (filesystem, network, SDK).
    #[error("store I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A backend-specific failure that does not fit the typed variants above
    /// (e.g. an SDK error from a network store). Carries a human-readable
    /// message and an optional source.
    #[error("store backend error: {message}")]
    Backend {
        /// Human-readable description of the failure.
        message: String,
        /// The wrapped backend error, if any.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },
}

/// A content-addressable storage backend for snapdir snapshots.
///
/// Implementors hold objects under [`object_path`] and manifests under
/// [`manifest_path`] within some root (a local directory, an S3/GCS/B2 bucket
/// prefix, …). The trait is the minimal surface the orchestrator needs to read
/// a snapshot back ([`get_manifest`](Store::get_manifest) +
/// [`fetch_files`](Store::fetch_files)) and to write one
/// ([`push`](Store::push)).
///
/// It is object-safe: callers can hold `&dyn Store` and pick the concrete
/// backend at runtime from a `store://` URL.
///
/// See the [module docs](crate::store) for why this is synchronous even though
/// network backends are async internally.
pub trait Store {
    /// Reads and parses the manifest stored under `id`'s sharded path,
    /// verifying that its bytes hash back to `id` before returning it.
    ///
    /// # Errors
    ///
    /// - [`StoreError::ManifestNotFound`] if no manifest is stored at `id`.
    /// - [`StoreError::Integrity`] if the stored bytes do not hash to `id`.
    /// - [`StoreError::Parse`] if the bytes are not a valid manifest.
    /// - [`StoreError::Io`] / [`StoreError::Backend`] on transport failure.
    fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError>;

    /// Materializes every entry of `manifest` under `dest`, pulling each
    /// referenced object from the store and reconstructing the directory tree
    /// (files, directories, permissions) rooted at `dest`.
    ///
    /// Implementations verify each fetched object against its manifest
    /// checksum.
    ///
    /// # Errors
    ///
    /// - [`StoreError::ObjectNotFound`] if a referenced object is missing.
    /// - [`StoreError::Integrity`] if a fetched object is corrupt.
    /// - [`StoreError::Io`] / [`StoreError::Backend`] on transport failure.
    fn fetch_files(&self, manifest: &Manifest, dest: &Path) -> Result<(), StoreError>;

    /// Uploads the objects referenced by `manifest` (reading their bytes from
    /// the tree rooted at `source`) and then the manifest itself, filing each
    /// under its sharded address.
    ///
    /// Implementations are expected to skip blobs already present and to write
    /// the manifest only after all of its objects have landed, so a manifest is
    /// never observable before the content it references (mirroring the
    /// oracle's commit ordering).
    ///
    /// # Errors
    ///
    /// - [`StoreError::Io`] / [`StoreError::Backend`] on transport failure.
    /// - [`StoreError::Integrity`] if a source file no longer matches its
    ///   manifest checksum at upload time.
    fn push(&self, manifest: &Manifest, source: &Path) -> Result<(), StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // The canonical cross-check: this exact hash → exact sharded path,
    // matching the original `_snapdir_get_object_rel_path` in the `snapdir`
    // script:
    //   .objects/${c:0:3}/${c:3:3}/${c:6:3}/${c:9}
    const SAMPLE: &str = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";

    #[test]
    fn store_object_path_matches_oracle_sharding() {
        assert_eq!(
            object_path(SAMPLE),
            ".objects/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92"
        );
    }

    #[test]
    fn store_manifest_path_matches_oracle_sharding() {
        assert_eq!(
            manifest_path(SAMPLE),
            ".manifests/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92"
        );
    }

    #[test]
    fn store_sharding_slices_three_three_three_rest() {
        // Independently reconstruct the oracle slicing to guard the boundaries.
        let h = SAMPLE;
        let expected = format!(
            ".objects/{}/{}/{}/{}",
            &h[0..3],
            &h[3..6],
            &h[6..9],
            &h[9..]
        );
        assert_eq!(object_path(h), expected);
        assert_eq!(&h[0..3], "49d");
        assert_eq!(&h[3..6], "c87");
        assert_eq!(&h[6..9], "0df");
    }

    #[test]
    fn store_path_prefixes_are_dot_objects_and_dot_manifests() {
        assert!(object_path(SAMPLE).starts_with(".objects/"));
        assert!(manifest_path(SAMPLE).starts_with(".manifests/"));
    }

    #[test]
    fn store_sharding_uses_forward_slashes_with_four_components_after_prefix() {
        let p = object_path(SAMPLE);
        let parts: Vec<&str> = p.split('/').collect();
        // [".objects", s0, s1, s2, rest]
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], ".objects");
        assert_eq!(parts[1].len(), 3);
        assert_eq!(parts[2].len(), 3);
        assert_eq!(parts[3].len(), 3);
        assert_eq!(parts[4].len(), SAMPLE.len() - 9);
    }

    #[test]
    fn store_sharding_clamps_short_inputs_like_bash() {
        // Bash `${var:0:3}` past the end yields empty rather than erroring.
        // Four `/` separators between the five (possibly empty) components.
        assert_eq!(object_path(""), ".objects////");
        assert_eq!(object_path("ab"), ".objects/ab///");
        assert_eq!(object_path("abcd"), ".objects/abc/d//");
        assert_eq!(object_path("abcdefghij"), ".objects/abc/def/ghi/j");
    }

    // Trait-shape / object-safety compile checks.

    /// A trivial in-memory implementor proving the trait is implementable and
    /// object-safe; exercised via `&dyn Store` below.
    struct NoopStore;

    impl Store for NoopStore {
        fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError> {
            Err(StoreError::ManifestNotFound { id: id.to_owned() })
        }

        fn fetch_files(&self, _manifest: &Manifest, _dest: &Path) -> Result<(), StoreError> {
            Ok(())
        }

        fn push(&self, _manifest: &Manifest, _source: &Path) -> Result<(), StoreError> {
            Ok(())
        }
    }

    #[test]
    fn store_trait_is_object_safe_and_implementable() {
        let store: Box<dyn Store> = Box::new(NoopStore);
        let dyn_ref: &dyn Store = store.as_ref();

        let manifest = Manifest::new();
        assert!(dyn_ref
            .fetch_files(&manifest, Path::new("/tmp/snapdir-dest"))
            .is_ok());
        assert!(dyn_ref
            .push(&manifest, Path::new("/tmp/snapdir-src"))
            .is_ok());

        match dyn_ref.get_manifest("deadbeef") {
            Err(StoreError::ManifestNotFound { id }) => assert_eq!(id, "deadbeef"),
            other => panic!("expected ManifestNotFound, got {other:?}"),
        }
    }

    #[test]
    fn store_error_parse_is_from_manifest_parse_error() {
        // A malformed manifest line surfaces as StoreError::Parse via #[from].
        let parse_err = Manifest::parse("F 700").unwrap_err();
        let store_err: StoreError = parse_err.into();
        assert!(matches!(store_err, StoreError::Parse(_)));
    }
}
