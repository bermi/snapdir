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
//! re-hash of the direct children's checksums), the snapshot id
//! ([`snapshot_id`] — BLAKE3 of the comment-stripped manifest text, distinct
//! from the root directory checksum), and the [`Hasher`] abstraction with its
//! in-process [`Blake3Hasher`], keyed [`Blake3KeyedHasher`]
//! (`SNAPDIR_MANIFEST_CONTEXT`), [`Md5Hasher`] and [`Sha256Hasher`]
//! (`--checksum-bin`) implementations. The [`excludes`] module owns the
//! `%system%`/`%common%` expansion, the `grep -E -v` matcher, and the
//! follow/no-follow option semantics.

pub mod cache;
pub mod excludes;
pub mod manifest;
pub mod merkle;
pub mod progress;
pub mod resources;
pub mod store;
pub mod walk;

pub use cache::{
    check_manifest_integrity, check_snapshot_integrity, flush_cache, load_cached_manifest,
    verify_cache, CacheError, CacheReport,
};
pub use excludes::{
    expand_excludes, ExcludeError, ExcludeMatcher, ExpandedExclude, FollowMode,
    COMMON_EXCLUDE_DIRS, SYSTEM_EXCLUDE_DIRS,
};
pub use manifest::{Manifest, ManifestEntry, ParseError, PathType};
pub use merkle::{
    directory_checksum, snapshot_id, Blake3Hasher, Blake3KeyedHasher, Hasher, Md5Hasher,
    Sha256Hasher,
};
pub use progress::{Meter, MeterSnapshot, Phase};
pub use resources::{resident_set_bytes, total_ram_bytes, CpuSampler};
pub use store::{manifest_path, object_path, Store, StoreError, MANIFESTS_DIR, OBJECTS_DIR};
pub use walk::{walk, walk_with_meter, PathMode, WalkError, WalkOptions};
