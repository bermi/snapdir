//! Manifest line format model and (de)serialization.
//!
//! A snapdir manifest is a UTF-8 text file listing files and directories, one
//! per line, sorted by path. Each line has the exact shape:
//!
//! ```text
//! PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH
//! ```
//!
//! single-space separated, where:
//!
//! - `PATH_TYPE` is `F` for files, `D` for directories (directory paths end
//!   with `/`).
//! - `PERMISSIONS` is the octal permission string (e.g. `700`, `600`).
//! - `CHECKSUM` is the hex checksum of the entry.
//! - `SIZE` is the content size in bytes.
//! - `PATH` is the entry path. In relative mode paths are prefixed with `./`;
//!   with `--absolute` the full path is kept.
//!
//! This module owns only the *format* (the line model, [`Display`], and
//! parsing). It does NOT compute checksums, walk the filesystem, or stat
//! files — those land in later gates. Per the library-purity principle it
//! performs no terminal I/O and reads no environment.
//!
//! [`Display`]: std::fmt::Display

use core::fmt;
use core::str::FromStr;

use thiserror::Error;

/// The type of a manifest entry's path.
///
/// Mirrors the oracle's `PATH_TYPE` column: `F` for files, `D` for
/// directories. Symbolic links are recorded as the type of their target, so
/// only these two variants exist in a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathType {
    /// A regular file (`F`).
    File,
    /// A directory (`D`). Its rendered path always ends with `/`.
    Directory,
}

impl PathType {
    /// Returns the single-character tag used in a manifest line (`"F"` or
    /// `"D"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            PathType::File => "F",
            PathType::Directory => "D",
        }
    }
}

impl fmt::Display for PathType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors raised while parsing a manifest line or document.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// The line did not contain the five space-separated fields
    /// `PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH`.
    #[error("malformed manifest line (expected 'TYPE PERM CHECKSUM SIZE PATH'): {0:?}")]
    MalformedLine(String),
    /// The `PATH_TYPE` field was neither `F` nor `D`.
    #[error("invalid path type {0:?} (expected 'F' or 'D')")]
    InvalidPathType(String),
    /// The `SIZE` field was not a non-negative integer.
    #[error("invalid size field {0:?}")]
    InvalidSize(String),
}

/// A single manifest entry: one line of the manifest.
///
/// Field order matches the on-disk format exactly. `path` is stored verbatim
/// as it should be rendered (already `./`-prefixed in relative mode, or
/// absolute, and already trailing-`/` for directories).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// `F` (file) or `D` (directory).
    pub path_type: PathType,
    /// Octal permission string, e.g. `"700"`.
    pub permissions: String,
    /// Hex checksum of the entry's content (file bytes or directory merkle).
    pub checksum: String,
    /// Content size in bytes.
    pub size: u64,
    /// The rendered path. Directories end with `/`; relative paths start
    /// with `./`.
    pub path: String,
}

impl ManifestEntry {
    /// Builds an entry from its parts, taking the `path` exactly as it should
    /// be rendered.
    #[must_use]
    pub fn new(
        path_type: PathType,
        permissions: impl Into<String>,
        checksum: impl Into<String>,
        size: u64,
        path: impl Into<String>,
    ) -> Self {
        Self {
            path_type,
            permissions: permissions.into(),
            checksum: checksum.into(),
            size,
            path: path.into(),
        }
    }

    /// Parses a single, non-empty, non-comment manifest line.
    ///
    /// The line is split into exactly five fields on the first four spaces;
    /// the fifth field (the path) is taken verbatim so paths may contain
    /// spaces.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] if the line has fewer than five fields, an
    /// unknown path type, or a non-integer size.
    pub fn parse_line(line: &str) -> Result<Self, ParseError> {
        // Split on the first four spaces only; the path (field 5) keeps any
        // remaining spaces verbatim. `splitn(5, ' ')` gives at most 5 pieces.
        let mut parts = line.splitn(5, ' ');
        let type_str = parts
            .next()
            .ok_or_else(|| ParseError::MalformedLine(line.to_owned()))?;
        let permissions = parts
            .next()
            .ok_or_else(|| ParseError::MalformedLine(line.to_owned()))?;
        let checksum = parts
            .next()
            .ok_or_else(|| ParseError::MalformedLine(line.to_owned()))?;
        let size_str = parts
            .next()
            .ok_or_else(|| ParseError::MalformedLine(line.to_owned()))?;
        let path = parts
            .next()
            .ok_or_else(|| ParseError::MalformedLine(line.to_owned()))?;

        let path_type = match type_str {
            "F" => PathType::File,
            "D" => PathType::Directory,
            other => return Err(ParseError::InvalidPathType(other.to_owned())),
        };

        // Reject empty fields that `splitn` would otherwise tolerate, e.g. a
        // line with the right number of spaces but a blank permission/path.
        if permissions.is_empty() || checksum.is_empty() || path.is_empty() {
            return Err(ParseError::MalformedLine(line.to_owned()));
        }

        let size = size_str
            .parse::<u64>()
            .map_err(|_| ParseError::InvalidSize(size_str.to_owned()))?;

        Ok(Self::new(path_type, permissions, checksum, size, path))
    }
}

