//! Memory-friendly, file-path-driven content hashing for the directory walk.
//!
//! The [`merkle::Hasher`](crate::merkle::Hasher) trait hashes an in-memory
//! `&[u8]`; that is the right shape for the directory-merkle rule (which hashes
//! short concatenations of child checksums) but a poor fit for hashing *file
//! contents*, where reading every file fully into a `Vec<u8>` before hashing is
//! both an allocation per file and a peak-memory hazard on large trees.
//!
//! This module adds a complementary [`HashFile`] trait whose
//! [`hash_file_hex`](HashFile::hash_file_hex) takes a [`Path`] and returns the
//! lowercase-hex checksum *and* the byte length, letting each hasher pick the
//! most memory-friendly engine:
//!
//! - **Unkeyed BLAKE3** ([`Blake3Hasher`](crate::Blake3Hasher)): for files at or
//!   above [`MMAP_THRESHOLD`] it memory-maps the file and hashes it with
//!   `update_mmap_rayon`, which both avoids a large heap copy and uses the
//!   `rayon` thread-pool to hash a single large file in parallel. Files below
//!   the threshold (and **all empty / 0-byte files**) take the plain
//!   [`std::fs::read`] branch — see the SIGBUS caveat below for why an empty
//!   file is never mmapped. The streaming `Hasher::new()+update+finalize`
//!   shape is byte-identical to the one-shot [`blake3::hash`], so the per-file
//!   checksums — and therefore the snapshot ids — are unchanged from the
//!   read-then-`hash_hex` path.
//! - **Keyed BLAKE3** ([`Blake3KeyedHasher`](crate::Blake3KeyedHasher)): the
//!   derive-key context lives in a private field of the frozen
//!   [`merkle`](crate::merkle) module, so this module cannot re-seed a raw
//!   [`blake3::Hasher`] with it. It therefore reads the whole file and defers
//!   to [`Hasher::hash_hex`](crate::merkle::Hasher::hash_hex), which is
//!   byte-identical to the previous `fs::read` + `hash_hex` pair (keyed mode is
//!   only used by the interop matrix, not the default snapshot path, so it does
//!   not gate the large-tree memory win).
//! - **MD5 / SHA-256** ([`Md5Hasher`](crate::Md5Hasher) /
//!   [`Sha256Hasher`](crate::Sha256Hasher)): these read the whole file and
//!   defer to [`Hasher::hash_hex`](crate::merkle::Hasher::hash_hex), staying
//!   byte-identical to the previous `fs::read` + `hash_hex` pair.
//!
//! ## SIGBUS on concurrent truncation (mmap caveat)
//!
//! `update_mmap_rayon` memory-maps the file and reads it through the mapping.
//! A snapshot assumes a **static tree**: if another process **truncates or
//! shrinks** a file *while it is being hashed*, accessing the now-invalid pages
//! raises `SIGBUS` and aborts the process. This is the **same exposure as
//! `b3sum`** (which mmaps by default) and is intrinsic to mmap-based hashing;
//! we deliberately do **not** install a `SIGBUS` handler. Callers that cannot
//! guarantee a quiescent tree should hash a copied/quiesced snapshot. Empty
//! and sub-threshold files take the plain-read branch and are not exposed to
//! this hazard.

use std::fs;
use std::io;
use std::path::Path;

use crate::merkle::{Blake3Hasher, Blake3KeyedHasher, Hasher, Md5Hasher, Sha256Hasher};

/// Files at or above this size (256 KiB) take the BLAKE3 mmap+rayon path; below
/// it (and any empty file) take the plain [`std::fs::read`] path.
///
/// 256 KiB matches `b3sum`'s own heuristic for when memory-mapping starts to
/// pay off; below it the syscall/setup overhead of mmap outweighs the copy a
/// plain read makes.
pub const MMAP_THRESHOLD: u64 = 256 * 1024;

/// Hashes a file's contents *by path*, returning `(lowercase_hex, byte_len)`.
///
/// This is the memory-friendly companion to
/// [`Hasher::hash_hex`](crate::merkle::Hasher::hash_hex): instead of taking an
/// already-materialized `&[u8]`, an implementation may stream or memory-map the
/// file so large files are hashed without a full heap copy. The returned hex is
/// **byte-identical** to `self.hash_hex(&fs::read(path)?)`, and the returned
/// length is the number of content bytes hashed (the file size).
pub trait HashFile {
    /// Hashes the file at `path`, returning its lowercase-hex checksum and its
    /// byte length.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] if the file's metadata cannot be
    /// read or its contents cannot be read / mapped.
    fn hash_file_hex(&self, path: &Path) -> io::Result<(String, u64)>;

