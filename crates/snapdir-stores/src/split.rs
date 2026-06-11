//! `SplitStore`: a composite store that serves objects from one pool and
//! manifests from a separate location.
//!
//! A [`SplitStore`] wraps TWO underlying [`StreamStore`]s:
//!
//! - an **objects** pool — only its `.objects/` content-addressed blobs are
//!   used;
//! - a **manifests** location — only its `.manifests/<id>` slots are used.
//!
//! Object-level operations
//! ([`has_object`](StreamStore::has_object) / [`get_object`](StreamStore::get_object)
//! / [`put_object`](StreamStore::put_object) / [`objects_needed`](StreamStore::objects_needed))
//! route to the objects pool; manifest operations
//! ([`get_manifest`](Store::get_manifest) / [`put_manifest`](StreamStore::put_manifest))
//! route to the manifests location. It implements both [`Store`] and
//! [`StreamStore`], so it is a drop-in for `push`/`fetch`/`pull`/`sync`.
//!
//! # Why split?
//!
//! One content-addressed objects pool can back many independent manifest
//! prefixes: pushing the same tree under two different manifest locations
//! re-uploads ZERO objects the second time, because the per-object skip probe is
//! the SHARED pool's [`has_object`](StreamStore::has_object). The objects bytes a
//! split push writes are byte-for-byte identical to a colocated [`FileStore`]
//! push of the same tree (frozen sharded-layout interop), so a split pool and a
//! colocated store are interchangeable.
//!
//! # Reuse, not reinvention
//!
//! [`SplitStore`] does NOT duplicate the materialization or upload machinery the
//! backends already ship:
//!
//! - [`fetch_files`](Store::fetch_files) delegates straight to the objects
//!   side's [`Store::fetch_files`], so per-entry directory/perms/skip-present
//!   materialization and the `ObjectNotFound` / `Integrity` discipline come from
//!   the backend verbatim.
//! - [`push`](Store::push) reuses the [`StreamStore`] object primitives
//!   ([`has_object`](StreamStore::has_object) +
//!   [`put_object`](StreamStore::put_object), each BLAKE3-verified before it
//!   writes) and the manifests side's [`put_manifest`](StreamStore::put_manifest),
//!   preserving the objects-before-manifest + manifest-last + all-or-nothing
//!   invariants. The only new code is the small "read+verify local file objects
//!   into the pool, then write the manifest last" glue — the same shape
//!   `file_store.rs` / `push.rs` already use, but with the manifest landing in a
//!   DIFFERENT store.

use std::path::Path;

use snapdir_core::manifest::{Manifest, PathType};
use snapdir_core::merkle::{snapshot_id, Blake3Hasher};
use snapdir_core::store::{Store, StoreError};

use crate::stream::StreamStore;

/// Strips a leading `./` (relative-mode manifest paths) and a trailing `/`
/// (directory entries) so the remainder can be joined onto a source root.
///
/// Mirrors the identical helper in `file_store.rs` / `push.rs`; kept local so
/// the split push glue never reaches into a sibling module's privates.
fn strip_leading_dot_slash(path: &str) -> &str {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    trimmed.strip_suffix('/').unwrap_or(trimmed)
}

/// A composite [`Store`]/[`StreamStore`] that serves content objects from one
/// pool and manifests from a separate location.
///
/// See the [module docs](crate::split) for the routing and reuse model.
/// Construct one with [`SplitStore::new`].
pub struct SplitStore {
    /// The objects pool: only its `.objects/` content-addressed blobs are used.
    objects: Box<dyn StreamStore + Sync>,
    /// The manifests location: only its `.manifests/<id>` slots are used.
    manifests: Box<dyn StreamStore + Sync>,
}

