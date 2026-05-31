//! Directory checksum (merkle) computation over manifest entries.
//!
//! snapdir derives a directory's checksum from the checksums of its **direct
//! children** — not from the directory's own bytes. The oracle
//! (`snapdir-manifest`) computes it with:
//!
//! ```sh
//! dir_checksums="$(echo "$dir_manifest" | cut -d' ' -f3 | sort -u | tr -d '\n')"
//! dir_checksum="$(echo -n "$dir_checksums" | _snapdir_manifest_checksum)"
//! ```
//!
//! that is: take the **CHECKSUM field** (column 3) of each direct child entry,
//! **sort** them lexicographically, **dedup** (`sort -u`), **concatenate with
//! no separator**, then **re-hash** the resulting byte string with the same
//! checksum function (BLAKE3 `--no-names` by default).
//!
//! The directory checksum is **not** the snapshot id. The root directory
//! checksum is just the CHECKSUM field of the `D ./` line. The **snapshot id**
//! is a distinct value: BLAKE3 of the full manifest text (with `#`-comment
//! lines removed, including the trailing newline `echo` adds). See
//! [`snapshot_id`].
//!
//! Edge cases, confirmed against the oracle:
//!
//! - An **empty directory** has no children, so the concatenation is the empty
//!   string and its checksum is `blake3("")` =
//!   `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`.
//! - Identical child checksums collapse under `sort -u`: a directory holding
//!   two empty files (both `af1349b9…`) hashes the single deduped value
//!   `af1349b9…`, yielding `dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b`.
//!
//! Per the library-purity principle this module performs no terminal I/O and
//! reads no environment; hashing is in-process via the [`blake3`] crate. We
//! never shell out to `b3sum`. The [`Hasher`] trait leaves room for the
//! `--checksum-bin` (md5/sha256) abstraction to slot in later without changing
//! the merkle algorithm.

use crate::manifest::Manifest;

/// A checksum function over an in-memory byte string.
///
/// The merkle rule is independent of the concrete hash: it sorts, dedups and
/// concatenates child checksum *strings* and hands the bytes to a `Hasher`.
/// The shipped default is in-process BLAKE3 ([`Blake3Hasher`]); the
/// `--checksum-bin` matrix (md5/sha256) can add further implementations later.
pub trait Hasher {
    /// Returns the lowercase hex checksum of `bytes`.
    fn hash_hex(&self, bytes: &[u8]) -> String;
}

/// In-process BLAKE3 hasher, equivalent to the oracle's default
/// `b3sum --no-names`.
///
/// This is the shipped default. It hashes the input bytes and renders the
/// 32-byte digest as lowercase hex, matching `b3sum --no-names` exactly.
#[derive(Debug, Clone, Copy, Default)]
pub struct Blake3Hasher;

impl Blake3Hasher {
    /// Creates a new BLAKE3 hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Hasher for Blake3Hasher {
    fn hash_hex(&self, bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }
}

/// Keyed (derived-key) BLAKE3 hasher, equivalent to the oracle's
/// `b3sum --derive-key="<context>" --no-names`.
///
/// The oracle switches to keyed mode whenever the `SNAPDIR_MANIFEST_CONTEXT`
/// environment variable is non-empty (and only for `b3sum`; see
/// `_snapdir_manifest_define_checksum_fn`). Per the library-purity principle,
/// `snapdir-core` does **not** read that environment variable: the CLI lane
/// reads it and constructs this hasher with the context string passed in as a
/// parameter.
///
/// The digest is `blake3::derive_key(context, input)` rendered as lowercase
/// hex, byte-for-byte identical to `b3sum --derive-key=<context> --no-names`.
#[derive(Debug, Clone)]
pub struct Blake3KeyedHasher {
    context: String,
}

impl Blake3KeyedHasher {
    /// Creates a keyed BLAKE3 hasher deriving its key from `context`.
    ///
    /// `context` is the `SNAPDIR_MANIFEST_CONTEXT` value the CLI lane reads
    /// from the environment; core never reads it itself.
    #[must_use]
    pub fn new(context: impl Into<String>) -> Self {
        Self {
            context: context.into(),
        }
    }
}

