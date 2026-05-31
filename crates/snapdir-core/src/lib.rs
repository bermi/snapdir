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
//! re-hash of the direct children's checksums); the root directory checksum is
//! the snapshot id.

pub mod manifest;
pub mod merkle;

pub use manifest::{Manifest, ManifestEntry, ParseError, PathType};
pub use merkle::{directory_checksum, Blake3Hasher, Hasher};
