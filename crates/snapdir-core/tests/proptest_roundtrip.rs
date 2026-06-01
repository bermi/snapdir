//! Property tests (`proptest`) for the frozen manifest format.
//!
//! Proves the parse/emit round-trip property for the FROZEN manifest line
//! format using only the public `snapdir-core` API
//! (`ManifestEntry::{new, parse_line}` + `Display`, and `Manifest::{from_entries,
//! entries}` + `Display` + `parse`). It does NOT touch the frozen format source
//! (`manifest.rs`); it lives in this integration-test crate.
//!
//! ## The round-trip property
//!
//! For any VALID `ManifestEntry`, parsing its rendered text yields back an
//! equal entry:
//!
//! ```text
//! ManifestEntry::parse_line(&entry.to_string()) == Ok(entry)
//! ```
//!
//! and, at the document level, emitting a `Manifest` and parsing it back yields
//! the same entries modulo the format's defined `sort -k5` path ordering.
//!
//! ## Input strategy — every constraint is derived from the ACTUAL
//! `parse_line`/`Display` behavior in `manifest.rs` (READ ONLY), not invented:
//!
//! - `path_type`: only `F`/`D` exist (`PathType` has exactly two variants;
//!   `parse_line` rejects any other tag), so we select across both.
//! - `permissions`: `parse_line` rejects an EMPTY permissions field and splits
//!   fields on spaces, so a space inside this field would shift the columns.
//!   The oracle emits an octal string, so we generate `[0-7]{3,4}`
//!   (non-empty, space-free) — exactly what the format emits.
//! - `checksum`: same non-empty / space-free rule as permissions. `parse_line`
//!   is otherwise length- and charset-agnostic for this field, but the format
//!   emits lowercase hex, so we use `[0-9a-f]{1,64}`.
//! - `size`: `parse_line` does `size_str.parse::<u64>()`, so the full `u64`
//!   range round-trips; we generate `any::<u64>()`.
//! - `path`: `parse_line` takes the trailing field VERBATIM (it `splitn(5, ' ')`
//!   so only the first four spaces delimit). Therefore:
//!   - It MUST be non-empty (`parse_line` rejects an empty path field).
//!   - Spaces ARE allowed and preserved (trailing-field rule), so we include
//!     paths with embedded spaces, unicode, `./` prefixes and nested dirs.
//!   - It must contain NO `\n` (and, for the manifest-level test, no `\r`):
//!     `Display` renders one line and `Manifest::parse` splits on `.lines()`,
//!     which would split/strip those bytes — that is a line-framing rule of the
//!     format, not a content rule for a single field.

use proptest::prelude::*;
use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};

/// A `PathType` strategy across BOTH (and only) the real variants `F`/`D`.
fn path_type_strategy() -> impl Strategy<Value = PathType> {
    prop_oneof![Just(PathType::File), Just(PathType::Directory)]
}

/// A permissions strategy: octal `[0-7]{3,4}`, the way the oracle emits perms.
/// Non-empty and space-free, matching what `parse_line` accepts in field 2.
fn permissions_strategy() -> impl Strategy<Value = String> {
    "[0-7]{3,4}"
}

/// A checksum strategy: lowercase hex `[0-9a-f]{1,64}` — the format's emitted
/// charset. Non-empty and space-free (field 3 cannot contain a space).
fn checksum_strategy() -> impl Strategy<Value = String> {
    "[0-9a-f]{1,64}"
}

/// A path strategy producing VALID, round-trippable paths for a SINGLE entry.
///
/// Constraints (all from `parse_line`/`Display`): non-empty; no `\n` (line
/// framing). Spaces, unicode, `./` prefixes and nested dirs ARE allowed because
/// the path is the verbatim trailing field.
fn entry_path_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        // Plain non-empty paths with no newline; spaces/unicode allowed.
        "[^\n]+",
        // Relative `./`-prefixed paths, including spaces and nested dirs.
        "\\./[^\n]*",
        // Directory-style trailing-slash paths.
        "\\./[^\n]*/",
        // A few explicit awkward-but-valid cases (spaces, unicode, dot-dirs).
        Just("./a file with spaces.txt".to_owned()),
        Just("./nested/dir with spaces/déjà vu.txt".to_owned()),
        Just("./".to_owned()),
        Just("/tmp/absolute/path.txt".to_owned()),
        Just("./🦀/crab path.txt".to_owned()),
    ]
}