impl Hasher for Blake3KeyedHasher {
    fn hash_hex(&self, bytes: &[u8]) -> String {
        // `blake3::Hasher::new_derive_key(context)` then update+finalize is the
        // streaming equivalent of `blake3::derive_key(context, bytes)`; both
        // match `b3sum --derive-key=<context> --no-names`.
        let mut hasher = blake3::Hasher::new_derive_key(&self.context);
        hasher.update(bytes);
        hasher.finalize().to_hex().to_string()
    }
}

/// In-process MD5 hasher, equivalent to the oracle's
/// `md5sum | cut -d' ' -f1`.
///
/// The oracle parses non-`b3sum` checksum binaries with `cut -d' ' -f1`, i.e.
/// it keeps only the leading lowercase-hex digest and drops the filename. This
/// reproduces that digest in-process via the pure-Rust [`md5`](md_5) crate; we
/// never shell out to `md5sum`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Md5Hasher;

impl Md5Hasher {
    /// Creates a new MD5 hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Hasher for Md5Hasher {
    fn hash_hex(&self, bytes: &[u8]) -> String {
        use md5::{Digest, Md5};
        let digest = Md5::digest(bytes);
        hex_lower(&digest)
    }
}

/// In-process SHA-256 hasher, equivalent to the oracle's
/// `sha256sum | cut -d' ' -f1`.
///
/// As with [`Md5Hasher`], this reproduces the leading lowercase-hex digest the
/// oracle keeps after `cut -d' ' -f1`, in-process via the pure-Rust [`sha2`]
/// crate; we never shell out to `sha256sum`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sha256Hasher;

impl Sha256Hasher {
    /// Creates a new SHA-256 hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Hasher for Sha256Hasher {
    fn hash_hex(&self, bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        hex_lower(&digest)
    }
}

