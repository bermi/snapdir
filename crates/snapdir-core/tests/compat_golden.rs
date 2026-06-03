//! Backwards-compatibility golden tests (`compat_*`).
//!
//! These pin the **frozen byte-format contract** of snapdir using only embedded
//! recorded constants and the public `snapdir-core` API. No oracle, no
//! shelling out: every expected value below was produced by the original Bash
//! implementation (the `snapdir` / `snapdir-manifest` scripts, `b3sum
//! --no-names`) during the port and is documented in
//! `docs/rust-port/manifest-spec.md`. Now that the Bash implementation has been
//! removed from the branch, this module is the standalone contract anchor.
//!
//! Coverage:
//!
//! 1. Manifest line format + `sort -k5` ordering + `#`-comment handling, via a
//!    recorded multi-line manifest round-trip (`compat_manifest_*`).
//! 2. Directory checksum == BLAKE3 merkle of sorted/deduped children, equal to
//!    the `D`-line CHECKSUM field (`compat_directory_*`).
//! 3. Snapshot id == BLAKE3 of the `#`-stripped manifest text (trailing newline
//!    included), distinct from the root directory checksum (`compat_snapshot_*`).
//! 4. Sharded `.objects` / `.manifests` key layout (`compat_sharded_*`).
//! 5. Checksum modes md5 / sha256 / keyed-BLAKE3 (`compat_checksum_mode_*`).
//!
//! Constants reused from `crates/snapdir-core/tests/golden_b3sum.rs` (the
//! guide's empty-files + modified manifests, their dir checksums and snapshot
//! ids) plus the canonical multi-level `b3sum` fixture from `snapdir-manifest`'s
//! own suite. The recorded values are the same ones the 67-gate port verified.

use snapdir_core::store::{manifest_path, object_path};
use snapdir_core::{
    directory_checksum, snapshot_id, Blake3Hasher, Blake3KeyedHasher, Hasher, Manifest, Md5Hasher,
    PathType, Sha256Hasher,
};

// --- Recorded golden constants (oracle-derived; reused from golden_b3sum.rs) --

/// `printf '' | b3sum --no-names` — an empty file's content checksum.
const EMPTY_FILE_B3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
/// `printf 'foo\n' | b3sum --no-names` — the guide's `foo.txt` content checksum.
const FOO_FILE_B3: &str = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";

/// Root (`D ./`) merkle checksums of the two guide manifests.
const EMPTY_FILES_DIR_B3: &str = "dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b";
const MODIFIED_DIR_B3: &str = "4a0732cfb45ebe9d8d572fc4c77b759384bed029911e35f8859430b889427d4d";

/// Snapshot ids (`snapdir id`) of the two guide manifests.
const EMPTY_FILES_SNAPSHOT_ID: &str =
    "c678a299380893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857";
const MODIFIED_SNAPSHOT_ID: &str =
    "8af03a1bec09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be";

/// The guide's empty-files manifest, verbatim from
/// `utils/qa-fixtures/expected-guide-commands.txt` lines 1-3.
const EMPTY_FILES_MANIFEST: &str = "\
D 700 dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b 0 ./
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./foo.txt";

/// The guide's modified manifest, verbatim from
/// `utils/qa-fixtures/expected-guide-commands.txt` lines 13-15.
const MODIFIED_MANIFEST: &str = "\
D 700 4a0732cfb45ebe9d8d572fc4c77b759384bed029911e35f8859430b889427d4d 4 ./
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
F 600 49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92 4 ./foo.txt";

/// The canonical multi-level `b3sum` fixture from `snapdir-manifest`'s own
/// suite (deep tree: `./a/aa/aaa/…`, `./b/bb/bbb/…`, `./c/cc/ccc/…`, `./r1f`).
/// Pins the line format and `sort -k5` ordering across many entries.
const MULTILEVEL_MANIFEST: &str = "\
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

