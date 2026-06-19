//! Pure exact-mirror prune-set computation (Phase 32: `--delete`).
//!
//! Given a target [`Manifest`] and a listing of the destination tree's
//! paths + types ([`DestEntry`]), [`prune_set`] returns the *extraneous* dest
//! paths — those present in the dest but ABSENT from the manifest — in
//! deepest-first deletion order. This is exactly the set `--delete` must remove
//! to make the destination an exact mirror of the manifest.
//!
//! This module is **pure** and content-free, by design:
//!
//! - It NEVER reads or hashes object content; the manifest's `checksum`/`size`
//!   fields are irrelevant to the keep/prune decision. The keep/prune key is a
//!   path's `(path, path_type)` only, encoded by the trailing-slash convention
//!   shared with [`ManifestEntry`](crate::ManifestEntry): a file `./p` and a
//!   directory `./p/` are DISTINCT keys, so a file↔directory type change at a
//!   path surfaces the dest form as extraneous (to be replaced).
//! - It performs NO I/O and takes no object-store handle; the destination
//!   walk that produces `dest_entries` is a later (cli) gate's job.
//!
//! The set-difference approach mirrors the dest-side `Added` classification of
//! [`crate diff`](../../snapdir_cli/diff/index.html) (a dest path not keyed by
//! the manifest == extraneous), re-implemented standalone here so `snapdir-core`
//! carries no dependency on `snapdir-cli`.

use std::collections::BTreeSet;

use crate::excludes::ExcludeMatcher;
use crate::{Manifest, PathType};

/// A single destination-tree entry: its path (verbatim, `./`-prefixed like a
/// manifest path; directories end with `/`) and its on-disk type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestEntry {
    /// The dest path, verbatim. Directories end with `/`, files do not.
    pub path: String,
    /// The on-disk type of the entry.
    pub path_type: PathType,
}

impl DestEntry {
    /// Builds a [`DestEntry`] from a path and its type.
    #[must_use]
    pub fn new(path: impl Into<String>, path_type: PathType) -> Self {
        Self {
            path: path.into(),
            path_type,
        }
    }
}

/// Computes the extraneous dest paths in DEEPEST-FIRST deletion order.
///
/// The prune-set is every [`DestEntry`] whose path key is NOT present in the
/// manifest, minus the destination root `./` (which mirrors the manifest target
/// and is never pruned), minus any path PROTECTED by an exclude.
///
/// Keep/prune keys on `(path, path_type)` only — the trailing-slash convention
/// distinguishes a file `./p` from a directory `./p/`, so a file↔directory type
/// change yields the dest form as extraneous. Content (`checksum`/`size`) is
/// never consulted, and no object pool is required.
///
/// `excludes` are extended-regex patterns (grep `-E` semantics, the same
/// primitive as [`ExcludeMatcher`]): an extraneous path matching ANY exclude is
/// PROTECTED — removed from the prune-set. Excludes only ever REMOVE entries;
/// they never resurrect a kept (in-manifest) path. An invalid regex pattern
/// simply protects nothing (it cannot match), keeping the function total.
///
/// The returned order is deepest-first: across the whole set, every descendant
/// precedes its ancestors, so a caller can unlink children before `rmdir`-ing
/// their parent directories. The order is deterministic and idempotent.
#[must_use]
pub fn prune_set(
    manifest: &Manifest,
    dest_entries: &[DestEntry],
    excludes: &[&str],
) -> Vec<String> {
    // The set of manifest path keys (verbatim, trailing-`/` for dirs). A dest
    // path is KEPT iff its exact key (including the type-distinguishing trailing
    // slash) is in this set.
    let kept: BTreeSet<&str> = manifest.entries().iter().map(|e| e.path.as_str()).collect();

    // Compile the exclude patterns once. An invalid pattern is skipped (it
    // protects nothing), keeping `prune_set` total and panic-free.
    let matchers: Vec<ExcludeMatcher> = excludes
        .iter()
        .filter_map(|p| ExcludeMatcher::new(p).ok())
        .collect();
    let is_protected = |path: &str| matchers.iter().any(|m| m.is_excluded(path));

    let mut extraneous: Vec<&str> = dest_entries
        .iter()
        .map(|d| d.path.as_str())
        // The mirror root is never pruned/emitted.
        .filter(|p| *p != "./")
        // Extraneous = dest path key absent from the manifest.
        .filter(|p| !kept.contains(p))
        // `--exclude` is a one-way PROTECT: drop matching paths from the set.
        .filter(|p| !is_protected(p))
        .collect();

    // Deepest-first deletion order: sort by descending path-segment depth so a
    // descendant always precedes its ancestors; break ties by path (byte order)
    // for a deterministic, idempotent result.
    extraneous.sort_by(|a, b| depth(b).cmp(&depth(a)).then_with(|| a.cmp(b)));

    extraneous.into_iter().map(str::to_owned).collect()
}

/// The directory depth of a path: the number of path separators, ignoring a
/// single trailing `/` (which marks a directory rather than adding depth).
///
/// `./` is depth 1, `./a` and `./a/` are depth 1 (a child of root rendered at
/// the top level), `./a/b` and `./a/b/` are depth 2, etc. Sorting by descending
/// depth places any descendant strictly after a shorter ancestor prefix.
fn depth(path: &str) -> usize {
    path.trim_end_matches('/').matches('/').count()
}
