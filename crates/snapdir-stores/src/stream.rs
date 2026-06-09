//! Object/manifest-level, content-addressed streaming over a [`Store`].
//!
//! [`StreamStore`] is the foundation for store-to-store sync: it exposes the
//! raw, content-addressed blob and manifest primitives an orchestrator needs to
//! copy a snapshot directly from one store to another — through memory, with no
//! local filesystem staging.
//!
//! Where [`Store`] works at the *snapshot* level (read a whole tree out with
//! [`get_manifest`](Store::get_manifest) + [`fetch_files`](Store::fetch_files),
//! write one in with [`push`](Store::push)), [`StreamStore`] works at the
//! *object* level: check whether a single content object is already present
//! ([`has_object`](StreamStore::has_object)), read one raw blob by its
//! content-address ([`get_object`](StreamStore::get_object)), write one raw blob
//! ([`put_object`](StreamStore::put_object)), and write the manifest object
//! itself ([`put_manifest`](StreamStore::put_manifest)). A later orchestrator
//! can then walk a source manifest, `get_object` each referenced blob from the
//! source, `put_object` it into the destination (skipping any the destination
//! already `has_object`), and finally `put_manifest` — never touching the local
//! disk.
//!
//! Every read and write is BLAKE3-verified against the address it is filed
//! under (the same integrity discipline as [`Store`]): a blob whose bytes do not
//! hash to its checksum is rejected with [`StoreError::Integrity`] rather than
//! returned or stored, so corruption can never silently propagate across a
//! store-to-store copy.
//!
//! The sharded object/manifest keys and the manifest byte-format are reused
//! verbatim from each backend's existing [`Store`] implementation, so a
//! `StreamStore` round-trip is byte-for-byte interchangeable with a `push` /
//! `fetch_files` round-trip (and with the Bash oracle's layout).
//!
//! Like [`Store`], the trait is **synchronous**: the network backends drive
//! their async SDK calls on an internal runtime via `block_on`, exactly as their
//! [`Store`] methods do. It is **not** implemented for the external-store shim
//! ([`ExternalStore`](crate::shim::ExternalStore)), which is shell- and
//! local-path-based and cannot stream raw object blobs.

use snapdir_core::manifest::Manifest;
use snapdir_core::store::{Store, StoreError};

/// Raw, content-addressed object/manifest streaming on top of a [`Store`].
///
/// See the [module docs](crate::stream) for the store-to-store sync motivation
/// and the verification invariants. The [`Store`] supertrait means every
/// implementor also offers [`get_manifest`](Store::get_manifest),
/// [`fetch_files`](Store::fetch_files), and [`push`](Store::push).
pub trait StreamStore: Store {
    /// Returns `true` if an object with this content-address already exists in
    /// the store.
    ///
    /// This is the existence check a store-to-store orchestrator uses to skip
    /// re-copying blobs the destination already holds. It does not read or
    /// verify the object body.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] / [`StoreError::Backend`] on transport failure.
    fn has_object(&self, checksum: &str) -> Result<bool, StoreError>;

    /// Reads the raw object blob filed under `checksum`, verifying its bytes
    /// hash (BLAKE3) back to `checksum` before returning them.
    ///
    /// # Errors
    ///
    /// - [`StoreError::ObjectNotFound`] if no object is stored at `checksum`.
    /// - [`StoreError::Integrity`] if the stored bytes do not hash to
    ///   `checksum` (the blob is corrupt or tampered).
    /// - [`StoreError::Io`] / [`StoreError::Backend`] on transport failure.
    fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError>;

    /// Writes a raw object blob at its content-address, verifying `bytes` hash
    /// (BLAKE3) to `checksum` *before* storing anything.
    ///
    /// A mismatch stores nothing and returns an error, so a corrupt blob can
    /// never land at a content-address it does not belong to.
    ///
    /// # Errors
    ///
    /// - [`StoreError::Integrity`] if `bytes` do not hash to `checksum`.
    /// - [`StoreError::Io`] / [`StoreError::Backend`] on transport failure.
    fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError>;

    /// Writes the manifest object for `id`, verifying the manifest's bytes hash
    /// back to `id` before storing it.
    ///
    /// This is the final step of a store-to-store copy: it is written only after
    /// every referenced object has landed, so a manifest is never observable
    /// before the content it references (mirroring [`push`](Store::push)).
    ///
    /// # Errors
    ///
    /// - [`StoreError::Integrity`] if the manifest does not hash to `id`.
    /// - [`StoreError::Io`] / [`StoreError::Backend`] on transport failure.
    fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError>;

    /// Returns the subset of `checksums` NOT present in the store, PRESERVING
    /// INPUT ORDER.
    ///
    /// This is the diff primitive behind the `snapdir objects-needed` wire
    /// plumbing (SNAPPACK acceleration, see [`crate::pack`]): a sender offers
    /// a snapshot's full object list and the receiver answers with exactly the
    /// objects it still needs, so only those ride the pack stream.
    ///
    /// Semantics:
    ///
    /// - **Fail closed:** every checksum is validated against
    ///   `^[0-9a-f]{64}$` ([`crate::pack::is_hex64`]) BEFORE the first
    ///   existence probe; any invalid entry is a hard error and nothing is
    ///   returned (a malformed request must never be partially answered).
    /// - **Order-preserving, no dedup:** the returned complement keeps the
    ///   input order, and deduplication is the CALLER's job — an absent
    ///   checksum supplied twice is reported twice.
    /// - The default implementation loops [`has_object`](Self::has_object)
    ///   (one existence probe per checksum). Follow-up (not this gate): the
    ///   S3/GCS backends should override this with their batched listing APIs
    ///   to cut round trips — any override must preserve the exact contract
    ///   above.
    ///
    /// # Errors
    ///
    /// - [`StoreError::Backend`] if any checksum is not 64 lowercase hex
    ///   characters (fail closed, before any probe).
    /// - Whatever [`has_object`](Self::has_object) surfaces on transport
    ///   failure.
    fn objects_needed(&self, checksums: &[String]) -> Result<Vec<String>, StoreError> {
        // Validate EVERYTHING up front so an invalid entry can never be
        // answered partially (fail closed).
        for checksum in checksums {
            if !crate::pack::is_hex64(checksum) {
                return Err(StoreError::Backend {
                    message: format!(
                        "invalid object checksum {checksum:?}: expected 64 lowercase hex characters"
                    ),
                    source: None,
                });
            }
        }
        let mut needed = Vec::new();
        for checksum in checksums {
            if !self.has_object(checksum)? {
                needed.push(checksum.clone());
            }
        }
        Ok(needed)
    }
}