/// Like [`entry_path_strategy`] but ALSO excludes `\r`, for the document-level
/// test where `Manifest::parse` splits on `.lines()` (which treats `\r\n`/`\r`
/// as line boundaries). This is a line-framing rule of the format.
fn manifest_path_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        "[^\n\r]+",
        "\\./[^\n\r]*",
        "\\./[^\n\r]*/",
        Just("./a file with spaces.txt".to_owned()),
        Just("./nested/dir with spaces/déjà vu.txt".to_owned()),
        Just("./".to_owned()),
        Just("./🦀/crab path.txt".to_owned()),
    ]
}

/// Build an arbitrary VALID `ManifestEntry` (entry-level: path may contain `\r`).
fn entry_strategy() -> impl Strategy<Value = ManifestEntry> {
    (
        path_type_strategy(),
        permissions_strategy(),
        checksum_strategy(),
        any::<u64>(),
        entry_path_strategy(),
    )
        .prop_map(|(path_type, permissions, checksum, size, path)| {
            ManifestEntry::new(path_type, permissions, checksum, size, path)
        })
}

/// Build an arbitrary VALID `ManifestEntry` whose path also avoids `\r`
/// (document-level: lines must survive `.lines()` framing).
fn manifest_entry_strategy() -> impl Strategy<Value = ManifestEntry> {
    (
        path_type_strategy(),
        permissions_strategy(),
        checksum_strategy(),
        any::<u64>(),
        manifest_path_strategy(),
    )
        .prop_map(|(path_type, permissions, checksum, size, path)| {
            ManifestEntry::new(path_type, permissions, checksum, size, path)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// PRIMARY property: an arbitrary valid entry round-trips through
    /// `Display` -> `parse_line` unchanged.
    #[test]
    fn proptest_entry_parse_line_round_trips(entry in entry_strategy()) {
        let rendered = entry.to_string();
        let parsed = ManifestEntry::parse_line(&rendered);
        prop_assert_eq!(parsed, Ok(entry));
    }

    /// The rendered line is exactly the five space-joined fields, and parsing
    /// recovers each field verbatim (path keeps embedded spaces).
    #[test]
    fn proptest_entry_fields_recovered_verbatim(entry in entry_strategy()) {
        let parsed = ManifestEntry::parse_line(&entry.to_string())
            .expect("a valid entry must parse");
        prop_assert_eq!(parsed.path_type, entry.path_type);
        prop_assert_eq!(&parsed.permissions, &entry.permissions);
        prop_assert_eq!(&parsed.checksum, &entry.checksum);
        prop_assert_eq!(parsed.size, entry.size);
        prop_assert_eq!(&parsed.path, &entry.path);
    }

    /// DOCUMENT-level property: a small `Manifest` emitted via `Display` and
    /// parsed back yields the same entries, modulo the format's `sort -k5`
    /// path ordering (and stable order among equal paths).
    #[test]
    fn proptest_manifest_round_trips(
        entries in prop::collection::vec(manifest_entry_strategy(), 0..16)
    ) {
        let manifest = Manifest::from_entries(entries);
        let rendered = manifest.to_string();
        let reparsed = Manifest::parse(&rendered).expect("emitted manifest must parse");

        // Both sides are sorted by path with the same stable `sort -k5`
        // semantics, so the entry sequences must be identical.
        prop_assert_eq!(reparsed.entries(), manifest.entries());
        // And the document text is a fixed point under parse->emit.
        prop_assert_eq!(reparsed.to_string(), rendered);
    }
}
