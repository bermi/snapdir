//! Pure `snapdir diff` logic: union manifests into per-side path maps and
//! classify each path as Added / Deleted / Modified / Unchanged.
//!
//! This module is intentionally I/O-free and store-free: it operates on
//! already-read [`Manifest`]s only. The CLI seam in [`crate::cli`] reads the
//! manifests (MANIFESTS ONLY — never an object store) and hands their entries
//! here. Keeping the comparison here makes `diff` a thin map-diff over manifest
//! entries, reusing [`ManifestEntry`] verbatim as the comparison key.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use snapdir_core::{Manifest, ManifestEntry};

/// The per-path change classification, mirroring git's porcelain letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// In TO but not FROM.
    Added,
    /// In FROM but not TO.
    Deleted,
    /// In both, but the entry differs (checksum, mode, or size).
    Modified,
    /// In both and identical. Hidden unless `--all`.
    Unchanged,
}

impl Status {
    /// The porcelain letter for a changed status, or `None` for `Unchanged`
    /// (which has no `A|D|M` letter — it is surfaced under `--all` only).
    #[must_use]
    pub fn letter(self) -> Option<&'static str> {
        match self {
            Status::Added => Some("A"),
            Status::Deleted => Some("D"),
            Status::Modified => Some("M"),
            Status::Unchanged => None,
        }
    }

    /// The JSON `status` token, including the `unchanged` marker used by
    /// `--all`.
    #[must_use]
    pub fn json_token(self) -> &'static str {
        match self {
            Status::Added => "A",
            Status::Deleted => "D",
            Status::Modified => "M",
            Status::Unchanged => "=",
        }
    }
}

/// The conflict policy for an intra-side path collision (the SAME path with
/// DIFFERING content unioned across two refs on one side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflict {
    /// Default: a differing-content collision is a hard error.
    Error,
    /// The last ref contributing the path wins.
    LastWins,
}

/// A single diff result row: the classification + the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    /// The change classification.
    pub status: Status,
    /// The entry path (verbatim, `./`-prefixed, directories trailing `/`).
    pub path: String,
}

/// An intra-side collision: the same path carried DIFFERING content by two refs
/// unioned on one side, under [`OnConflict::Error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// The colliding path.
    pub path: String,
}

/// The comparable fingerprint of a manifest entry: everything that, when
/// changed, classifies the path as Modified.
///
/// For a FILE this is the full [`ManifestEntry`] tuple `(path_type,
/// permissions, checksum, size)`, so a mode-only OR a content/size delta both
/// surface as `M`.
///
/// For a DIRECTORY the `checksum` is the subtree MERKLE and `size` is the
/// derived directory size — both change whenever ANY descendant changes. A diff
/// reports the changed descendants on their OWN lines, so a directory must NOT
/// also surface as `M` merely because its children moved; we compare a
/// directory by `(path_type, permissions)` only. A genuine directory mode-only
/// change still surfaces as `M` (its permissions differ), and two refs carrying
/// the same directory with the same permissions never collide.
fn fingerprint(entry: &ManifestEntry) -> (snapdir_core::PathType, &str, Option<&str>, Option<u64>) {
    match entry.path_type {
        snapdir_core::PathType::File => (
            entry.path_type,
            entry.permissions.as_str(),
            Some(entry.checksum.as_str()),
            Some(entry.size),
        ),
        snapdir_core::PathType::Directory => {
            (entry.path_type, entry.permissions.as_str(), None, None)
        }
    }
}

/// Two entries describe the same content iff their fingerprints match (the
/// directory merkle/size are excluded — see [`fingerprint`]).
fn same_content(a: &ManifestEntry, b: &ManifestEntry) -> bool {
    fingerprint(a) == fingerprint(b)
}