/// Snapshot id of `MULTILEVEL_MANIFEST`, recorded from the oracle:
/// `manifest | grep -v '^#' | b3sum --no-names` (trailing newline included).
const MULTILEVEL_SNAPSHOT_ID: &str =
    "10ff7d9a837670d1946b9188768eee0d78e25829767763430f08cb1622ed6c16";

/// `D`-line checksum of `./a/aa/aaa/` in `MULTILEVEL_MANIFEST`, whose direct
/// children are `aaa1f` (`5cfee4fb…`) and `aaa2f` (`3791f11a…`). The recorded
/// merkle value (sort -u + concat + b3sum of those two child checksums).
const MULTILEVEL_AAA_DIR_B3: &str =
    "8aed4caf45b22aa4c8a195945136e3a01f77864e91fabe2d9272feeee87ae334";

// --- 1. Manifest line format + sort + comment handling -----------------------

#[test]
fn compat_manifest_line_format_fields_and_round_trip() {
    // TYPE PERMISSIONS CHECKSUM SIZE PATH, single-space separated, parsed back
    // to typed fields, then Display reproduces the line byte-identically.
    let line = "F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt";
    let manifest = Manifest::parse(line).expect("single line parses");
    let entry = &manifest.entries()[0];
    assert_eq!(entry.path_type, PathType::File);
    assert_eq!(entry.permissions, "600");
    assert_eq!(entry.checksum, EMPTY_FILE_B3);
    assert_eq!(entry.size, 0);
    assert_eq!(entry.path, "./bar.txt");
    assert_eq!(manifest.to_string(), line);
}

#[test]
fn compat_manifest_round_trips_multiline_byte_identical() {
    // Parse -> Display of a recorded multi-line manifest stays byte-identical
    // for every guide/oracle fixture, proving format + ordering are frozen.
    for fixture in [EMPTY_FILES_MANIFEST, MODIFIED_MANIFEST, MULTILEVEL_MANIFEST] {
        let manifest = Manifest::parse(fixture).expect("fixture parses");
        assert_eq!(manifest.to_string(), fixture);
    }
}

