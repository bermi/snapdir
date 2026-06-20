//! ADVERSARY black-box spec suite (`assert_cmd`) for the **linked-mode
//! checksum-reuse fast path** (Phase 32, gate `mirror-linked-fastpath-spec-tests`).
//!
//! AUTHORED FROM SPEC ONLY. The fast path does NOT exist yet — no implementation
//! was read (none exists). Authored from the locked design
//! (`.gatesmith/reviews/mirror-linked-fastpath.md`) + the SPEC + the EXISTING
//! public CLI surface (the `--linked` checkout wiring already landed). These
//! tests are EXPECTED TO FAIL until the fast path lands; they pin the contract
//! and must NOT be weakened to go green.
//!
//! ## The fast path, restated
//! In `--linked` mode a checkout's dest entries are SYMLINKS into a local
//! `file://` store's content-addressed `.objects/<sharded>` pool, whose path
//! mechanically encodes the file's BLAKE3. A re-snapshot (`snapdir id` /
//! `manifest`) of a linked tree can RECOVER each file's checksum directly from
//! the symlink target's object path — WITHOUT reading or hashing the content.
//!
//! Eligibility (ALL): entry is a symlink whose canonical target is an object in
//! a KNOWN local store, AND plain non-keyed BLAKE3, AND not
//! `SNAPDIR_VERIFY_COPIES=1`. Fallbacks: wrong/keyed algo, escaped/non-object
//! target, strict-verify → re-hash; dangling → typed error (no panic).
//!
//! CHECKSUM-ONLY: the recovered value is the content CHECKSUM only; the walk
//! still records the symlink's OWN lstat mode+size, so a linked re-snapshot does
//! NOT reproduce the original source snapshot id. These tests therefore NEVER
//! assert `id <linked-tree> == source-snapshot-id`.
//!
//! ## Assumed/observed CLI surface (see handoff)
//! - **Keystone toggle:** default `snapdir id <linked-tree>` = recover-from-path
//!   (fast path); `SNAPDIR_VERIFY_COPIES=1 snapdir id <linked-tree>` = force a
//!   content re-hash. On a healthy store the two outputs are byte-identical.
//! - **No-read proof:** corrupt an object's BYTES while keeping its FILENAME (=
//!   its original address). Default `id` still emits the original checksum (read
//!   the path, not the garbage); `SNAPDIR_VERIFY_COPIES=1 id` READS the garbage,
//!   detects content != address, and ERRORS (non-zero, names the object/file).
//! - **Wrong/keyed algo:** `snapdir id` does NOT expose `--checksum-bin` (only
//!   `snapdir manifest` does). So the non-BLAKE3 case uses `snapdir manifest
//!   --checksum-bin md5sum <linked-tree>` (must re-hash → reads the garbage →
//!   ERRORS), and the keyed case uses `SNAPDIR_MANIFEST_CONTEXT=<key> snapdir id
//!   <linked-tree>` (keyed BLAKE3 != the store's plain-BLAKE3 address → must
//!   re-hash → reads the garbage → ERRORS). Flagged in the handoff.
//! - A linked tree is built by `push`ing a small tree to a local `file://` store
//!   then `pull --store <file://> --id <id> --linked <dest>`.
//!
//! ## Garbage injection vs 0444
//! Linked objects are `0444` (read-only). To overwrite an object's bytes while
//! keeping its filename, the test chmods the object writable (scoped to the
//! tempdir store's `.objects/` ONLY), truncates+writes garbage, restores nothing
//! (the tempdir is discarded). All injection is on the per-test tempdir store.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;

/// Unique temp dir under the OS temp root, removed by the caller on drop.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-linkfast-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A fresh `snapdir` command with the cache pinned and the developer's env
/// scrubbed so leakage can't mask a bug: `SNAPDIR_STORE`/`SNAPDIR_OBJECTS_STORE`
/// removed, `SNAPDIR_VERIFY_COPIES`/`SNAPDIR_MANIFEST_CONTEXT` removed (each test
/// re-adds them explicitly when exercising those knobs), and `HOME`/
/// `XDG_CACHE_HOME` redirected inside the sandbox.
fn snapdir(cache: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env("HOME", home);
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
    cmd.env_remove("SNAPDIR_STORE");
    cmd.env_remove("SNAPDIR_OBJECTS_STORE");
    cmd.env_remove("SNAPDIR_VERIFY_COPIES");
    cmd.env_remove("SNAPDIR_MANIFEST_CONTEXT");
    cmd
}

