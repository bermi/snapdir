//! snapdir core library.
//!
//! Manifest format, BLAKE3 merkle hashing, store trait, directory walk, and
//! cache live here. Per the library-purity principle, this crate performs no
//! terminal I/O and reads no `$HOME`/config/environment for behavior: inputs
//! arrive as parameters and errors surface as typed [`thiserror`] enums.
//!
//! The [`manifest`] module owns the frozen manifest line format
//! (`PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH`) and its (de)serialization.

pub mod manifest;

pub use manifest::{Manifest, ManifestEntry, ParseError, PathType};
