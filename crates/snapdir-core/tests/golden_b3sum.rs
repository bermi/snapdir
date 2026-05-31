//! Golden BLAKE3 reproduction tests (`golden_b3sum`).
//!
//! Proves the in-process BLAKE3 engine reproduces the frozen golden manifest
//! hashes and snapshot ids **byte-for-byte**, using only the public
//! `snapdir-core` API (`Manifest` parsing + `directory_checksum` +
//! `Blake3Hasher`). No filesystem walk and no shelling out to `b3sum`: file
//! contents are hashed in-process with the `blake3` crate.
//!
//! Source of truth: `utils/qa-fixtures/expected-guide-commands.txt` and the
//! frozen Bash oracle (`snapdir` / `snapdir-manifest`). Every expected id below
//! was re-derived against the oracle, not trusted from a label:
//!
//! - empty file content `""`            -> `af1349b9…`
//! - file content `"foo\n"`             -> `49dc870d…`
//! - dir of two empty files (root)      -> `dba5865c…`  (`sort -u` collapses the
//!   two identical `af1349b9…` child checksums to one, then re-hashes)
//! - dir of `bar.txt`(empty)+`foo.txt`(`foo\n`) -> `4a0732cf…`
//! - snapshot id of the empty-files manifest    -> `c678a299…`
//! - snapshot id of the modified manifest       -> `8af03a1b…`
//!
//! The snapshot id is NOT the root directory checksum: the oracle computes it as
//! `manifest | grep -v '^#' | b3sum --no-names`, i.e. BLAKE3 over the full
//! manifest text **including its trailing newline** (`snapdir` line 259/762).
//! The root directory checksum (`dba5865c…` / `4a0732cf…`) is only the CHECKSUM
//! field of the `D ./` line.

use snapdir_core::{directory_checksum, Blake3Hasher, Manifest};

/// Golden file-content checksums (re-derived: `printf '' | b3sum --no-names`,
/// `printf 'foo\n' | b3sum --no-names`).
const EMPTY_FILE_B3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
const FOO_FILE_B3: &str = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";

/// Golden directory (merkle) checksums == the CHECKSUM field of each `D ./`.
const EMPTY_FILES_DIR_B3: &str = "dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b";
const MODIFIED_DIR_B3: &str = "4a0732cfb45ebe9d8d572fc4c77b759384bed029911e35f8859430b889427d4d";

/// Golden snapshot ids (`snapdir id`) == BLAKE3 of the whole manifest text
/// (trailing newline included).
const EMPTY_FILES_SNAPSHOT_ID: &str =
    "c678a299380893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857";
const MODIFIED_SNAPSHOT_ID: &str =
    "8af03a1bec09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be";

/// The guide's empty-files manifest, copied verbatim from
/// `utils/qa-fixtures/expected-guide-commands.txt` (lines 1-3). The fixture
/// file itself is never modified.
const EMPTY_FILES_MANIFEST: &str = "\
D 700 dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b 0 ./
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./foo.txt";

/// The guide's modified manifest (after `echo "foo" > foo.txt`), copied verbatim
/// from `utils/qa-fixtures/expected-guide-commands.txt`.
const MODIFIED_MANIFEST: &str = "\
D 700 4a0732cfb45ebe9d8d572fc4c77b759384bed029911e35f8859430b889427d4d 4 ./
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
F 600 49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92 4 ./foo.txt";

/// In-process BLAKE3 hex, equivalent to the oracle's `b3sum --no-names`. File
/// contents are hashed here directly via the `blake3` crate — never `b3sum`.
fn b3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The snapshot id is `manifest | grep -v '^#' | b3sum --no-names`. Parsing then
/// re-displaying drops comments and normalizes ordering; the oracle's manifest
/// text carries a trailing newline, so we append one before hashing.
fn snapshot_id(manifest: &Manifest) -> String {
    let mut text = manifest.to_string();
    text.push('\n');
    b3_hex(text.as_bytes())
}