impl fmt::Display for ManifestEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} {} {}",
            self.path_type, self.permissions, self.checksum, self.size, self.path
        )
    }
}

/// An ordered collection of manifest entries.
///
/// [`Display`] reproduces the exact manifest text: entries sorted by path
/// (`sort -k5` semantics), one per line, joined by `\n` with no trailing
/// newline. Parsing strips empty lines and excludes `#`-comment lines.
///
/// [`Display`]: std::fmt::Display
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// Creates an empty manifest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Builds a manifest from entries, sorting them by path (`sort -k5`).
    #[must_use]
    pub fn from_entries(entries: Vec<ManifestEntry>) -> Self {
        let mut manifest = Self { entries };
        manifest.sort();
        manifest
    }

    /// Appends an entry. Call [`Manifest::sort`] (or use [`Manifest::display`]
    /// via [`Display`]) to restore path ordering afterwards.
    ///
    /// [`Display`]: std::fmt::Display
    pub fn push(&mut self, entry: ManifestEntry) {
        self.entries.push(entry);
    }

    /// Returns the entries in their current order.
    #[must_use]
    pub fn entries(&self) -> &[ManifestEntry] {
        &self.entries
    }

    /// Sorts entries by path, matching the oracle's `sort -k5` (a byte-wise
    /// ordering on the path field).
    pub fn sort(&mut self) {
        self.entries
            .sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    }

    /// Parses a manifest document: splits on newlines, strips empty lines,
    /// and excludes `#`-comment lines (which are also excluded from the
    /// checksum by the oracle). The remaining lines are parsed and sorted by
    /// path.
    ///
    /// # Errors
    ///
    /// Returns the first [`ParseError`] encountered on a non-empty,
    /// non-comment line.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut entries = Vec::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            entries.push(ManifestEntry::parse_line(line)?);
        }
        Ok(Self::from_entries(entries))
    }
}

impl fmt::Display for Manifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render in path order without mutating self.
        let mut order: Vec<&ManifestEntry> = self.entries.iter().collect();
        order.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let mut first = true;
        for entry in order {
            if first {
                first = false;
            } else {
                f.write_str("\n")?;
            }
            write!(f, "{entry}")?;
        }
        Ok(())
    }
}

