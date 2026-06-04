//! Small shared helpers used across the in-lane store backends.
//!
//! Kept crate-private so the three [`Store`](snapdir_core::store::Store)
//! implementations (`file://`, `s3://`, `gs://`) share one definition of the
//! fetch-side "already present and verified?" decision rather than each
//! reimplementing it.

use std::path::Path;

use snapdir_core::merkle::Hasher;
use snapdir_core::store::StoreError;

/// Hashes a file's full byte content with `hasher`, returning its hex digest.
///
/// Reused by every backend's `fetch_files` to recompute a destination file's
/// content address (BLAKE3) for the skip-if-present-and-verified check.
pub(crate) fn hash_file(path: &Path, hasher: &impl Hasher) -> Result<String, StoreError> {
    let bytes = std::fs::read(path)?;
    Ok(hasher.hash_hex(&bytes))
}

/// Returns `true` when `target` already exists as a regular file whose
/// locally-recomputed content hash equals `expected`.
///
/// This is the fetch-side skip gate: a present, checksum-matching destination
/// file never needs to be re-copied or re-downloaded (and a mismatching one is
/// left for the caller to repair by overwriting). The check is content-gated,
/// not mere existence — a corrupt local file returns `false` so it gets
/// re-fetched, and any non-file (directory, symlink target, …) also returns
/// `false`.
///
/// A read error while hashing an existing file is treated as "not a clean
/// match" (`false`) rather than propagated, so a transiently unreadable
/// destination falls through to a normal fetch instead of aborting the run.
pub(crate) fn file_present_and_verified(
    target: &Path,
    expected: &str,
    hasher: &impl Hasher,
) -> bool {
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.is_file() => {}
        _ => return false,
    }
    matches!(hash_file(target, hasher), Ok(actual) if actual == expected)
}