impl SplitStore {
    /// Builds a [`SplitStore`] over an `objects` pool and a `manifests`
    /// location.
    ///
    /// Object ops route to `objects`; manifest ops route to `manifests`. Either
    /// side may be any [`StreamStore`] (a [`FileStore`](crate::FileStore), an
    /// `S3Store`, etc.).
    pub fn new(
        objects: impl StreamStore + Sync + 'static,
        manifests: impl StreamStore + Sync + 'static,
    ) -> Self {
        Self {
            objects: Box::new(objects),
            manifests: Box::new(manifests),
        }
    }

    /// Builds a [`SplitStore`] from two already-boxed stores (e.g. when the two
    /// sides are different concrete types resolved at the CLI seam).
    #[must_use]
    pub fn from_boxed(
        objects: Box<dyn StreamStore + Sync>,
        manifests: Box<dyn StreamStore + Sync>,
    ) -> Self {
        Self { objects, manifests }
    }
}

impl Store for SplitStore {
    fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError> {
        // Manifest ops route to the manifests location.
        self.manifests.get_manifest(id)
    }

    fn fetch_files(&self, manifest: &Manifest, dest: &Path) -> Result<(), StoreError> {
        // The objects side already materializes file objects from its own
        // `.objects/` pool into `dest`, reusing the backend's per-entry
        // directory/perms/symlink + skip-present + `ObjectNotFound`/`Integrity`
        // discipline. No duplication here.
        self.objects.fetch_files(manifest, dest)
    }

    fn push(&self, manifest: &Manifest, source: &Path) -> Result<(), StoreError> {
        let hasher = Blake3Hasher::new();
        let id = snapshot_id(manifest, &hasher);

        // Skip-if-present probed on the MANIFESTS side: a present manifest there
        // implies all of its objects already landed in the pool (we maintain
        // that by writing the manifest LAST). The fast path must not touch the
        // pool, so we never probe `objects` here.
        if self.manifests.get_manifest(&id).is_ok() {
            return Ok(());
        }

        // Objects-before-manifest: read+verify every absent local file object
        // and upload it into the objects pool. Per-object skip is probed on the
        // pool's `has_object` (content-addressed: a present object is already
        // the right bytes). Any failure returns early WITHOUT writing the
        // manifest (all-or-nothing).
        for entry in manifest.entries() {
            if entry.path_type != PathType::File {
                continue;
            }
            if self.objects.has_object(&entry.checksum)? {
                continue;
            }
            let rel = strip_leading_dot_slash(&entry.path);
            let object_source = source.join(rel);
            // A missing/unreadable source aborts the push (interrupted push ->
            // no manifest). `put_object` re-verifies the bytes against the
            // content-address before storing, so a corrupt source can never
            // land at the wrong key.
            let bytes = std::fs::read(&object_source)?;
            self.objects.put_object(&entry.checksum, bytes)?;
        }

        // Manifest-last: only after every object landed in the pool do we write
        // the manifest to the manifests location.
        self.manifests.put_manifest(&id, manifest)
    }
}

impl StreamStore for SplitStore {
    fn has_object(&self, checksum: &str) -> Result<bool, StoreError> {
        self.objects.has_object(checksum)
    }

    fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
        self.objects.get_object(checksum)
    }

    fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        self.objects.put_object(checksum, bytes)
    }

    fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        // Manifest ops route to the manifests location; the objects pool is
        // never written.
        self.manifests.put_manifest(id, manifest)
    }

    fn objects_needed(&self, checksums: &[String]) -> Result<Vec<String>, StoreError> {
        // Delegate so the pool's own (possibly batched) override is used, and
        // the fail-closed checksum validation is the pool's.
        self.objects.objects_needed(checksums)
    }

    fn list_manifest_ids(&self) -> Result<Vec<String>, StoreError> {
        // `list_manifest_ids` is a MANIFEST op: it enumerates the `.manifests/`
        // ids of the manifests location, NEVER the shared objects pool. Two
        // split stores sharing one pool therefore each list only their own
        // manifests (shared-pool isolation).
        self.manifests.list_manifest_ids()
    }
}