impl FromStr for Manifest {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Manifest::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The canonical multi-level b3sum fixture from `snapdir-manifest`'s own
    // test suite (the frozen oracle). Used to pin the exact line format and
    // ordering.
    const ORACLE_B3SUM_MANIFEST: &str = "\
D 700 207d090daf06217a0920593ee642a90fcad85b9dccec02725e85311005f64327 43 ./
D 700 ed23cfd2037d23cf8c6b67497425e7a06d5e40ea2bd8e43fc434006022dafe86 21 ./a/
F 600 3c9cb8b8c8f3588f8e59e18d284330b0a951be644fbef2b9784b56e15d1c6096 4 ./a/a1f
D 700 ee795476bff6c1816b4c7558a74ee0b44ec600c3cde6b02564508f67d536a656 17 ./a/aa/
F 600 a2951028421deef48d1ba185f4c497c2d986f1dd76079baf2f5eb8479f132b5a 5 ./a/aa/aa1f
D 700 8aed4caf45b22aa4c8a195945136e3a01f77864e91fabe2d9272feeee87ae334 12 ./a/aa/aaa/
F 600 5cfee4fb4074748633b4ccbddb6b184a9b5e2f5ce74df6d2803f5fea0392a197 6 ./a/aa/aaa/aaa1f
F 600 3791f11a017feedffd24c2656e18d5c4ca9d6c404c8f40ccc511b6351c8575a6 6 ./a/aa/aaa/aaa2f
D 700 9a8b0e35c000df69893648b91d15cc30ab88ae5a40af48228caf5fa443dafc9b 12 ./b/
D 700 d41c2090167e6f546a510f0da98d8a8355d6bd2b61666644604c73b3a8f5b5d9 12 ./b/bb/
D 700 3b9023fa454aa22466feeb8cbf55a2c764dd79de0e93c9a793e8b54caec227da 12 ./b/bb/bbb/
F 600 8d18b7f3aabbef192a524fa2549d1d36b48c9030d234c9bdf87caa267fb09933 6 ./b/bb/bbb/bbb1f
F 600 2e16e172b6e337325f271d4eae00bc1ea20e41609ef78665710cada1477005cc 6 ./b/bb/bbb/bbb2f
D 700 15eb2657c1e6f5a24023c10429bb6f1b7d81b2cc2057eedee2192fbf3e7b892c 6 ./c/
D 700 e711f4e76ae9b3e25ad9a32b5f115cc9a81e55a428c552aa0bcab8543967f51a 6 ./c/cc/
D 700 31a1955d5a65328f31014650cf79b5c0c3d9b82de19352ade8d299cc22f6ec40 6 ./c/cc/ccc/
F 600 24f0cf3553e0dac0ce8aead4279e0fc368899e89ef776999d0d7e812b5ca0f3b 6 ./c/cc/ccc/ccc1f
F 600 27a55588c59999fd686667c4b186af08161b95c287216f0cde723f0e191d1974 4 ./r1f";

    #[test]
    fn manifest_entry_display_line_format() {
        let entry = ManifestEntry::new(
            PathType::File,
            "600",
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
            0,
            "./bar.txt",
        );
        assert_eq!(
            entry.to_string(),
            "F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt"
        );
    }

    #[test]
    fn manifest_directory_entry_display_keeps_trailing_slash() {
        let entry = ManifestEntry::new(
            PathType::Directory,
            "700",
            "dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b",
            0,
            "./",
        );
        assert_eq!(
            entry.to_string(),
            "D 700 dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b 0 ./"
        );
    }

    #[test]
    fn manifest_display_round_trips_oracle_b3sum_fixture() {
        // Parsing then displaying must reproduce the oracle byte-for-byte.
        let manifest = Manifest::parse(ORACLE_B3SUM_MANIFEST).expect("oracle parses");
        assert_eq!(manifest.to_string(), ORACLE_B3SUM_MANIFEST);
    }

    #[test]
    fn manifest_display_round_trips_empty_dir_guide_fixture() {
        // The empty-dir guide fixture (two duplicate empty files).
        let fixture = "\
D 700 dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b 0 ./
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./foo.txt";
        let manifest = Manifest::parse(fixture).expect("guide fixture parses");
        assert_eq!(manifest.to_string(), fixture);
    }

