//! snapdir core library.
//!
//! Manifest format, BLAKE3 merkle hashing, store trait, directory walk, and
//! cache live here. Per the library-purity principle, this crate performs no
//! terminal I/O and reads no `$HOME`/config/environment for behavior: inputs
//! arrive as parameters and errors surface as typed [`thiserror`] enums.
//!
//! The [`manifest`] module owns the frozen manifest line format
//! (`PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH`) and its (de)serialization. The
//! [`merkle`] module owns the directory checksum rule (sort + dedup + concat +
//! re-hash of the direct children's checksums) and the snapshot id
//! ([`snapshot_id`] — BLAKE3 of the comment-stripped manifest text, distinct
//! from the root directory checksum).

pub mod manifest;
pub mod merkle;

pub use manifest::{Manifest, ManifestEntry, ParseError, PathType};
pub use merkle::{directory_checksum, snapshot_id, Blake3Hasher, Hasher};