/// Renders raw digest bytes as a lowercase hex string.
fn hex_lower(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Infallible: writing to a String never errors.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Computes a directory checksum from the checksums of its direct children.
///
/// Implements the oracle rule exactly: each child checksum string is **sorted**
/// (lexicographic, byte-wise on the hex strings), **deduplicated** (`sort -u`),
/// **concatenated with no separator**, and the resulting byte string is hashed
/// with `hasher`.
///
/// `child_checksums` is the CHECKSUM field (column 3) of each direct child
/// manifest line. Passing an empty iterator yields the hash of the empty
/// string (an empty directory).
///
/// This is **not** the snapshot id. Computing it over the root directory's
/// direct children yields the CHECKSUM field of the `D ./` line, not the id
/// `snapdir id` reports. For the snapshot id, use [`snapshot_id`].
pub fn directory_checksum<'a, I, H>(child_checksums: I, hasher: &H) -> String
where
    I: IntoIterator<Item = &'a str>,
    H: Hasher,
{
    // `sort -u`: collect into a sorted, deduplicated set keyed on the hex
    // string bytes (BTreeSet orders by Ord, which for &str is byte-wise — the
    // same ordering `sort` uses in the C locale the oracle runs under).
    let unique: std::collections::BTreeSet<&str> = child_checksums.into_iter().collect();

    // `tr -d '\n'`: concatenate with no separator.
    let mut concatenated = String::new();
    for checksum in unique {
        concatenated.push_str(checksum);
    }

    hasher.hash_hex(concatenated.as_bytes())
}

/// Computes the **snapshot id** of a manifest.
///
/// This is the value `snapdir id` reports, and the id under which a snapshot is
/// stored. It is **distinct** from the root [`directory_checksum`]: the oracle
/// (`snapdir` lines 259, 762, 436, 776) derives it as
///
/// ```sh
/// snapshot_id="$(echo "$manifest" | grep -v '^#' | b3sum --no-names -)"
/// ```
///
/// i.e. BLAKE3 over the **full manifest text** with `#`-comment lines removed.
/// Because the oracle pipes the text through `echo`, the hashed bytes carry a
/// single **trailing newline** after the last manifest line; reproducing the
/// golden ids requires that newline.
///
/// [`Manifest`]'s [`Display`] already renders entries in `sort -k5` order with
/// no trailing newline and excludes comments on parse, so this renders the
/// manifest, appends the `echo` newline, and hashes the result with `hasher`.
///
/// [`Display`]: std::fmt::Display
#[must_use]
pub fn snapshot_id<H>(manifest: &Manifest, hasher: &H) -> String
where
    H: Hasher,
{
    let mut text = manifest.to_string();
    // The oracle's `echo "$manifest"` appends a single trailing newline before
    // the bytes reach `b3sum`. The golden ids only reproduce with it.
    text.push('\n');
    hasher.hash_hex(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `blake3("")` — the empty-input / empty-directory checksum, as emitted by
    /// `snapdir-manifest` for a truly empty directory (`D 700 af1349b9… 0 ./`).
    const EMPTY_BLAKE3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    /// The guide fixture root id: a directory containing two empty files, both
    /// `af1349b9…`. After `sort -u` they collapse to one, so the directory
    /// checksum is `blake3("af1349b9…")`.
    /// (`utils/qa-fixtures/expected-guide-commands.txt` line 4.)
    const TWO_EMPTY_FILES_ROOT_ID: &str =
        "dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b";

    #[test]
    fn blake3_hasher_matches_b3sum_no_names_for_empty_input() {
        let hasher = Blake3Hasher::new();
        assert_eq!(hasher.hash_hex(b""), EMPTY_BLAKE3);
    }

    #[test]
    fn empty_directory_checksum_is_hash_of_empty_string() {
        // No children -> concatenation is "" -> blake3("").
        let hasher = Blake3Hasher::new();
        let no_children: [&str; 0] = [];
        assert_eq!(directory_checksum(no_children, &hasher), EMPTY_BLAKE3);
    }

    #[test]
    fn directory_checksum_matches_guide_fixture_empty_files_root() {
        // The guide root dir holds two empty files (foo.txt, bar.txt), both
        // hashing to af1349b9…. `sort -u` dedups to a single value, and the
        // directory checksum is blake3 of that single value. (This is the
        // `D ./` CHECKSUM field, not the snapshot id.)
        let hasher = Blake3Hasher::new();
        let children = [EMPTY_BLAKE3, EMPTY_BLAKE3];
        assert_eq!(
            directory_checksum(children, &hasher),
            TWO_EMPTY_FILES_ROOT_ID
        );
    }

    #[test]
    fn directory_checksum_sorts_dedups_and_concatenates_in_order() {
        // Synthetic multi-child case verifying the exact sort+dedup+concat
        // pipeline independent of the hash: feed unsorted, duplicated child
        // checksums and confirm the hashed input equals the sorted-unique
        // concatenation.
        let hasher = Blake3Hasher::new();

        // Deliberately out of order, with a duplicate of "bbb".
        let children = ["ccc", "aaa", "bbb", "bbb"];
        let got = directory_checksum(children, &hasher);

        // Expected: sort -> [aaa, bbb, ccc], dedup (no-op here beyond the dup),
        // concat -> "aaabbbccc", then blake3 of those bytes.
        let expected = blake3::hash(b"aaabbbccc").to_hex().to_string();
        assert_eq!(got, expected);
    }

    #[test]
    fn directory_checksum_dedup_collapses_identical_children() {
        // All children identical -> a single value remains after `sort -u`.
        let hasher = Blake3Hasher::new();
        let children = ["zz", "zz", "zz"];
        let got = directory_checksum(children, &hasher);
        let expected = blake3::hash(b"zz").to_hex().to_string();
        assert_eq!(got, expected);
    }

    #[test]
    fn directory_checksum_is_order_independent_of_input_ordering() {
        // The merkle rule sorts, so input ordering must not affect the result.
        let hasher = Blake3Hasher::new();
        let forward = directory_checksum(["a1", "b2", "c3"], &hasher);
        let reverse = directory_checksum(["c3", "b2", "a1"], &hasher);
        assert_eq!(forward, reverse);
    }

    #[test]
    fn directory_checksum_is_the_root_d_line_field() {
        // The root directory checksum is the CHECKSUM field of the `D ./` line
        // — sort -u + concat + re-hash of the root's direct children. It is NOT
        // the snapshot id (see the snapshot_id_* tests for that distinction).
        let hasher = Blake3Hasher::new();
        let root_children = [EMPTY_BLAKE3, EMPTY_BLAKE3];
        let root_d_line_checksum = directory_checksum(root_children, &hasher);
        assert_eq!(root_d_line_checksum, TWO_EMPTY_FILES_ROOT_ID);
    }

    /// The guide's empty-files manifest (two duplicate empty files), copied
    /// verbatim from `utils/qa-fixtures/expected-guide-commands.txt`.
    const EMPTY_FILES_MANIFEST: &str = "\
D 700 dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b 0 ./
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./foo.txt";

    /// The guide's modified manifest (after `echo "foo" > foo.txt`).
    const MODIFIED_MANIFEST: &str = "\
D 700 4a0732cfb45ebe9d8d572fc4c77b759384bed029911e35f8859430b889427d4d 4 ./
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
F 600 49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92 4 ./foo.txt";

    /// Golden snapshot ids (`snapdir id`) == BLAKE3 of the whole manifest text
    /// with `#`-comment lines removed, trailing `echo` newline included.
    const EMPTY_FILES_SNAPSHOT_ID: &str =
        "c678a299380893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857";
    const MODIFIED_SNAPSHOT_ID: &str =
        "8af03a1bec09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be";

    #[test]
    fn snapshot_id_reproduces_empty_files_golden_id() {
        let hasher = Blake3Hasher::new();
        let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
        assert_eq!(snapshot_id(&manifest, &hasher), EMPTY_FILES_SNAPSHOT_ID);
    }

    #[test]
    fn snapshot_id_reproduces_modified_golden_id() {
        let hasher = Blake3Hasher::new();
        let manifest = Manifest::parse(MODIFIED_MANIFEST).expect("parses");
        assert_eq!(snapshot_id(&manifest, &hasher), MODIFIED_SNAPSHOT_ID);
    }

    #[test]
    fn snapshot_id_requires_the_trailing_newline() {
        // Guard the exact byte handling: the oracle's `echo` appends a trailing
        // newline, so hashing the manifest text WITHOUT it must NOT match the
        // golden id (and snapshot_id, which adds it, must).
        let hasher = Blake3Hasher::new();
        let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
        let without_newline = hasher.hash_hex(manifest.to_string().as_bytes());
        assert_ne!(without_newline, EMPTY_FILES_SNAPSHOT_ID);
        assert_eq!(snapshot_id(&manifest, &hasher), EMPTY_FILES_SNAPSHOT_ID);
    }

    #[test]
    fn snapshot_id_ignores_comment_lines() {
        // `#`-comment lines are stripped on parse, so a manifest with comments
        // yields the same snapshot id as one without.
        let hasher = Blake3Hasher::new();
        let with_comments = format!("# generated by snapdir\n{EMPTY_FILES_MANIFEST}\n# eof");
        let manifest = Manifest::parse(&with_comments).expect("parses");
        assert_eq!(snapshot_id(&manifest, &hasher), EMPTY_FILES_SNAPSHOT_ID);
    }

    // --- --checksum-bin abstraction: md5sum / sha256sum -------------------

    #[test]
    fn golden_md5_known_vectors() {
        // Standard MD5 test vectors, lowercase hex (what the oracle keeps after
        // `md5sum | cut -d' ' -f1`).
        let hasher = Md5Hasher::new();
        assert_eq!(hasher.hash_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        // md5("abc")
        assert_eq!(hasher.hash_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        // md5("foo\n") — matches `printf 'foo\n' | md5sum` (the guide's foo.txt).
        assert_eq!(
            hasher.hash_hex(b"foo\n"),
            "d3b07384d113edec49eaa6238ad5ff00"
        );
    }

    #[test]
    fn golden_md5_works_with_directory_checksum_and_snapshot_id() {
        // The merkle rule and snapshot id are hash-agnostic: they must run
        // unchanged with the MD5 hasher.
        let hasher = Md5Hasher::new();
        // directory_checksum = md5(sorted-unique-concat of children).
        let dir = directory_checksum(["ccc", "aaa", "bbb", "bbb"], &hasher);
        assert_eq!(dir, hasher.hash_hex(b"aaabbbccc"));

        // snapshot_id = md5(manifest text + trailing newline).
        let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
        let text = format!("{manifest}\n");
        assert_eq!(
            snapshot_id(&manifest, &hasher),
            hasher.hash_hex(text.as_bytes())
        );
    }

    #[test]
    fn golden_sha256_known_vectors() {
        // Standard SHA-256 test vectors, lowercase hex (what the oracle keeps
        // after `sha256sum | cut -d' ' -f1`).
        let hasher = Sha256Hasher::new();
        assert_eq!(
            hasher.hash_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // sha256("abc")
        assert_eq!(
            hasher.hash_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // sha256("foo\n") — matches `printf 'foo\n' | sha256sum`.
        assert_eq!(
            hasher.hash_hex(b"foo\n"),
            "b5bb9d8014a0f9b1d61e21e796d78dccdf1352f23cd32812f4850b878ae4944c"
        );
    }

    #[test]
    fn golden_sha256_works_with_directory_checksum_and_snapshot_id() {
        let hasher = Sha256Hasher::new();
        let dir = directory_checksum(["ccc", "aaa", "bbb", "bbb"], &hasher);
        assert_eq!(dir, hasher.hash_hex(b"aaabbbccc"));

        let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
        let text = format!("{manifest}\n");
        assert_eq!(
            snapshot_id(&manifest, &hasher),
            hasher.hash_hex(text.as_bytes())
        );
    }

    // --- keyed mode (SNAPDIR_MANIFEST_CONTEXT) ----------------------------

    #[test]
    fn keyed_context_matches_blake3_derive_key_and_differs_from_unkeyed() {
        // The oracle's keyed mode is `b3sum --derive-key=<ctx> --no-names`,
        // which is exactly `blake3::derive_key(ctx, input)`.
        let context = "snapdir 2026 test context";
        let input = b"the quick brown fox";

        let keyed = Blake3KeyedHasher::new(context);
        let expected = blake3::derive_key(context, input);
        let expected_hex = blake3::Hash::from(expected).to_hex().to_string();
        assert_eq!(keyed.hash_hex(input), expected_hex);

        // Keyed digest must differ from the unkeyed default.
        let unkeyed = Blake3Hasher::new();
        assert_ne!(keyed.hash_hex(input), unkeyed.hash_hex(input));

        // Different contexts produce different digests for the same input.
        let other = Blake3KeyedHasher::new("a different context");
        assert_ne!(keyed.hash_hex(input), other.hash_hex(input));
    }

    #[test]
    fn keyed_context_drives_directory_checksum_and_snapshot_id() {
        // Keyed hashing slots into the merkle rule / snapshot id like any other
        // Hasher; core never reads the env var — the context is a parameter.
        let context = "interop matrix context";
        let keyed = Blake3KeyedHasher::new(context);

        let dir = directory_checksum(["b", "a"], &keyed);
        assert_eq!(dir, keyed.hash_hex(b"ab"));

        let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
        let id = snapshot_id(&manifest, &keyed);
        let unkeyed_id = snapshot_id(&manifest, &Blake3Hasher::new());
        assert_ne!(id, unkeyed_id);
    }

    #[test]
    fn snapshot_id_differs_from_root_directory_checksum() {
        // The keystone distinction: the snapshot id hashes the whole manifest
        // text; it is NOT the root directory checksum for the same tree.
        let hasher = Blake3Hasher::new();
        let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
        let id = snapshot_id(&manifest, &hasher);

        let root_children: Vec<&str> = manifest
            .entries()
            .iter()
            .filter(|e| e.path != "./")
            .map(|e| e.checksum.as_str())
            .collect();
        let root_dir_checksum = directory_checksum(root_children, &hasher);

        assert_eq!(root_dir_checksum, TWO_EMPTY_FILES_ROOT_ID);
        assert_ne!(id, root_dir_checksum);
        assert_eq!(id, EMPTY_FILES_SNAPSHOT_ID);
    }
}