    #[test]
    fn manifest_sorts_entries_by_path_sort_k5() {
        // Insert out of order; Display must sort by path field.
        let mut manifest = Manifest::new();
        manifest.push(ManifestEntry::new(PathType::File, "600", "ccc", 4, "./r1f"));
        manifest.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            "aaa",
            0,
            "./",
        ));
        manifest.push(ManifestEntry::new(
            PathType::Directory,
            "700",
            "bbb",
            21,
            "./a/",
        ));
        manifest.push(ManifestEntry::new(
            PathType::File,
            "600",
            "ddd",
            4,
            "./a/a1f",
        ));

        let rendered = manifest.to_string();
        let expected = "\
D 700 aaa 0 ./
D 700 bbb 21 ./a/
F 600 ddd 4 ./a/a1f
F 600 ccc 4 ./r1f";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn manifest_sort_k5_orders_by_path_not_type_or_checksum() {
        // `sort -k5` keys on the path; a 'D' line can follow an 'F' line and a
        // larger checksum can precede a smaller one when the paths demand it.
        let parsed = Manifest::parse(ORACLE_B3SUM_MANIFEST).expect("parses");
        let paths: Vec<&str> = parsed.entries().iter().map(|e| e.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(paths, sorted, "entries must be in sort -k5 path order");
        // ./a/ (dir) sorts before ./a/a1f (file) because of the path bytes.
        let idx_a_dir = paths.iter().position(|p| *p == "./a/").unwrap();
        let idx_a1f = paths.iter().position(|p| *p == "./a/a1f").unwrap();
        assert!(idx_a_dir < idx_a1f);
    }

    #[test]
    fn manifest_parse_strips_empty_lines() {
        let with_blanks = "\n\nD 700 aaa 0 ./\n\nF 600 bbb 4 ./r1f\n\n";
        let manifest = Manifest::parse(with_blanks).expect("parses with blanks");
        assert_eq!(manifest.entries().len(), 2);
        assert_eq!(manifest.to_string(), "D 700 aaa 0 ./\nF 600 bbb 4 ./r1f");
    }

    #[test]
    fn manifest_parse_excludes_comment_lines() {
        // `#` lines are comments: excluded from the manifest (and the oracle's
        // checksum). They must not appear in the parsed entries or output.
        let with_comments = "\
# this is a comment header
D 700 aaa 0 ./
# another comment in the middle
F 600 bbb 4 ./r1f
#trailing comment without space";
        let manifest = Manifest::parse(with_comments).expect("parses with comments");
        assert_eq!(manifest.entries().len(), 2);
        assert_eq!(manifest.to_string(), "D 700 aaa 0 ./\nF 600 bbb 4 ./r1f");
    }

    #[test]
    fn manifest_relative_vs_absolute_path_rendering() {
        // Relative mode: paths prefixed with `./`.
        let relative = ManifestEntry::new(PathType::Directory, "700", "aaa", 43, "./");
        assert!(relative.to_string().ends_with(" ./"));

        // Absolute mode: the full path is kept verbatim (no `./` rewrite).
        let absolute = ManifestEntry::new(
            PathType::Directory,
            "700",
            "207d090daf06217a0920593ee642a90fcad85b9dccec02725e85311005f64327",
            43,
            "/tmp/files/",
        );
        assert_eq!(
            absolute.to_string(),
            "D 700 207d090daf06217a0920593ee642a90fcad85b9dccec02725e85311005f64327 43 /tmp/files/"
        );
        let abs_file = ManifestEntry::new(PathType::File, "600", "abc", 4, "/tmp/files/r1f");
        assert_eq!(abs_file.to_string(), "F 600 abc 4 /tmp/files/r1f");
    }

    #[test]
    fn manifest_entry_parse_line_round_trips() {
        let line =
            "F 600 a2951028421deef48d1ba185f4c497c2d986f1dd76079baf2f5eb8479f132b5a 5 ./a/aa/aa1f";
        let entry = ManifestEntry::parse_line(line).expect("parses");
        assert_eq!(entry.path_type, PathType::File);
        assert_eq!(entry.permissions, "600");
        assert_eq!(entry.size, 5);
        assert_eq!(entry.path, "./a/aa/aa1f");
        assert_eq!(entry.to_string(), line);
    }

    #[test]
    fn manifest_entry_parse_line_allows_spaces_in_path() {
        // Only the first four spaces delimit fields; the path keeps the rest.
        let line = "F 600 abc 4 ./a file with spaces.txt";
        let entry = ManifestEntry::parse_line(line).expect("parses");
        assert_eq!(entry.path, "./a file with spaces.txt");
        assert_eq!(entry.to_string(), line);
    }

    #[test]
    fn manifest_entry_parse_line_rejects_bad_type() {
        let err = ManifestEntry::parse_line("X 600 abc 4 ./x").unwrap_err();
        assert_eq!(err, ParseError::InvalidPathType("X".to_owned()));
    }

    #[test]
    fn manifest_entry_parse_line_rejects_bad_size() {
        let err = ManifestEntry::parse_line("F 600 abc notanumber ./x").unwrap_err();
        assert_eq!(err, ParseError::InvalidSize("notanumber".to_owned()));
    }

    #[test]
    fn manifest_entry_parse_line_rejects_too_few_fields() {
        let err = ManifestEntry::parse_line("F 600 abc 4").unwrap_err();
        assert_eq!(err, ParseError::MalformedLine("F 600 abc 4".to_owned()));
    }

    #[test]
    fn manifest_from_str_matches_parse() {
        let parsed: Manifest = ORACLE_B3SUM_MANIFEST.parse().expect("FromStr parses");
        assert_eq!(parsed.to_string(), ORACLE_B3SUM_MANIFEST);
    }
}