    /// Like [`hash_file_hex`](HashFile::hash_file_hex), but guaranteed not to
    /// spawn its own nested `rayon` tasks for a single file.
    ///
    /// The byte-identical result is the same as [`hash_file_hex`]; only the
    /// *engine* differs. The cross-file parallel walk uses this variant when it
    /// already has at least as many pending files as worker threads, so each
    /// worker hashes one file single-threaded and the bounded walk pool is not
    /// oversubscribed by intra-file BLAKE3 `rayon` tasks. The default
    /// implementation forwards to [`hash_file_hex`]; only the unkeyed BLAKE3
    /// hasher (whose [`hash_file_hex`] uses `update_mmap_rayon`) overrides it to
    /// drop down to the single-threaded `update_mmap` engine.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] under the same conditions as
    /// [`hash_file_hex`].
    fn hash_file_hex_seq(&self, path: &Path) -> io::Result<(String, u64)> {
        self.hash_file_hex(path)
    }
}

/// Hashes a BLAKE3 `hasher` over the file at `path`, choosing the mmap+rayon
/// engine for files `>= MMAP_THRESHOLD` and a plain read otherwise.
///
/// `hasher` arrives pre-seeded (plain `new()` for unkeyed, `new_derive_key` for
/// keyed); this only selects the read strategy. Returns `(hex, byte_len)`.
fn blake3_hash_file(mut hasher: blake3::Hasher, path: &Path) -> io::Result<(String, u64)> {
    let len = fs::metadata(path)?.len();
    if len >= MMAP_THRESHOLD {
        // Large file: memory-map + hash in parallel, no large heap copy. Never
        // reached for an empty file (len 0 < MMAP_THRESHOLD), so mmap of a
        // zero-length file — which can SIGBUS — never happens.
        hasher.update_mmap_rayon(path)?;
    } else {
        // Small/empty file: a plain read is cheaper than the mmap setup, and
        // the streaming update is byte-identical to the one-shot hash.
        let bytes = fs::read(path)?;
        hasher.update(&bytes);
    }
    Ok((hasher.finalize().to_hex().to_string(), len))
}

/// Single-threaded BLAKE3 file hash: same mmap/plain-read selection as
/// [`blake3_hash_file`] but using `update_mmap` (no nested `rayon`) for large
/// files. Byte-identical output; only the engine differs.
fn blake3_hash_file_seq(mut hasher: blake3::Hasher, path: &Path) -> io::Result<(String, u64)> {
    let len = fs::metadata(path)?.len();
    if len >= MMAP_THRESHOLD {
        // Large file: memory-map + hash single-threaded (no intra-file rayon
        // fan-out). Empty files never reach this branch (len 0 < threshold).
        hasher.update_mmap(path)?;
    } else {
        let bytes = fs::read(path)?;
        hasher.update(&bytes);
    }
    Ok((hasher.finalize().to_hex().to_string(), len))
}

impl HashFile for Blake3Hasher {
    fn hash_file_hex(&self, path: &Path) -> io::Result<(String, u64)> {
        blake3_hash_file(blake3::Hasher::new(), path)
    }

    fn hash_file_hex_seq(&self, path: &Path) -> io::Result<(String, u64)> {
        blake3_hash_file_seq(blake3::Hasher::new(), path)
    }
}

impl HashFile for Blake3KeyedHasher {
    fn hash_file_hex(&self, path: &Path) -> io::Result<(String, u64)> {
        // The derive-key context is a private field of the frozen `merkle`
        // module, so we cannot seed a raw blake3 hasher with it here. Read the
        // whole file and defer to `hash_hex` (the keyed `new_derive_key` +
        // update path), byte-identical to the previous fs::read + hash_hex pair.
        let bytes = fs::read(path)?;
        Ok((self.hash_hex(&bytes), bytes.len() as u64))
    }
}

impl HashFile for Md5Hasher {
    fn hash_file_hex(&self, path: &Path) -> io::Result<(String, u64)> {
        // MD5 has no mmap fast-path here: read the whole file and defer to
        // hash_hex, byte-identical to the previous fs::read + hash_hex pair.
        let bytes = fs::read(path)?;
        Ok((self.hash_hex(&bytes), bytes.len() as u64))
    }
}

impl HashFile for Sha256Hasher {
    fn hash_file_hex(&self, path: &Path) -> io::Result<(String, u64)> {
        // SHA-256: read the whole file and defer to hash_hex, byte-identical to
        // the previous fs::read + hash_hex pair.
        let bytes = fs::read(path)?;
        Ok((self.hash_hex(&bytes), bytes.len() as u64))
    }
}
