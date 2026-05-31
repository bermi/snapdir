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
//! The **root directory's checksum is the snapshot id**.
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
/// To obtain the **snapshot id**, call this with the checksums of the root
/// directory's direct children; the returned value is the root directory
/// checksum.
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
        // directory checksum (== snapshot id) is blake3 of that single value.
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
    fn snapshot_id_equals_root_directory_checksum() {
        // The snapshot id is, by definition, the root directory checksum. This
        // documents that contract: computing the root dir checksum from the
        // root's direct children yields the snapshot id, here the guide root.
        let hasher = Blake3Hasher::new();
        let root_children = [EMPTY_BLAKE3, EMPTY_BLAKE3];
        let snapshot_id = directory_checksum(root_children, &hasher);
        assert_eq!(snapshot_id, TWO_EMPTY_FILES_ROOT_ID);
    }
}