#[test]
fn golden_b3sum_empty_file_content() {
    // An empty file's content checksum is blake3 of zero bytes.
    assert_eq!(b3_hex(b""), EMPTY_FILE_B3);
}

#[test]
fn golden_b3sum_foo_file_content() {
    // `echo "foo"` writes "foo\n" (4 bytes) -> the golden object id.
    assert_eq!(b3_hex(b"foo\n"), FOO_FILE_B3);
}

#[test]
fn golden_b3sum_empty_files_directory_checksum() {
    // Root of two empty files: both children hash af1349b9…; `sort -u` collapses
    // them to one, and the directory checksum is blake3 of that single value.
    let hasher = Blake3Hasher::new();
    let children = [EMPTY_FILE_B3, EMPTY_FILE_B3];
    assert_eq!(directory_checksum(children, &hasher), EMPTY_FILES_DIR_B3);
}

#[test]
fn golden_b3sum_modified_directory_checksum() {
    // Root of bar.txt(empty) + foo.txt("foo\n"): two distinct child checksums,
    // sorted + concatenated + re-hashed.
    let hasher = Blake3Hasher::new();
    let children = [EMPTY_FILE_B3, FOO_FILE_B3];
    assert_eq!(directory_checksum(children, &hasher), MODIFIED_DIR_B3);
}

#[test]
fn golden_b3sum_directory_checksum_is_root_d_line_checksum() {
    // Cross-check: the directory checksum we compute equals the CHECKSUM field of
    // the manifest's `D ./` root entry, parsed by the Manifest parser.
    let hasher = Blake3Hasher::new();

    let empty = Manifest::parse(EMPTY_FILES_MANIFEST).expect("empty-files manifest parses");
    let empty_root = empty
        .entries()
        .iter()
        .find(|e| e.path == "./")
        .expect("has a root entry");
    let empty_children: Vec<&str> = empty
        .entries()
        .iter()
        .filter(|e| e.path != "./")
        .map(|e| e.checksum.as_str())
        .collect();
    assert_eq!(empty_root.checksum, EMPTY_FILES_DIR_B3);
    assert_eq!(
        directory_checksum(empty_children, &hasher),
        empty_root.checksum
    );

    let modified = Manifest::parse(MODIFIED_MANIFEST).expect("modified manifest parses");
    let modified_root = modified
        .entries()
        .iter()
        .find(|e| e.path == "./")
        .expect("has a root entry");
    let modified_children: Vec<&str> = modified
        .entries()
        .iter()
        .filter(|e| e.path != "./")
        .map(|e| e.checksum.as_str())
        .collect();
    assert_eq!(modified_root.checksum, MODIFIED_DIR_B3);
    assert_eq!(
        directory_checksum(modified_children, &hasher),
        modified_root.checksum
    );
}

#[test]
fn golden_b3sum_empty_files_snapshot_id() {
    // `snapdir id` over the empty-files manifest.
    let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
    assert_eq!(snapshot_id(&manifest), EMPTY_FILES_SNAPSHOT_ID);
}

#[test]
fn golden_b3sum_modified_snapshot_id() {
    // `snapdir id` over the modified manifest.
    let manifest = Manifest::parse(MODIFIED_MANIFEST).expect("parses");
    assert_eq!(snapshot_id(&manifest), MODIFIED_SNAPSHOT_ID);
}

#[test]
fn golden_b3sum_snapshot_id_differs_from_root_directory_checksum() {
    // Guard the subtle distinction: the snapshot id hashes the whole manifest
    // text, NOT the root directory checksum. They must not be conflated.
    assert_ne!(EMPTY_FILES_SNAPSHOT_ID, EMPTY_FILES_DIR_B3);
    assert_ne!(MODIFIED_SNAPSHOT_ID, MODIFIED_DIR_B3);
}