#[test]
fn compat_manifest_sorts_by_path_sort_k5() {
    // Entries must be ordered by the PATH field (byte-wise `sort -k5`), not by
    // type or checksum. `./a/` (dir) precedes `./a/a1f` (file) on path bytes.
    let manifest = Manifest::parse(MULTILEVEL_MANIFEST).expect("parses");
    let paths: Vec<&str> = manifest.entries().iter().map(|e| e.path.as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(paths, sorted, "entries must be in sort -k5 path order");

    let idx_a_dir = paths.iter().position(|p| *p == "./a/").unwrap();
    let idx_a1f = paths.iter().position(|p| *p == "./a/a1f").unwrap();
    assert!(idx_a_dir < idx_a1f);
}

#[test]
fn compat_manifest_comment_and_blank_lines_excluded() {
    // `#`-comment lines and blank lines are dropped on parse (and so excluded
    // from the snapshot-id checksum). A manifest wrapped in comments/blanks
    // renders identically to the bare one.
    let wrapped = format!("# generated by snapdir\n\n{EMPTY_FILES_MANIFEST}\n\n# eof");
    let manifest = Manifest::parse(&wrapped).expect("parses");
    assert_eq!(manifest.entries().len(), 3);
    assert_eq!(manifest.to_string(), EMPTY_FILES_MANIFEST);
}

// --- 2. Directory checksum == merkle of children == D-line CHECKSUM ----------

#[test]
fn compat_directory_checksum_equals_d_line_field() {
    // For each recorded manifest, the BLAKE3 merkle of the root's direct
    // children reproduces the CHECKSUM field of its `D ./` line.
    let hasher = Blake3Hasher::new();
    for (fixture, expected_root) in [
        (EMPTY_FILES_MANIFEST, EMPTY_FILES_DIR_B3),
        (MODIFIED_MANIFEST, MODIFIED_DIR_B3),
    ] {
        let manifest = Manifest::parse(fixture).expect("parses");
        let root = manifest
            .entries()
            .iter()
            .find(|e| e.path == "./")
            .expect("has a root entry");
        let children: Vec<&str> = manifest
            .entries()
            .iter()
            .filter(|e| e.path != "./")
            .map(|e| e.checksum.as_str())
            .collect();
        assert_eq!(root.checksum, expected_root);
        assert_eq!(directory_checksum(children, &hasher), root.checksum);
    }
}

#[test]
fn compat_directory_checksum_nested_subdirectory() {
    // A nested directory's CHECKSUM is the merkle of ITS direct children only.
    // `./a/aa/aaa/` holds `aaa1f` + `aaa2f`; their sorted/deduped/concatenated
    // checksums hash to the recorded `D`-line value.
    let hasher = Blake3Hasher::new();
    let manifest = Manifest::parse(MULTILEVEL_MANIFEST).expect("parses");

    let aaa_dir = manifest
        .entries()
        .iter()
        .find(|e| e.path == "./a/aa/aaa/")
        .expect("has the ./a/aa/aaa/ entry");
    assert_eq!(aaa_dir.checksum, MULTILEVEL_AAA_DIR_B3);

    let aaa1f = manifest
        .entries()
        .iter()
        .find(|e| e.path == "./a/aa/aaa/aaa1f")
        .unwrap();
    let aaa2f = manifest
        .entries()
        .iter()
        .find(|e| e.path == "./a/aa/aaa/aaa2f")
        .unwrap();
    let children = [aaa1f.checksum.as_str(), aaa2f.checksum.as_str()];
    assert_eq!(directory_checksum(children, &hasher), aaa_dir.checksum);
}

#[test]
fn compat_directory_checksum_dedups_identical_children() {
    // The empty-files root holds two identical empty-file checksums; `sort -u`
    // collapses them to one before hashing — the recorded root value.
    let hasher = Blake3Hasher::new();
    let children = [EMPTY_FILE_B3, EMPTY_FILE_B3];
    assert_eq!(directory_checksum(children, &hasher), EMPTY_FILES_DIR_B3);
}

// --- 3. Snapshot id == BLAKE3 of #-stripped manifest text --------------------

#[test]
fn compat_snapshot_id_reproduces_recorded_ids() {
    // The public snapshot_id reproduces each recorded `(manifest -> id)` pair.
    let hasher = Blake3Hasher::new();
    for (fixture, expected_id) in [
        (EMPTY_FILES_MANIFEST, EMPTY_FILES_SNAPSHOT_ID),
        (MODIFIED_MANIFEST, MODIFIED_SNAPSHOT_ID),
        (MULTILEVEL_MANIFEST, MULTILEVEL_SNAPSHOT_ID),
    ] {
        let manifest = Manifest::parse(fixture).expect("parses");
        assert_eq!(snapshot_id(&manifest, &hasher), expected_id);
    }
}

#[test]
fn compat_snapshot_id_is_not_root_directory_checksum() {
    // Keystone distinction: the snapshot id hashes the whole manifest text, NOT
    // the root directory checksum. They must never be conflated.
    let hasher = Blake3Hasher::new();
    let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
    let id = snapshot_id(&manifest, &hasher);
    assert_eq!(id, EMPTY_FILES_SNAPSHOT_ID);
    assert_ne!(id, EMPTY_FILES_DIR_B3);
    assert_ne!(MODIFIED_SNAPSHOT_ID, MODIFIED_DIR_B3);
}

#[test]
fn compat_snapshot_id_ignores_comment_lines() {
    // Comment lines are excluded from the id checksum: wrapping the manifest in
    // `#`-comments yields the same recorded id.
    let hasher = Blake3Hasher::new();
    let with_comments = format!("# header\n{EMPTY_FILES_MANIFEST}\n# trailer");
    let manifest = Manifest::parse(&with_comments).expect("parses");
    assert_eq!(snapshot_id(&manifest, &hasher), EMPTY_FILES_SNAPSHOT_ID);
}

// --- 4. Sharded object / manifest key layout ---------------------------------

#[test]
fn compat_sharded_object_paths() {
    // Recorded `(hash -> .objects/<h0:3>/<h3:6>/<h6:9>/<h9:>)` pairs, cross-checked
    // against utils/qa-fixtures/expected-guide-commands.txt lines 9-10, 22, 25.
    assert_eq!(
        object_path(EMPTY_FILE_B3),
        ".objects/af1/349/b9f/5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_eq!(
        object_path(FOO_FILE_B3),
        ".objects/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92"
    );
}

#[test]
fn compat_sharded_manifest_paths() {
    // Recorded `(id -> .manifests/<id0:3>/<id3:6>/<id6:9>/<id9:>)` pairs. The
    // empty-files id is cross-checked against the fixture (line 8); the
    // multi-level id is derived from its recorded snapshot id constant.
    assert_eq!(
        manifest_path(EMPTY_FILES_SNAPSHOT_ID),
        ".manifests/c67/8a2/993/80893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857"
    );
    assert_eq!(
        manifest_path(MULTILEVEL_SNAPSHOT_ID),
        ".manifests/10f/f7d/9a8/37670d1946b9188768eee0d78e25829767763430f08cb1622ed6c16"
    );
}

// --- 5. Checksum modes: md5 / sha256 / keyed-BLAKE3 --------------------------

#[test]
fn compat_checksum_mode_md5_golden_vector() {
    // `--checksum-bin=md5sum`: leading lowercase-hex digest the oracle keeps
    // after `md5sum | cut -d' ' -f1`. md5("foo\n") matches the guide's foo.txt.
    let hasher = Md5Hasher::new();
    assert_eq!(hasher.hash_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(
        hasher.hash_hex(b"foo\n"),
        "d3b07384d113edec49eaa6238ad5ff00"
    );
}

#[test]
fn compat_checksum_mode_sha256_golden_vector() {
    // `--checksum-bin=sha256sum`: digest the oracle keeps after
    // `sha256sum | cut -d' ' -f1`. sha256("foo\n") matches the guide's foo.txt.
    let hasher = Sha256Hasher::new();
    assert_eq!(
        hasher.hash_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hasher.hash_hex(b"foo\n"),
        "b5bb9d8014a0f9b1d61e21e796d78dccdf1352f23cd32812f4850b878ae4944c"
    );
}

#[test]
fn compat_checksum_mode_keyed_blake3_derive_key() {
    // Keyed mode (`SNAPDIR_MANIFEST_CONTEXT`) == `b3sum --derive-key=<ctx>`,
    // i.e. blake3::derive_key(ctx, input). Pin one golden and confirm it differs
    // from the unkeyed default. (core never reads the env var; ctx is a param.)
    let context = "snapdir compat golden context";
    let input = b"foo\n";
    let keyed = Blake3KeyedHasher::new(context);

    let expected = blake3::Hash::from(blake3::derive_key(context, input))
        .to_hex()
        .to_string();
    assert_eq!(keyed.hash_hex(input), expected);
    assert_ne!(keyed.hash_hex(input), Blake3Hasher::new().hash_hex(input));
}

#[test]
fn compat_checksum_mode_md5_drives_merkle_and_snapshot_id() {
    // The merkle rule and snapshot id are hash-agnostic: swapping in MD5 must
    // run unchanged (dir = md5(sorted-unique-concat); id = md5(text + "\n")).
    let hasher = Md5Hasher::new();
    let dir = directory_checksum(["ccc", "aaa", "bbb", "bbb"], &hasher);
    assert_eq!(dir, hasher.hash_hex(b"aaabbbccc"));

    let manifest = Manifest::parse(EMPTY_FILES_MANIFEST).expect("parses");
    let text = format!("{manifest}\n");
    assert_eq!(
        snapshot_id(&manifest, &hasher),
        hasher.hash_hex(text.as_bytes())
    );
}