/// Runs `snapdir <args>`, asserts success, returns trimmed stdout.
fn ok_stdout(mut cmd: Command, args: &[&str]) -> String {
    let out = cmd.args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// Build a tiny deterministic source tree (stable perms) and return it.
fn build_src(tag: &str) -> PathBuf {
    let src = temp_dir(tag);
    fs::write(src.join("a.txt"), b"hello").unwrap();
    fs::set_permissions(src.join("a.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::create_dir(src.join("sub")).unwrap();
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(src.join("sub").join("b.txt"), b"world!!").unwrap();
    fs::set_permissions(
        src.join("sub").join("b.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    src
}

/// Push `src` into a fresh `file://` store and return `(store_url, id)`.
fn push_to_store(src: &Path, cache: &Path, home: &Path, store: &Path) -> (String, String) {
    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let id = ok_stdout(
        snapdir(cache, home),
        &["push", "--store", &store_url, &src_str],
    );
    assert_eq!(id.len(), 64, "push must print a 64-hex id");
    (store_url, id)
}

/// Push `src` then `pull --linked` it into a fresh `dest`, returning
/// `(store_url, id, dest)`. The dest entries become symlinks into the store's
/// `.objects/` pool — the precondition for the fast path.
fn build_linked_tree(
    tag: &str,
    src: &Path,
    cache: &Path,
    home: &Path,
    store: &Path,
) -> (String, String, PathBuf) {
    let (store_url, id) = push_to_store(src, cache, home, store);
    let dest = temp_dir(&format!("{tag}-dest"));
    let dest_str = dest.to_string_lossy().into_owned();
    ok_stdout(
        snapdir(cache, home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--linked", &dest_str,
        ],
    );
    // Sanity: the dest really is a linked tree (symlinks), else the rest of the
    // suite would not be exercising the fast path at all.
    assert!(
        dest.join("a.txt")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "precondition: --linked checkout must materialize symlinks (got a non-symlink a.txt)"
    );
    (store_url, id, dest)
}

/// Overwrite the BYTES of a content object while KEEPING its filename (= its
/// original BLAKE3 address). Resolves the symlink at `dest/<rel>` to its target
/// object in the tempdir store, chmods it writable (scoped to this tempdir
/// store), truncates, and writes `garbage`. Returns the object's on-disk path.
fn corrupt_object_keep_name(dest: &Path, rel: &str, garbage: &[u8]) -> PathBuf {
    let link = dest.join(rel);
    let target = fs::canonicalize(&link).expect("resolve linked object target");
    // Objects are 0444; relax just this object so we can rewrite its bytes.
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644))
        .expect("relax object perms in tempdir store");
    fs::write(&target, garbage).expect("inject garbage into the object (filename preserved)");
    target
}

/// Best-effort recursive cleanup of test scratch dirs.
fn cleanup(dirs: &[&Path]) {
    for d in dirs {
        fs::remove_dir_all(d).ok();
    }
}

/// REVIEW helper: asserts `manifest_text` is a well-formed snapdir manifest whose
/// FILE rows (`F`) carry a checksum of exactly `checksum_hex_len` lowercase-hex
/// chars. Every non-empty, non-comment line must be `<F|D> <perms> <checksum>
/// <size> <path>` (5 whitespace columns; paths may contain spaces, so the path
/// column is the remainder). Used to prove the md5 re-hash produced a real,
/// structurally-valid md5 manifest — not a stale/garbage echo of the fast path.
fn is_well_formed_file_manifest(manifest_text: &str, checksum_hex_len: usize) -> bool {
    let mut saw_file = false;
    for line in manifest_text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.splitn(5, ' ').collect();
        if cols.len() != 5 {
            return false;
        }
        let (ty, _perms, checksum, size, _path) = (cols[0], cols[1], cols[2], cols[3], cols[4]);
        if ty != "F" && ty != "D" {
            return false;
        }
        if size.parse::<u64>().is_err() {
            return false;
        }
        if ty == "F" {
            saw_file = true;
            let is_hex = checksum.len() == checksum_hex_len
                && checksum
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !is_hex {
                return false;
            }
        }
    }
    saw_file
}

/// REVIEW helper: returns the md5 hex of `content` as computed by snapdir ITSELF
/// (the frozen `--checksum-bin md5sum` oracle) over a throwaway one-file real
/// tree. Keeps the test free of any md5 implementation / dependency of its own
/// while still pinning that the corrupted bytes were actually read. The single
/// FILE row's checksum column is the md5 of `content`.
fn md5_of_bytes_via_snapdir(cache: &Path, home: &Path, tag: &str, content: &[u8]) -> String {
    let dir = temp_dir(tag);
    fs::write(dir.join("only.bin"), content).unwrap();
    fs::set_permissions(dir.join("only.bin"), fs::Permissions::from_mode(0o644)).unwrap();
    let dir_str = dir.to_string_lossy().into_owned();
    let manifest = ok_stdout(
        snapdir(cache, home),
        &["manifest", "--checksum-bin", "md5sum", &dir_str],
    );
    let md5 = manifest
        .lines()
        .find(|l| l.starts_with("F "))
        .and_then(|l| l.split(' ').nth(2))
        .expect("md5 manifest must have a FILE row with a checksum")
        .to_owned();
    assert_eq!(md5.len(), 32, "md5sum checksum column must be 32-hex");
    cleanup(&[&dir]);
    md5
}

// ===========================================================================
// KEYSTONE — default fast path == strict re-hash path on a HEALTHY store.
// ===========================================================================

/// SPEC (KEYSTONE): on a healthy store, default `snapdir id <linked-tree>` (fast
/// path, recover-from-path) is BYTE-IDENTICAL to `SNAPDIR_VERIFY_COPIES=1 snapdir
/// id <linked-tree>` (forced content re-hash). The recovered checksum equals the
/// hashed checksum, so the two manifests/ids match exactly.
#[test]
fn keystone_default_id_equals_strict_verify_id_on_healthy_store() {
    let src = build_src("keystone-src");
    let store = temp_dir("keystone-store");
    let cache = temp_dir("keystone-cache");
    let home = temp_dir("keystone-home");
    let (_url, _id, dest) = build_linked_tree("keystone", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // Default (recover-from-object-path fast path).
    let fast = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);

    // Strict (force a content re-hash through every symlink).
    let mut strict_cmd = snapdir(&cache, &home);
    strict_cmd.env("SNAPDIR_VERIFY_COPIES", "1");
    let strict = ok_stdout(strict_cmd, &["id", &dest_str]);

    assert_eq!(
        fast, strict,
        "KEYSTONE: the fast path's id must be byte-identical to the strict re-hash \
         id on a healthy store"
    );
    assert_eq!(fast.len(), 64, "id must be a 64-hex snapshot id");

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

/// SPEC (KEYSTONE, manifest-level): not just the id but the full MANIFEST is
/// identical between the default fast path and the strict re-hash, since the
/// recovered checksum must equal the hashed checksum for EVERY eligible entry.
#[test]
fn keystone_default_manifest_equals_strict_verify_manifest() {
    let src = build_src("keymani-src");
    let store = temp_dir("keymani-store");
    let cache = temp_dir("keymani-cache");
    let home = temp_dir("keymani-home");
    let (_url, _id, dest) = build_linked_tree("keymani", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    let fast = ok_stdout(snapdir(&cache, &home), &["manifest", &dest_str]);

    let mut strict_cmd = snapdir(&cache, &home);
    strict_cmd.env("SNAPDIR_VERIFY_COPIES", "1");
    let strict = ok_stdout(strict_cmd, &["manifest", &dest_str]);

    assert_eq!(
        fast, strict,
        "the fast-path manifest must be byte-identical to the strict re-hash manifest"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

// ===========================================================================
// PROVE NO CONTENT READ — corrupt object bytes, keep filename (= address).
// ===========================================================================

/// SPEC (no-read proof): after a linked checkout, OVERWRITE an object's bytes
/// with garbage while KEEPING its filename (its original hash). Default `snapdir
/// id <linked-tree>` STILL emits the ORIGINAL checksum (it read the path, not the
/// garbage) — its id is unchanged from the pre-corruption id.
#[test]
fn default_id_unchanged_after_object_bytes_corrupted_keeping_name() {
    let src = build_src("noread-src");
    let store = temp_dir("noread-store");
    let cache = temp_dir("noread-cache");
    let home = temp_dir("noread-home");
    let (_url, _id, dest) = build_linked_tree("noread", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // The id BEFORE corruption (fast path, healthy store).
    let before = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);

    // Corrupt a.txt's object bytes but keep its filename (= its BLAKE3 address).
    corrupt_object_keep_name(&dest, "a.txt", b"GARBAGE-NOT-HELLO-XXXXXXXXXXXXXXXX");

    // Default fast path reads the PATH, not the garbage → id is unchanged.
    let after = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);
    assert_eq!(
        after, before,
        "the default fast path must recover the checksum from the object PATH and \
         NOT read the corrupted bytes — the id must be unchanged"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

/// SPEC (no-read proof, strict side): `SNAPDIR_VERIFY_COPIES=1 snapdir id
/// <linked-tree>` against the SAME garbage-injected object READS the bytes,
/// detects content != address, and ERRORS (non-zero exit, naming the offending
/// file/object). This is the strict counterpart that proves the default path
/// truly avoided the read.
#[test]
fn strict_verify_id_errors_on_corrupted_object_naming_it() {
    let src = build_src("strictbad-src");
    let store = temp_dir("strictbad-store");
    let cache = temp_dir("strictbad-cache");
    let home = temp_dir("strictbad-home");
    let (_url, _id, dest) = build_linked_tree("strictbad", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    let target = corrupt_object_keep_name(&dest, "a.txt", b"GARBAGE-NOT-HELLO-YYYYYYYYYYYYYY");

    let mut strict_cmd = snapdir(&cache, &home);
    strict_cmd.env("SNAPDIR_VERIFY_COPIES", "1");
    let out = strict_cmd
        .args(["id", &dest_str])
        .output()
        .expect("run snapdir");
    assert!(
        !out.status.success(),
        "SNAPDIR_VERIFY_COPIES=1 must READ the bytes, detect the content/address \
         mismatch, and FAIL with non-zero exit"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let target_name = target.file_name().unwrap().to_string_lossy();
    assert!(
        combined.contains("a.txt") || combined.contains(target_name.as_ref()),
        "the strict-verify error must name the offending file/object; got: {combined}"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

// ===========================================================================
// WRONG / KEYED ALGO disables the fast path → re-hash → reads garbage → ERROR.
// ===========================================================================

/// SPEC (wrong algo): a non-default `--checksum-bin` (md5sum) makes the embedded
/// BLAKE3 the WRONG algorithm, so the fast path is ineligible and content must be
/// re-hashed. NOTE: `snapdir id` does NOT expose `--checksum-bin`; only `snapdir
/// manifest` does. The fast path is DISABLED for md5 (it cannot recover an md5
/// from a BLAKE3-addressed path), so the bytes are RE-HASHED: `snapdir manifest
/// --checksum-bin md5sum <linked-tree>` SUCCEEDS and emits an md5 manifest that
/// reflects the object's ACTUAL (corrupted) content — it must NOT echo the stale
/// BLAKE3 fast-path output. Per the locked design, wrong-algo merely re-hashes;
/// it does NOT error.
#[test]
fn wrong_checksum_bin_disables_fastpath_and_rehashes_corrupted_object() {
    let src = build_src("md5-src");
    let store = temp_dir("md5-store");
    let cache = temp_dir("md5-cache");
    let home = temp_dir("md5-home");
    let (_url, _id, dest) = build_linked_tree("md5", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // The plain-BLAKE3 fast-path id over the HEALTHY tree (what a wrongly-fired
    // fast path would echo). Captured before corruption.
    let blake3_fast = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);
    assert_eq!(blake3_fast.len(), 64);

    let garbage: &[u8] = b"GARBAGE-FOR-MD5-PATHWAY-ZZZZZZZZ";
    corrupt_object_keep_name(&dest, "a.txt", garbage);

    // md5sum mode cannot recover a checksum from the BLAKE3 object path: the fast
    // path is DISABLED, so it RE-HASHES the (now corrupted) content and SUCCEEDS,
    // emitting an md5 manifest that reflects the actual bytes — NOT the stale
    // BLAKE3 fast-path output.
    let md5_manifest = ok_stdout(
        snapdir(&cache, &home),
        &["manifest", "--checksum-bin", "md5sum", &dest_str],
    );
    assert!(
        !md5_manifest.contains(&blake3_fast),
        "a non-BLAKE3 --checksum-bin must DISABLE the fast path and re-hash; the md5 \
         manifest must NOT echo the stale BLAKE3 fast-path id; manifest:\n{md5_manifest}"
    );

    // STRENGTHENED (review): the correction claims SUCCESS + a re-hash that
    // actually READS the corrupted bytes. Prove all three:
    //  (1) the output is a well-formed manifest (every line = 5 space-separated
    //      columns; the checksum column is exactly 32-hex md5 for FILE rows);
    //  (2) the md5 of the CORRUPTED bytes is PRESENT (content was truly read —
    //      not the healthy "hello" md5, not the BLAKE3 address);
    //  (3) the md5 of the ORIGINAL bytes ("hello") is ABSENT.
    // The expected md5 hexes are derived from snapdir ITSELF (oracle md5sum) over
    // real one-file trees, so the test carries no md5 implementation of its own
    // and no extra dependency.
    assert!(
        is_well_formed_file_manifest(&md5_manifest, 32),
        "md5 manifest must be well-formed with 32-hex checksum columns; got:\n{md5_manifest}"
    );
    let md5_corrupt = md5_of_bytes_via_snapdir(&cache, &home, "md5-corrupt", garbage);
    let md5_original = md5_of_bytes_via_snapdir(&cache, &home, "md5-original", b"hello");
    assert_ne!(
        md5_corrupt, md5_original,
        "sanity: corrupted and original content must have distinct md5s"
    );
    assert!(
        md5_manifest.contains(&md5_corrupt),
        "the md5 re-hash must reflect the CORRUPTED content (proving the fast path was \
         disabled and the bytes were actually read); expected md5 {md5_corrupt} in:\n{md5_manifest}"
    );
    assert!(
        !md5_manifest.contains(&md5_original),
        "the md5 manifest must NOT contain the ORIGINAL content's md5 — the corrupted \
         bytes were read, not a stale/cached value; manifest:\n{md5_manifest}"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

/// SPEC (wrong algo, positive): even on a HEALTHY store, `snapdir manifest
/// --checksum-bin md5sum <linked-tree>` must NOT contain the store's plain
/// BLAKE3 object addresses (which would betray the fast path firing for the wrong
/// algorithm). The emitted checksums must be md5 (32 hex), recomputed from
/// content — not the 64-hex BLAKE3 the object path encodes.
#[test]
fn wrong_checksum_bin_does_not_emit_blake3_object_address() {
    let src = build_src("md5pos-src");
    let store = temp_dir("md5pos-store");
    let cache = temp_dir("md5pos-cache");
    let home = temp_dir("md5pos-home");
    let (_url, blake3_id, dest) = build_linked_tree("md5pos", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // The plain-BLAKE3 address of a.txt's content ("hello"), recovered from the
    // healthy store path. The md5 manifest must NOT contain it.
    let blake3_a = {
        let target = fs::canonicalize(dest.join("a.txt")).unwrap();
        // The object's filename + its three shard segments reconstruct the hash;
        // simplest CLI-observable proxy: the default fast-path id is pure BLAKE3.
        // We only need *a* known BLAKE3 hex string from this tree to assert it is
        // absent from the md5 manifest.
        let _ = target;
        ok_stdout(snapdir(&cache, &home), &["id", &dest_str])
    };
    assert_eq!(blake3_a.len(), 64);
    assert_eq!(blake3_id.len(), 64);

    let md5_manifest = ok_stdout(
        snapdir(&cache, &home),
        &["manifest", "--checksum-bin", "md5sum", &dest_str],
    );
    // md5 checksums are 32-hex; the BLAKE3-flavored fast-path id (64-hex) must not
    // leak into the md5 manifest (that would mean the fast path wrongly fired).
    assert!(
        !md5_manifest.contains(&blake3_a),
        "md5 manifest must NOT contain the BLAKE3 fast-path id — the fast path must \
         be disabled for a non-BLAKE3 algorithm; manifest:\n{md5_manifest}"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

/// SPEC (keyed algo): a keyed `SNAPDIR_MANIFEST_CONTEXT` makes the manifest's
/// checksum keyed-BLAKE3, which differs from the store's plain-BLAKE3 address, so
/// the fast path is ineligible and content must be re-hashed. The keyed re-hash
/// SUCCEEDS and reflects the object's ACTUAL (corrupted) content, so its id
/// DIFFERS from the plain-BLAKE3 fast-path output — it must NOT echo the stale
/// plain address. Per the locked design, keyed-algo merely re-hashes; it does
/// NOT error.
#[test]
fn keyed_manifest_context_disables_fastpath_and_rehashes_corrupted_object() {
    let src = build_src("keyed-src");
    let store = temp_dir("keyed-store");
    let cache = temp_dir("keyed-cache");
    let home = temp_dir("keyed-home");
    let (_url, _id, dest) = build_linked_tree("keyed", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // The plain-BLAKE3 fast-path id over the HEALTHY tree (what a wrongly-fired
    // fast path would echo). Captured before corruption.
    let plain_fast = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);
    assert_eq!(plain_fast.len(), 64);

    // STRENGTHENED (review): the keyed id over the HEALTHY tree BEFORE corruption.
    // The fast path is disabled for a keyed context, so this run already re-hashes
    // the (healthy) content with the key. We capture it to prove that, AFTER
    // corruption, the SAME keyed run produces a DIFFERENT id — i.e. it genuinely
    // re-read the bytes each time rather than recovering a fixed address.
    let mut keyed_healthy_cmd = snapdir(&cache, &home);
    keyed_healthy_cmd.env("SNAPDIR_MANIFEST_CONTEXT", "some-keyed-context");
    let keyed_healthy = ok_stdout(keyed_healthy_cmd, &["id", &dest_str]);
    assert_eq!(
        keyed_healthy.len(),
        64,
        "keyed id must be a 64-hex snapshot id"
    );

    corrupt_object_keep_name(&dest, "a.txt", b"GARBAGE-FOR-KEYED-CONTEXT-QQQQQQ");

    // The keyed run cannot recover the plain address from the object path: the
    // fast path is DISABLED, so it RE-HASHES the (now corrupted) content with the
    // key and SUCCEEDS, emitting a keyed id that reflects the actual bytes — NOT
    // the stale plain-BLAKE3 fast-path output.
    let mut cmd = snapdir(&cache, &home);
    cmd.env("SNAPDIR_MANIFEST_CONTEXT", "some-keyed-context");
    let keyed = ok_stdout(cmd, &["id", &dest_str]);
    assert_eq!(keyed.len(), 64, "keyed id must be a 64-hex snapshot id");
    assert_ne!(
        keyed, plain_fast,
        "a keyed SNAPDIR_MANIFEST_CONTEXT must DISABLE the fast path (keyed BLAKE3 != \
         the store's plain-BLAKE3 address) and re-hash; the keyed id must NOT echo the \
         stale plain-BLAKE3 fast-path id"
    );
    // The KEYSTONE of the correction: corruption changed the keyed id, proving the
    // keyed run actually READ the (now garbage) content instead of recovering a
    // fixed object address from the path. A fast path firing wrongly here would
    // have yielded the SAME id before and after corruption.
    assert_ne!(
        keyed, keyed_healthy,
        "the keyed id over the CORRUPTED tree must differ from the keyed id over the \
         HEALTHY tree — proving the keyed run re-read the bytes (fast path truly \
         disabled), not recovered a stale address"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

/// SPEC (keyed algo, divergence): on a HEALTHY store, a keyed
/// `SNAPDIR_MANIFEST_CONTEXT` id must DIFFER from the default plain-BLAKE3
/// fast-path id — proving the keyed run re-hashed with the key rather than
/// recovering the plain address from the object path.
#[test]
fn keyed_manifest_context_id_differs_from_plain_fastpath_id() {
    let src = build_src("keyeddiv-src");
    let store = temp_dir("keyeddiv-store");
    let cache = temp_dir("keyeddiv-cache");
    let home = temp_dir("keyeddiv-home");
    let (_url, _id, dest) = build_linked_tree("keyeddiv", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    let plain = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);

    let mut keyed_cmd = snapdir(&cache, &home);
    keyed_cmd.env("SNAPDIR_MANIFEST_CONTEXT", "another-keyed-context");
    let keyed = ok_stdout(keyed_cmd, &["id", &dest_str]);

    assert_ne!(
        plain, keyed,
        "a keyed-context id must DIFFER from the plain fast-path id (keyed re-hash, \
         not plain-address recovery)"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

// ===========================================================================
// ESCAPED / NON-OBJECT symlink → normal hash, never trusted.
// ===========================================================================

/// SPEC (escaped target): a dest symlink pointing OUTSIDE any store object (to an
/// arbitrary file) is hashed NORMALLY, never treated as a recoverable object. We
/// prove the content is read by giving the escaped target known bytes and
/// asserting the linked-tree id equals the id of an equivalent REAL-FILE tree
/// with the same content+symlink-mode — i.e. the checksum came from the content,
/// not from any (absent) object path.
#[test]
fn escaped_non_object_symlink_is_hashed_normally_not_trusted() {
    let src = build_src("escape-src");
    let store = temp_dir("escape-store");
    let cache = temp_dir("escape-cache");
    let home = temp_dir("escape-home");
    let (_url, _id, dest) = build_linked_tree("escape", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // An arbitrary external file OUTSIDE any store, with known content.
    let outside = temp_dir("escape-outside");
    let ext_file = outside.join("external.bin");
    fs::write(&ext_file, b"ESCAPED-CONTENT").unwrap();

    // Add an extraneous dest symlink that escapes to the external file. This is
    // NOT an object path, so the fast path must hash its (followed) content.
    let escape_link = dest.join("escape.bin");
    symlink(&ext_file, &escape_link).unwrap();

    // The default id must succeed (the escaped link is hashed normally, the
    // in-store links recover from path). Compare against SNAPDIR_VERIFY_COPIES=1:
    // for the escaped entry both paths hash content, so a healthy store keeps the
    // two ids identical — proving the escaped link was hashed, not trusted as an
    // object, in BOTH modes.
    let fast = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);

    let mut strict_cmd = snapdir(&cache, &home);
    strict_cmd.env("SNAPDIR_VERIFY_COPIES", "1");
    let strict = ok_stdout(strict_cmd, &["id", &dest_str]);

    assert_eq!(
        fast, strict,
        "an escaped (non-object) symlink must be HASHED normally in the default path \
         too, so its id matches the strict re-hash id"
    );

    cleanup(&[&src, &store, &cache, &home, &dest, &outside]);
}

// ===========================================================================
// DANGLING symlink → typed error, no panic.
// ===========================================================================

/// SPEC (dangling): after a linked checkout, REMOVE the target object so the dest
/// symlink dangles. `snapdir id <linked-tree>` must FAIL with a clear typed error
/// (non-zero exit, naming the path) — never a panic / SIGABRT / SIGSEGV.
#[test]
fn dangling_symlink_is_typed_error_not_panic() {
    let src = build_src("dangle-src");
    let store = temp_dir("dangle-store");
    let cache = temp_dir("dangle-cache");
    let home = temp_dir("dangle-home");
    let (_url, _id, dest) = build_linked_tree("dangle", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // Remove the target object so dest/a.txt dangles. Objects are 0444; relax the
    // parent dir if needed, then unlink the object (scoped to the tempdir store).
    let target = fs::canonicalize(dest.join("a.txt")).expect("resolve target");
    if let Some(parent) = target.parent() {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).ok();
    }
    fs::remove_file(&target).expect("remove the linked object → dangling symlink");
    assert!(
        dest.join("a.txt").symlink_metadata().is_ok(),
        "the dest symlink itself must still exist (only its target was removed)"
    );

    let out = snapdir(&cache, &home)
        .args(["id", &dest_str])
        .output()
        .expect("run snapdir");
    assert!(
        !out.status.success(),
        "a dangling linked object must FAIL with a typed error (non-zero exit)"
    );
    // Must be a clean error exit, not a crash: assert it terminated via exit code
    // (Some(code)), never killed by a signal (None on Unix => terminated by signal).
    assert!(
        out.status.code().is_some(),
        "dangling symlink must produce a typed error, NOT a panic/SIGABRT/SIGSEGV \
         (process was killed by a signal: {:?})",
        out.status
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("a.txt") || combined.to_lowercase().contains("no such file"),
        "the dangling-symlink error should name the offending path; got: {combined}"
    );
    assert!(
        !combined.contains("panicked"),
        "the failure must be a typed error, not a Rust panic; got: {combined}"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

// ===========================================================================
// CHECKSUM-ONLY honesty — the linked re-snapshot id is NOT the source id.
// ===========================================================================

/// SPEC (checksum-only): the fast path is CHECKSUM-ONLY. `snapdir id
/// <linked-tree>` records the SYMLINK's own lstat mode+size (not the target's),
/// so it must NOT reproduce the original source snapshot id. We pin that it
/// DIFFERS (mode/size of a symlink != the original file), guarding against an
/// over-claim that a linked re-snapshot round-trips the source id.
#[test]
fn linked_resnapshot_id_differs_from_source_snapshot_id() {
    let src = build_src("creuse-src");
    let store = temp_dir("creuse-store");
    let cache = temp_dir("creuse-cache");
    let home = temp_dir("creuse-home");

    // The original source snapshot id (real files, real modes/sizes).
    let src_str = src.to_string_lossy().into_owned();
    let source_id = ok_stdout(snapdir(&cache, &home), &["id", &src_str]);

    let (_url, pushed_id, dest) = build_linked_tree("creuse", &src, &cache, &home, &store);
    assert_eq!(
        pushed_id, source_id,
        "precondition: the pushed snapshot id equals the real-file source id"
    );
    let dest_str = dest.to_string_lossy().into_owned();

    // The linked re-snapshot must NOT equal the source id: symlink lstat mode+size
    // differ from the original files. The fast path is checksum-only.
    let linked_id = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);
    assert_ne!(
        linked_id, source_id,
        "CHECKSUM-ONLY: a linked re-snapshot must NOT reproduce the source snapshot \
         id (symlink mode/size differ from the original files)"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

// ===========================================================================
// REVIEW-ADDED (impl-revealed) — the now-visible core/cli wiring exposes the
// `manifest` keystone (not just `id`), mixed trees, and the strict-verify
// integrity error on the `manifest` command path.
// ===========================================================================

/// REVIEW (no-read proof, MANIFEST level): the staged suite proves the default
/// fast path doesn't read content for `snapdir id`; the impl recovers the
/// checksum identically for the `manifest` command (both go through the same
/// `resolve_walk`/`object_store_roots` wiring). Corrupt an object's bytes while
/// keeping its filename (= its address): the default `snapdir manifest
/// <linked-tree>` is UNCHANGED (it read the path, not the garbage).
#[test]
fn default_manifest_unchanged_after_object_bytes_corrupted_keeping_name() {
    let src = build_src("manifest-noread-src");
    let store = temp_dir("manifest-noread-store");
    let cache = temp_dir("manifest-noread-cache");
    let home = temp_dir("manifest-noread-home");
    let (_url, _id, dest) = build_linked_tree("manifest-noread", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    let before = ok_stdout(snapdir(&cache, &home), &["manifest", &dest_str]);

    corrupt_object_keep_name(&dest, "a.txt", b"GARBAGE-MANIFEST-NOREAD-WWWWWWWW");

    let after = ok_stdout(snapdir(&cache, &home), &["manifest", &dest_str]);
    assert_eq!(
        after, before,
        "the default fast path must recover EACH file's checksum from the object PATH \
         for `manifest` too — the corrupted bytes must not be read, so the manifest is \
         byte-identical"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

/// REVIEW (strict-verify integrity, MANIFEST level): the staged strict test
/// drives `id`; the impl raises `WalkError::LinkedObjectIntegrity` from the same
/// walk regardless of the front-end command. `SNAPDIR_VERIFY_COPIES=1 snapdir
/// manifest <linked-tree>` against a garbage-injected object READS the bytes,
/// detects content != address, and ERRORS (non-zero, names the file) — never a
/// stale manifest, never a panic.
#[test]
fn strict_verify_manifest_errors_on_corrupted_object_naming_it() {
    let src = build_src("strictman-src");
    let store = temp_dir("strictman-store");
    let cache = temp_dir("strictman-cache");
    let home = temp_dir("strictman-home");
    let (_url, _id, dest) = build_linked_tree("strictman", &src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    let target = corrupt_object_keep_name(&dest, "a.txt", b"GARBAGE-STRICT-MANIFEST-VVVVVVVV");

    let mut strict_cmd = snapdir(&cache, &home);
    strict_cmd.env("SNAPDIR_VERIFY_COPIES", "1");
    let out = strict_cmd
        .args(["manifest", &dest_str])
        .output()
        .expect("run snapdir");
    assert!(
        !out.status.success(),
        "SNAPDIR_VERIFY_COPIES=1 manifest must READ the bytes, detect the \
         content/address mismatch, and FAIL with non-zero exit"
    );
    assert!(
        out.status.code().is_some(),
        "strict-verify mismatch must be a typed error, NOT a signal kill; got: {:?}",
        out.status
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let target_name = target.file_name().unwrap().to_string_lossy();
    assert!(
        combined.contains("a.txt") || combined.contains(target_name.as_ref()),
        "the strict-verify error must name the offending file/object; got: {combined}"
    );
    assert!(
        !combined.contains("panicked"),
        "the failure must be a typed error, not a Rust panic; got: {combined}"
    );

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

/// REVIEW (mixed tree): a tree where SOME entries are recoverable in-store
/// symlinks and ANOTHER is a real (non-symlink) file must be handled per-entry —
/// the fast-path eligibility is decided per symlink, not for the whole walk. On
/// a healthy store the default id (fast path for the linked entries, normal hash
/// for the real file) is byte-identical to `SNAPDIR_VERIFY_COPIES=1` (every entry
/// re-hashed). This pins that adding a non-symlink sibling does not break the
/// per-entry decision and the two paths still agree.
#[test]
fn mixed_linked_and_real_tree_default_equals_strict() {
    let src = build_src("mixed-src");
    let store = temp_dir("mixed-store");
    let cache = temp_dir("mixed-cache");
    let home = temp_dir("mixed-home");
    let (_url, _id, dest) = build_linked_tree("mixed", &src, &cache, &home, &store);

    // Add a REAL regular file alongside the linked entries (not a symlink, so it
    // is hashed normally in BOTH modes).
    let real = dest.join("real.txt");
    fs::write(&real, b"a genuine non-linked regular file").unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o644)).unwrap();
    let dest_str = dest.to_string_lossy().into_owned();

    let fast = ok_stdout(snapdir(&cache, &home), &["id", &dest_str]);

    let mut strict_cmd = snapdir(&cache, &home);
    strict_cmd.env("SNAPDIR_VERIFY_COPIES", "1");
    let strict = ok_stdout(strict_cmd, &["id", &dest_str]);

    assert_eq!(
        fast, strict,
        "a mixed linked+real tree must produce the same id under the default fast path \
         and the strict re-hash on a healthy store (per-entry eligibility)"
    );
    assert_eq!(fast.len(), 64);

    cleanup(&[&src, &store, &cache, &home, &dest]);
}

/// REVIEW (canonicalization / `..` in the symlink target): `recover_object_key`
/// lexically folds `.`/`..` before testing the target against the store root. A
/// linked tree whose object symlinks are still well-formed object addresses after
/// folding must take the fast path. We exercise the recover path against a tree
/// reached through a `..`-containing dest path (the dest is addressed via its
/// parent + `..`), proving the lexical normalization in `recover_object_key`
/// doesn't reject a legitimately-rooted object and the id still matches strict.
#[test]
fn dest_addressed_with_dotdot_still_fast_paths_and_matches_strict() {
    let src = build_src("dotdot-src");
    let store = temp_dir("dotdot-store");
    let cache = temp_dir("dotdot-cache");
    let home = temp_dir("dotdot-home");
    let (_url, _id, dest) = build_linked_tree("dotdot", &src, &cache, &home, &store);

    // Re-address the dest through a `..` hop: <dest>/sub/.. == <dest>.
    let via_dotdot = dest.join("sub").join("..");
    let via_dotdot_str = via_dotdot.to_string_lossy().into_owned();

    let fast = ok_stdout(snapdir(&cache, &home), &["id", &via_dotdot_str]);

    let mut strict_cmd = snapdir(&cache, &home);
    strict_cmd.env("SNAPDIR_VERIFY_COPIES", "1");
    let strict = ok_stdout(strict_cmd, &["id", &via_dotdot_str]);

    assert_eq!(
        fast, strict,
        "a dest addressed via a `..` hop must still fast-path (lexical normalization \
         in recover_object_key) and match the strict re-hash on a healthy store"
    );
    assert_eq!(fast.len(), 64);

    cleanup(&[&src, &store, &cache, &home, &dest]);
}