/// Unions a side's manifests (in ref order) into a `path -> ManifestEntry` map.
///
/// Two refs carrying the SAME path with IDENTICAL content union silently. The
/// SAME path with DIFFERING content is a collision: under
/// [`OnConflict::Error`] the first such path is returned as `Err`; under
/// [`OnConflict::LastWins`] the later ref's entry overwrites the earlier one.
///
/// # Errors
///
/// Returns the first [`Collision`] when `on_conflict` is [`OnConflict::Error`]
/// and two refs disagree on a path's content.
pub fn union_side(
    manifests: &[Manifest],
    on_conflict: OnConflict,
) -> Result<BTreeMap<String, ManifestEntry>, Collision> {
    let mut map: BTreeMap<String, ManifestEntry> = BTreeMap::new();
    for manifest in manifests {
        for entry in manifest.entries() {
            match map.get(&entry.path) {
                Some(existing) if same_content(existing, entry) => {
                    // Identical content for the same path: no conflict.
                }
                Some(_existing) => match on_conflict {
                    OnConflict::Error => {
                        return Err(Collision {
                            path: entry.path.clone(),
                        });
                    }
                    OnConflict::LastWins => {
                        map.insert(entry.path.clone(), entry.clone());
                    }
                },
                None => {
                    map.insert(entry.path.clone(), entry.clone());
                }
            }
        }
    }
    Ok(map)
}

/// Classifies every path across the two side maps, returning the rows sorted by
/// path (byte order — the `BTreeMap` keys are already byte-ordered `String`s).
///
/// `include_unchanged` (the `--all` flag) keeps equal paths as
/// [`Status::Unchanged`]; otherwise they are dropped.
#[must_use]
pub fn classify(
    from: &BTreeMap<String, ManifestEntry>,
    to: &BTreeMap<String, ManifestEntry>,
    include_unchanged: bool,
) -> Vec<DiffEntry> {
    // Union of all paths, byte-sorted (BTreeSet iteration order).
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    for p in from.keys() {
        paths.insert(p.as_str());
    }
    for p in to.keys() {
        paths.insert(p.as_str());
    }

    let mut rows = Vec::new();
    for path in &paths {
        let in_from = from.get(*path);
        let in_to = to.get(*path);

        // `diff` reports FILE-level differences. A directory entry carries a
        // subtree merkle/size that changes whenever a descendant changes, and
        // those descendants are reported on their own lines — so a path that is
        // a directory on every side it appears in is NOT a diff row of its own
        // (added/deleted/modified subtrees surface via their file entries). A
        // file<->directory type change at a path is still a real difference and
        // falls through to the classification below.
        let is_dir = |e: &ManifestEntry| e.path_type == snapdir_core::PathType::Directory;
        let present_only_dirs = match (in_from, in_to) {
            (Some(f), Some(t)) => is_dir(f) && is_dir(t),
            (Some(e), None) | (None, Some(e)) => is_dir(e),
            (None, None) => false,
        };
        if present_only_dirs {
            continue;
        }

        let status = match (in_from, in_to) {
            (None, Some(_)) => Status::Added,
            (Some(_), None) => Status::Deleted,
            (Some(f), Some(t)) => {
                if same_content(f, t) {
                    Status::Unchanged
                } else {
                    Status::Modified
                }
            }
            (None, None) => unreachable!("a path in the union must be in at least one side"),
        };
        if status == Status::Unchanged && !include_unchanged {
            continue;
        }
        rows.push(DiffEntry {
            status,
            path: (*path).to_owned(),
        });
    }
    rows
}

/// Renders the diff rows as porcelain `X\t./path` lines (one per row, sorted by
/// path because `rows` already is). Unchanged rows (only present under `--all`)
/// use a `=` marker, which the porcelain consumer treats as "not A/D/M".
#[must_use]
pub fn render_porcelain(rows: &[DiffEntry]) -> String {
    let mut out = String::new();
    for row in rows {
        let letter = row.status.letter().unwrap_or("=");
        out.push_str(letter);
        out.push('\t');
        out.push_str(&row.path);
        out.push('\n');
    }
    out
}

/// Renders the diff rows as a JSON array of `{"status":"X","path":"./p"}`
/// objects. `status` is the `A|D|M` letter (or `=` for an unchanged row under
/// `--all`); `path` is JSON-string-escaped.
#[must_use]
pub fn render_json(rows: &[DiffEntry]) -> String {
    let mut out = String::from("[");
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"status\":\"");
        out.push_str(row.status.json_token());
        out.push_str("\",\"path\":\"");
        out.push_str(&json_escape(&row.path));
        out.push_str("\"}");
    }
    out.push(']');
    out
}

/// Minimal JSON string escaping for a manifest path (quote, backslash, and the
/// control chars JSON requires escaping). Manifest paths are UTF-8 text; the
/// remaining bytes pass through verbatim so unicode/space paths round-trip.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}
