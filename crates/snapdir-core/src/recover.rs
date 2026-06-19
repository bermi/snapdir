//! Linked-mode checksum-reuse fast path: recover a file's content checksum from
//! the object-store path its symlink points at, *without reading the bytes*.
//!
//! In `--linked` mode a checkout's destination entries are symlinks into a local
//! content-addressed object store whose layout mechanically encodes the file's
//! BLAKE3 digest:
//!
//! ```text
//! <store-root>/.objects/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>
//! ```
//!
//! (the exact inverse of the frozen [`object_path`](crate::store::object_path) /
//! `sharded_path` split). A re-snapshot (`snapdir id` / `manifest`) of such a
//! tree can therefore RECOVER each file's checksum directly from the symlink
//! target's object path, never reading or hashing the object's content — the
//! whole point of the fast path.
//!
//! This module is **pure** (string/path parsing only — no I/O of its own) and
//! **dormant by default**: it is consulted only when the [`walk`](crate::walk)
//! caller supplies a non-empty object-store-roots hint
//! ([`WalkOptions::object_store_roots`](crate::walk::WalkOptions::object_store_roots))
//! AND the active hasher reports it recovers object keys
//! ([`HashFile::recovers_object_keys`](crate::hash_file::HashFile::recovers_object_keys),
//! true only for plain, non-keyed BLAKE3 — the store's addressing algorithm).
//! With no hint every existing call site behaves byte-identically.
//!
//! ## Trust boundary (no trust, no panic)
//!
//! Recovery is attempted only for a target that **lexically resolves under a
//! hinted root** as a well-formed `.objects/3/3/3/rest` object path whose four
//! shard segments concatenate to a valid 64-hex key. A target that escaped the
//! store, has the wrong shard shape, or is not 64-hex yields [`None`] (the
//! caller falls back to a normal followed-symlink content hash). Whether the
//! object actually EXISTS (dangling vs present) is decided by the caller via a
//! cheap `exists()` check, so a dangling object becomes a typed
//! [`WalkError`](crate::walk::WalkError) rather than a silent drop or a panic.

use std::path::{Component, Path, PathBuf};

use crate::store::OBJECTS_DIR;

/// Returns `true` iff `s` is exactly 64 lowercase hex characters — the shape of
/// a snapdir BLAKE3 object key / snapshot id. (Mirrors the `is_hex64` predicate
/// used by the store layer; kept local so `snapdir-core` has no dependency on
/// the stores crate.)
#[must_use]
fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Lexically normalizes `path` by folding `.` and `..` components, WITHOUT
/// touching the filesystem.
///
/// Used to canonicalize a symlink target enough to test it against the hinted
/// store roots even when the target object is MISSING (a `fs::canonicalize`
/// would fail on a dangling target — we must still recognize that the dangling
/// target *was* an object path so the caller can raise a typed error rather than
/// silently dropping it). For symlink targets snapdir's `--linked` checkout
/// emits — absolute paths into a store's `.objects/` — no symlink components
/// remain to be followed, so the lexical fold matches the real canonical path.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop a trailing normal component; keep `..` only when there is
                // nothing to pop (a path that climbs above its anchor).
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolves a symlink target (as read by `read_link`, possibly relative to the
/// link's parent directory) to an absolute, lexically-normalized path.
fn resolve_target(link_parent: &Path, target: &Path) -> PathBuf {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_parent.join(target)
    };
    lexically_normalize(&joined)
}

/// Recovers the 64-hex object key encoded by a symlink target's object path, if
/// the target is a well-formed object under one of the hinted store `roots`.
///
/// `link_parent` is the directory containing the symlink (used to resolve a
/// relative `target`). `target` is the raw symlink target (`fs::read_link`).
/// `roots` are the local store roots the caller hinted (each a `<store-root>`
/// directory that holds a `.objects/` pool); an empty slice always yields
/// [`None`] (the fast path is dormant).
///
/// Returns `Some(key)` only when the resolved target is exactly
/// `<root>/.objects/<3>/<3>/<3>/<rest>` for some hinted `root`, the three shard
/// segments are 3 chars each, and their concatenation with the trailing segment
/// is a valid 64-hex key. Otherwise (escaped the store, wrong shard shape, non-
/// hex, …) returns [`None`] and the caller hashes the followed content normally.
///
/// This performs **no filesystem I/O** and never panics: it is pure path
/// parsing. The caller decides present-vs-dangling separately.
#[must_use]
pub fn recover_object_key(link_parent: &Path, target: &Path, roots: &[PathBuf]) -> Option<String> {
    if roots.is_empty() {
        return None;
    }
    let resolved = resolve_target(link_parent, target);

    for root in roots {
        let root = lexically_normalize(root);
        let Ok(rel) = resolved.strip_prefix(&root) else {
            continue;
        };
        // The remainder under the root must be exactly the sharded object
        // layout: `.objects / s0 / s1 / s2 / rest` (five components). Anything
        // else (not under `.objects`, wrong depth, an extra nested level) is not
        // a clean object address and is rejected.
        let comps: Vec<&std::ffi::OsStr> = rel
            .components()
            .map(|c| match c {
                Component::Normal(s) => Some(s),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        if comps.len() != 5 {
            continue;
        }
        if comps[0] != OBJECTS_DIR {
            continue;
        }
        let (Some(s0), Some(s1), Some(s2), Some(rest)) = (
            comps[1].to_str(),
            comps[2].to_str(),
            comps[3].to_str(),
            comps[4].to_str(),
        ) else {
            continue;
        };
        // The frozen `sharded_path` split is `[0..3] / [3..6] / [6..9] / [9..]`,
        // so the three shard dirs are exactly 3 chars each.
        if s0.len() != 3 || s1.len() != 3 || s2.len() != 3 {
            continue;
        }
        let key = format!("{s0}{s1}{s2}{rest}");
        if is_hex64(&key) {
            return Some(key);
        }
    }
    None
}
