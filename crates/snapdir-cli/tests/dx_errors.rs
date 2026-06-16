//! Black-box spec suite for the 1.8.0 ERROR-MESSAGE + SILENT-WRONG contract
//! (phase 30, gate `dx-errors-spec-tests`).
//!
//! AUTHORED FROM THE SPEC ONLY — this suite pins the QUALITY of error messages
//! and rules out two CI-hostile "silent-wrong" bugs (§6: silent-empty-store and
//! the sync miscount). It is staged in `.gatesmith/pending-tests/` so the
//! workspace keeps compiling; the cli impl teammate moves it to
//! `crates/snapdir-cli/tests/dx_errors.rs` and wires it. Several tests here are
//! EXPECTED TO FAIL against the current binary — that is the point. They must not
//! be weakened to pass.
//!
//! SPEC under test (the six clauses, each test comments the clause it pins)
//! =======================================================================
//!  (1) MISSING STORE HINT — a transfer command that needs a store but has
//!      neither `--store` nor `SNAPDIR_STORE` must fail with an ACTIONABLE error
//!      that NAMES `--store` (and ideally `SNAPDIR_STORE`), not a bare
//!      "missing --store option" with no guidance.
//!  (2) SPLIT-STORE HINT — reading a SPLIT snapshot (objects in a separate pool)
//!      without `--objects-store` must MENTION the objects-store / split concept
//!      (name `--objects-store`), NOT a raw `object not found: <hash>`.
//!  (3) BAD --limit-rate — `--limit-rate bogus` must fail nonzero with a message
//!      that SHOWS the accepted forms (e.g. `10M`/`512K`/`1G`) or mentions
//!      "rate", not a bare opaque clap error.
//!  (4) UNKNOWN SNAPSHOT ID keeps its good hint — checkout of a nonexistent id
//!      still emits the existing actionable "did you ... fetch" style hint (a
//!      regression guard; this one should already pass).
//!  (5) §6 SILENT-EMPTY-STORE — `diff`/`sync` reading from a store LOCATION THAT
//!      DOES NOT EXIST must FAIL nonzero naming the bad/unreadable store; it must
//!      NOT silently treat it as an empty store and emit a fabricated full delta
//!      at exit 0. A store that EXISTS but is legitimately EMPTY must still behave
//!      correctly (no spurious error).
//!  (6) §6 SYNC MISCOUNT — `sync` (and `sync --dryrun`) counters must reflect
//!      UNIQUE OBJECT counts, not file-reference counts; and a FIRST sync into an
//!      EMPTY destination must NOT report a nonzero "skipped" count.
//!
//! These drive the REAL `snapdir` binary over `file://` stores with NO
//! credentials; every test is hermetic (per-test temp cache + temp stores/trees,
//! env store vars REMOVED so the developer's env cannot mask a bug). Substance is
//! pinned with case-insensitive line-contains so the impl keeps wording latitude.

// The crate enables `clippy::pedantic` workspace-wide; suppress test-only
// stylistic lints (mirroring the `#![allow(...)]` in sibling suites like
// `sync_split.rs`/`diff.rs`) so the staged suite compiles under `-D warnings`
// WITHOUT touching any assertion or behavior.
#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::doc_markdown
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ===========================================================================
// Harness (mirrors crates/snapdir-cli/tests/sync_split.rs + diff.rs)
// ===========================================================================

/// Path to the compiled `snapdir` binary under test.
fn snapdir_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// A unique temp directory; created and returned. `tag` only aids debugging.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-dxerr-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A path under the system temp dir that is GUARANTEED NOT TO EXIST (never
/// created). Used to point a store flag at an unreadable/typo'd location.
fn nonexistent_path(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-dxerr-MISSING-{tag}-{}-{:?}-no-such-store",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(!dir.exists(), "the nonexistent path must really be absent");
    dir
}

/// A `file://<dir>` store URI for `dir`.
fn file_url(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

/// Runs `snapdir <args>` with the cache pinned under `cache`, the store/objects
/// env vars REMOVED (so the developer's env cannot mask a bug), and any
/// `extra_env` applied. Returns the raw `Output`.
fn run_raw(args: &[&str], cache: &Path, extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(snapdir_bin());
    cmd.args(args)
        .env("SNAPDIR_CACHE_DIR", cache)
        .env_remove("SNAPDIR_STORE")
        .env_remove("SNAPDIR_OBJECTS_STORE");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("run snapdir")
}

/// Runs `snapdir <args>`, asserts SUCCESS, returns trimmed stdout.
fn run_ok(args: &[&str], cache: &Path) -> String {
    let out = run_raw(args, cache, &[]);
    assert!(
        out.status.success(),
        "snapdir {args:?} exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout)
        .expect("stdout is UTF-8")
        .trim_end()
        .to_owned()
}

/// stderr of an `Output`, lossy.
fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// stdout of an `Output`, lossy.
fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Builds a tree with deterministic perms so it manifests to a stable id.
/// `leaves` is `(relative_path, contents)`. Distinct contents => distinct blobs.
fn build_tree(dir: &Path, leaves: &[(&str, &[u8])]) {
    for (rel, bytes) in leaves {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    set_dir_perms_recursive(dir);
}

fn set_dir_perms_recursive(dir: &Path) {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        if entry.file_type().unwrap().is_dir() {
            set_dir_perms_recursive(&entry.path());
        }
    }
}

/// Number of leaf blobs under `<dir>/.objects/` (0 if absent).
fn count_objects(dir: &Path) -> usize {
    fn walk(dir: &Path, n: &mut usize) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let ft = match e.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if ft.is_dir() {
                    walk(&e.path(), n);
                } else if ft.is_file() {
                    *n += 1;
                }
            }
        }
    }
    let mut n = 0;
    walk(&dir.join(".objects"), &mut n);
    n
}

/// Pulls the FIRST integer token immediately preceding `word` out of `line`.
/// e.g. `parse_count("4 copied, 0 skipped", "skipped") == Some(0)`.
fn parse_count(line: &str, word: &str) -> Option<usize> {
    let idx = line.find(word)?;
    line[..idx]
        .split_whitespace()
        .next_back()
        .and_then(|tok| tok.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok())
}

// ===========================================================================
// (1) MISSING STORE HINT — actionable, names --store (+ SNAPDIR_STORE).
// ===========================================================================

/// Clause 1: `push <dir>` with neither `--store` nor `SNAPDIR_STORE` must fail
/// nonzero AND the error must NAME `--store` and, for actionability, also mention
/// the `SNAPDIR_STORE` env fallback — not a bare "missing --store option" that
/// gives no path to a fix. (Current binary prints exactly "missing --store
/// option" with no env mention, so this is EXPECTED TO FAIL until the message is
/// upgraded.)
#[test]
fn missing_store_push_names_store_flag_and_env() {
    let cache = temp_dir("ms-push-cache");
    let src = temp_dir("ms-push-src");
    build_tree(&src, &[("a.txt", b"hello")]);
    let src_str = src.to_string_lossy().into_owned();

    let out = run_raw(&["push", &src_str], &cache, &[]);
    assert!(
        !out.status.success(),
        "push with no --store and no SNAPDIR_STORE must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("--store"),
        "the missing-store error must NAME the --store flag; got: {}",
        stderr_of(&out)
    );
    // Actionable: it must also point at the env fallback so a user knows the two
    // ways to supply a store (this is the message-quality upgrade for 1.8.0).
    assert!(
        err.contains("snapdir_store"),
        "the missing-store error must also mention the SNAPDIR_STORE env fallback \
         to be actionable; got: {}",
        stderr_of(&out)
    );
}

/// Clause 1 (mirror, read side): `fetch --id <id>` with neither `--store` nor
/// `SNAPDIR_STORE` must likewise fail nonzero naming `--store` (and the env). The
/// read path (fetch/pull) needs a store just as the write path does.
#[test]
fn missing_store_fetch_names_store_flag_and_env() {
    let cache = temp_dir("ms-fetch-cache");
    let out = run_raw(&["fetch", "--id", &"0".repeat(64)], &cache, &[]);
    assert!(
        !out.status.success(),
        "fetch with no --store and no SNAPDIR_STORE must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("--store"),
        "the missing-store error must NAME the --store flag; got: {}",
        stderr_of(&out)
    );
    assert!(
        err.contains("snapdir_store"),
        "the missing-store error must also mention SNAPDIR_STORE; got: {}",
        stderr_of(&out)
    );
}

// ===========================================================================
// (2) SPLIT-STORE HINT — reading a split snapshot without the objects pool.
// ===========================================================================

/// Clause 2: a SPLIT snapshot (objects pushed to a separate `--objects-store`
/// pool, manifest in `--store`) cannot be FETCHED back from the manifest store
/// alone. The error must MENTION the objects-store / split concept (name
/// `--objects-store`), guiding the user to point at the pool — NOT a bare
/// `object not found: <hash>` with no clue that an objects pool is required.
/// (Current binary prints `object not found: <hash>` only, so EXPECTED TO FAIL.)
#[test]
fn split_snapshot_fetch_without_objects_store_hints_objects_store() {
    let cache = temp_dir("split-fetch-cache");
    let src = temp_dir("split-fetch-src");
    let mani = temp_dir("split-fetch-mani");
    let pool = temp_dir("split-fetch-pool");
    build_tree(&src, &[("a.txt", b"hello"), ("b.txt", b"world")]);
    let src_str = src.to_string_lossy().into_owned();
    let mani_url = file_url(&mani);
    let pool_url = file_url(&pool);

    // Push as a SPLIT store: objects -> pool, manifest -> mani.
    let id = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &mani_url,
            &src_str,
        ],
        &cache,
    );
    assert_eq!(id.len(), 64, "snapshot id is 64 hex chars");
    // Sanity: the manifest store really holds NO objects (they are in the pool).
    assert_eq!(
        count_objects(&mani),
        0,
        "split push must keep objects out of the manifest store"
    );
    assert!(count_objects(&pool) > 0, "the pool must hold the blobs");

    // Fetch from the manifest store WITHOUT pointing at the pool, fresh cache.
    let fresh = temp_dir("split-fetch-fresh");
    let out = run_raw(&["fetch", "--store", &mani_url, "--id", &id], &fresh, &[]);
    assert!(
        !out.status.success(),
        "fetching a split snapshot without --objects-store must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("--objects-store")
            || err.contains("objects-store")
            || err.contains("objects store")
            || err.contains("object pool")
            || err.contains("split"),
        "the split-read error must mention the objects-store / split concept \
         (name --objects-store), not just a raw 'object not found'; got: {}",
        stderr_of(&out)
    );
}

/// Clause 2 (mirror, pull): the same guidance must surface from `pull` (fetch +
/// checkout) of a split snapshot without `--objects-store` — a one-shot pull is
/// the most common way a user hits this, so its message must guide them too.
#[test]
fn split_snapshot_pull_without_objects_store_hints_objects_store() {
    let cache = temp_dir("split-pull-cache");
    let src = temp_dir("split-pull-src");
    let mani = temp_dir("split-pull-mani");
    let pool = temp_dir("split-pull-pool");
    build_tree(&src, &[("a.txt", b"hello"), ("b.txt", b"world")]);
    let src_str = src.to_string_lossy().into_owned();
    let mani_url = file_url(&mani);
    let pool_url = file_url(&pool);

    let id = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &mani_url,
            &src_str,
        ],
        &cache,
    );

    let fresh = temp_dir("split-pull-fresh");
    let dest = temp_dir("split-pull-dest");
    let dest_str = dest.to_string_lossy().into_owned();
    let out = run_raw(
        &["pull", "--store", &mani_url, "--id", &id, &dest_str],
        &fresh,
        &[],
    );
    assert!(
        !out.status.success(),
        "pulling a split snapshot without --objects-store must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("--objects-store")
            || err.contains("objects-store")
            || err.contains("objects store")
            || err.contains("object pool")
            || err.contains("split"),
        "the split-read error from pull must mention the objects-store / split \
         concept; got: {}",
        stderr_of(&out)
    );
}

// ===========================================================================
// (3) BAD --limit-rate — message shows the accepted forms.
// ===========================================================================

/// Clause 3: `--limit-rate bogus` must fail nonzero AND the error must SHOW the
/// accepted forms (mention `10M`/`512K`/`1G`, or at least "rate") so the user
/// learns the grammar — not an opaque rejection. (The arg cluster already rejects
/// it; this pins the MESSAGE quality. It may already pass.)
#[test]
fn bad_limit_rate_shows_accepted_forms() {
    let cache = temp_dir("lr-cache");
    let src = temp_dir("lr-src");
    build_tree(&src, &[("a.txt", b"x")]);
    let src_str = src.to_string_lossy().into_owned();
    // A throwaway store so the ONLY reason to fail is the bad --limit-rate value.
    let store = temp_dir("lr-store");
    let store_url = file_url(&store);

    let out = run_raw(
        &[
            "push",
            "--store",
            &store_url,
            "--limit-rate",
            "bogus",
            &src_str,
        ],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "an unparseable --limit-rate must fail nonzero; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("10m") || err.contains("512k") || err.contains("1g") || err.contains("rate"),
        "the --limit-rate error must show the accepted forms (e.g. 10M/512K/1G) \
         or mention 'rate'; got: {}",
        stderr_of(&out)
    );
}

// ===========================================================================
// (4) UNKNOWN SNAPSHOT ID keeps its good hint (regression guard — should pass).
// ===========================================================================

/// Clause 4: checking out a nonexistent id from an empty cache must STILL emit the
/// existing actionable "did you ... fetch" style hint (the message that tells a
/// user to fetch the snapshot first). This is a REGRESSION GUARD — it should
/// already pass against the current binary; it must keep passing after the 1.8.0
/// message work so the good hint is not lost in the refactor.
#[test]
fn checkout_unknown_id_keeps_fetch_hint() {
    let cache = temp_dir("hint-cache");
    let dest = temp_dir("hint-dest");
    let dest_str = dest.to_string_lossy().into_owned();

    // Empty cache + no store: the manifest cannot be found locally.
    let out = run_raw(
        &["checkout", "--id", &"0".repeat(64), &dest_str],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "checkout of an unknown id must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("fetch"),
        "the unknown-id error must keep its actionable 'did you ... fetch' hint; \
         got: {}",
        stderr_of(&out)
    );
}

// ===========================================================================
// (5) §6 SILENT-EMPTY-STORE — the keystone CI-hostile bug.
//
// CRITICAL DISTINCTION pinned by the paired tests below:
//   (a) a store LOCATION THAT DOES NOT EXIST (typo'd/nonexistent path) -> the
//       command MUST FAIL nonzero and name the bad/unreadable store. It must NOT
//       silently treat the missing location as an empty store and fabricate a
//       full `D`/`A` delta at exit 0.
//   (b) a store that EXISTS but is legitimately EMPTY (a real dir with no
//       manifests) MUST behave correctly (exit 0, no spurious error) — here the
//       delta against the empty side is the CORRECT answer, not a fabrication.
// The current binary FAILS (a) for diff (it prints the full delta at exit 0), so
// the nonexistent-store diff test is EXPECTED TO FAIL until the fix lands.
// ===========================================================================

/// Clause 5(a) DIFF: `diff --from <real> --to <NONEXISTENT>` must FAIL nonzero
/// and name the bad/unreadable store — NOT silently fabricate a full-deletion
/// porcelain at exit 0. This is the keystone: an unreadable destination that the
/// engine mistakes for "empty" would make CI report every file as deleted while
/// exiting green.
#[test]
fn diff_nonexistent_to_store_errors_not_silent_full_delta() {
    let cache = temp_dir("se-diff-nx-cache");
    let src = temp_dir("se-diff-nx-src");
    build_tree(&src, &[("a.txt", b"hello"), ("b.txt", b"world")]);
    let src_str = src.to_string_lossy().into_owned();

    // A real FROM store with one snapshot.
    let from = temp_dir("se-diff-nx-from");
    let from_url = file_url(&from);
    run_ok(&["push", "--store", &from_url, &src_str], &cache);

    // A TO store location that DOES NOT EXIST.
    let missing = nonexistent_path("se-diff-nx-to");
    let missing_url = file_url(&missing);

    let out = run_raw(
        &["diff", "--from", &from_url, "--to", &missing_url],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "diff against a NONEXISTENT --to store must FAIL (not silently print a \
         fabricated full delta at exit 0).\nstdout:\n{}\nstderr:\n{}",
        stdout_of(&out),
        stderr_of(&out)
    );
    // The error must reference the bad/unreadable store so the user can fix the
    // typo'd path (case-insensitive: name the store or the missing path).
    let err = stderr_of(&out).to_lowercase();
    let needle = missing
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_lowercase();
    assert!(
        err.contains(&needle)
            || err.contains("no such")
            || err.contains("not found")
            || err.contains("does not exist")
            || err.contains("unreadable")
            || err.contains("store"),
        "the error must name the bad/unreadable --to store; got: {}",
        stderr_of(&out)
    );
}

/// Clause 5(b) DIFF: a TO store that EXISTS but is legitimately EMPTY (a real dir
/// with no `.manifests/`) must still diff correctly — exit 0 and report every
/// FROM file as `D` (deleted) WITHOUT erroring. This pins that the fix for 5(a)
/// distinguishes "missing/unreadable" from "present but empty"; a fix that simply
/// errors on any objectless side would break this legitimate case.
#[test]
fn diff_existing_empty_to_store_is_valid_full_deletion() {
    let cache = temp_dir("se-diff-empty-cache");
    let src = temp_dir("se-diff-empty-src");
    build_tree(&src, &[("a.txt", b"hello"), ("b.txt", b"world")]);
    let src_str = src.to_string_lossy().into_owned();

    let from = temp_dir("se-diff-empty-from");
    let from_url = file_url(&from);
    run_ok(&["push", "--store", &from_url, &src_str], &cache);

    // A real, EXISTING, EMPTY directory as the TO store.
    let empty = temp_dir("se-diff-empty-to");
    let empty_url = file_url(&empty);

    let out = run_raw(
        &["diff", "--from", &from_url, "--to", &empty_url],
        &cache,
        &[],
    );
    assert!(
        out.status.success(),
        "diff against an EXISTING-EMPTY --to store must SUCCEED (exit 0); a real \
         empty store is not an error.\nstderr:\n{}",
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);
    // Every FROM file is gone in the empty TO -> a `D` per file. This delta is the
    // CORRECT answer here (the store really is empty), unlike the 5(a) case.
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with('D') && l.contains("./a.txt"))
            && stdout
                .lines()
                .any(|l| l.starts_with('D') && l.contains("./b.txt")),
        "an existing-empty TO must report the FROM files as deleted (D); got:\n{stdout}"
    );
}

/// Clause 5(a) SYNC: `sync --from <NONEXISTENT> --to <real>` must FAIL nonzero and
/// reference the bad/unreadable source store — it must NOT silently treat the
/// missing source as an empty store and exit 0 having copied nothing while
/// claiming success. (sync reads the source manifest similarly to diff, so the
/// same silent-empty hazard applies on the read side.)
#[test]
fn sync_nonexistent_from_store_errors_not_silent_success() {
    let cache = temp_dir("se-sync-nx-cache");

    // A real, valid id shape (never pushed anywhere) and a real, writable dest.
    let id = "0".repeat(64);
    let to = temp_dir("se-sync-nx-to");
    let to_url = file_url(&to);

    // A FROM store location that DOES NOT EXIST.
    let missing = nonexistent_path("se-sync-nx-from");
    let missing_url = file_url(&missing);

    let out = run_raw(
        &["sync", "--id", &id, "--from", &missing_url, "--to", &to_url],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "sync from a NONEXISTENT --from store must FAIL (not silently succeed \
         treating it as empty).\nstdout:\n{}\nstderr:\n{}",
        stdout_of(&out),
        stderr_of(&out)
    );
    // It must not have written a manifest into the dest as if a phantom snapshot
    // had been synced.
    let manifests = {
        let mut n = 0;
        fn walk(dir: &Path, n: &mut usize) {
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        walk(&e.path(), n);
                    } else {
                        *n += 1;
                    }
                }
            }
        }
        walk(&to.join(".manifests"), &mut n);
        n
    };
    assert_eq!(
        manifests, 0,
        "a failed sync from a missing source must not publish any dest manifest"
    );
}

// ===========================================================================
// (6) §6 SYNC MISCOUNT — counters report UNIQUE OBJECTS, fresh dest skips 0.
//
// Dedup-tree construction: four file entries (one/two/three/four) where one==two
// and three==four byte-for-byte, so the manifest references N=4 file entries but
// only M=2 UNIQUE objects (BLAKE3 dedup collapses the duplicates). The counters
// must report the UNIQUE-OBJECT count (2), not the file-reference count (4); and a
// FIRST sync into an EMPTY dest must report 0 skipped (nothing pre-exists). The
// current binary reports "4 copied" / "would copy 4 object(s)", so the
// object-count assertions are EXPECTED TO FAIL until the miscount is fixed.
// ===========================================================================

/// Builds a 4-file / 2-unique-object dedup tree and pushes it into a fresh store.
/// Returns `(store_dir, store_url, id, unique_objects)`.
fn dedup_capture(tag: &str, cache: &Path) -> (PathBuf, String, String, usize) {
    let src = temp_dir(&format!("{tag}-src"));
    // one==two (content "AAAA"), three==four (content "BBBB") => 2 unique blobs.
    build_tree(
        &src,
        &[
            ("one.txt", b"AAAA"),
            ("two.txt", b"AAAA"),
            ("three.txt", b"BBBB"),
            ("four.txt", b"BBBB"),
        ],
    );
    let src_str = src.to_string_lossy().into_owned();
    let store = temp_dir(&format!("{tag}-store"));
    let store_url = file_url(&store);
    let id = run_ok(&["push", "--store", &store_url, &src_str], cache);
    // Prove the dedup actually happened: the store holds exactly 2 unique blobs
    // even though the manifest references 4 file entries.
    let unique = count_objects(&store);
    assert_eq!(
        unique, 2,
        "the dedup tree must collapse 4 file refs to 2 unique objects; got {unique}"
    );
    (store, store_url, id, unique)
}

/// Clause 6 (live sync): a first sync of the dedup snapshot into an EMPTY dest
/// must report counts that reflect UNIQUE OBJECTS (2), NOT file references (4),
/// and must report 0 skipped (nothing pre-exists to skip on a fresh dest).
#[test]
fn sync_counts_unique_objects_and_fresh_dest_skips_zero() {
    let cache = temp_dir("mc-live-cache");
    let (_from, from_url, id, unique) = dedup_capture("mc-live", &cache);

    let to = temp_dir("mc-live-to");
    let to_url = file_url(&to);

    let out = run_raw(
        &["sync", "--id", &id, "--from", &from_url, "--to", &to_url],
        &cache,
        &[],
    );
    assert!(
        out.status.success(),
        "the dedup sync must succeed; stderr: {}",
        stderr_of(&out)
    );
    // The summary line is on stderr ("N copied, M skipped ...").
    let stderr = stderr_of(&out);
    let summary = stderr
        .lines()
        .find(|l| l.contains("copied"))
        .unwrap_or_else(|| panic!("expected a sync summary line with a copied count:\n{stderr}"));

    // (i) FRESH-DEST SKIP must be 0: nothing pre-exists in an empty dest.
    if let Some(skipped) = parse_count(summary, "skipped") {
        assert_eq!(
            skipped, 0,
            "a FIRST sync into an EMPTY dest must report 0 skipped (nothing \
             pre-exists to skip); got:\n{summary}"
        );
    }

    // (ii) The copied count must be the UNIQUE-OBJECT count (2), NOT the
    // file-reference count (4). The duplicate files share a blob, so only 2
    // objects are actually transferred.
    let copied = parse_count(summary, "copied")
        .unwrap_or_else(|| panic!("no copied count in summary:\n{summary}"));
    assert_eq!(
        copied, unique,
        "the 'copied' count must be the UNIQUE-OBJECT count ({unique}), not the \
         file-reference count (4); got:\n{summary}"
    );

    // Cross-check against the truth on disk: exactly `unique` blobs landed.
    assert_eq!(
        count_objects(&to),
        unique,
        "the dest must hold exactly {unique} unique objects after the sync"
    );
}

/// Clause 6 (dryrun): `sync --dryrun` must likewise report the UNIQUE-OBJECT count
/// (2), not the file-reference count (4). A dry-run that over-counts misleads the
/// operator about how much work a real sync will do. (Current binary prints
/// "would copy 4 object(s)", so this is EXPECTED TO FAIL.)
#[test]
fn sync_dryrun_counts_unique_objects_not_file_refs() {
    let cache = temp_dir("mc-dry-cache");
    let (_from, from_url, id, unique) = dedup_capture("mc-dry", &cache);

    let to = temp_dir("mc-dry-to");
    let to_url = file_url(&to);

    let out = run_raw(
        &[
            "sync", "--dryrun", "--id", &id, "--from", &from_url, "--to", &to_url,
        ],
        &cache,
        &[],
    );
    assert!(
        out.status.success(),
        "the dedup dry-run sync must succeed; stderr: {}",
        stderr_of(&out)
    );
    // The dry-run report may go to stdout or stderr; search both for the count
    // line mentioning objects/copy.
    let combined = format!("{}\n{}", stdout_of(&out), stderr_of(&out));
    let line = combined
        .lines()
        .find(|l| {
            let lc = l.to_lowercase();
            (lc.contains("object") || lc.contains("copy") || lc.contains("copied"))
                && l.chars().any(|c| c.is_ascii_digit())
        })
        .unwrap_or_else(|| panic!("expected a dry-run object-count line:\n{combined}"));

    // The COUNT must be the unique-object count (2), never the 4 file references.
    // Pin loosely on substance: the line must contain "2" and must NOT report "4
    // object(s)" (the file-ref miscount).
    assert!(
        line.contains(&unique.to_string()),
        "the dry-run must report the unique-object count ({unique}); got: {line}"
    );
    assert!(
        !line.replace("object(s)", "objects").contains("4 object"),
        "the dry-run must NOT report 4 object(s) (that is the file-reference \
         miscount; only {unique} unique objects exist); got: {line}"
    );

    // Dry-run must not actually copy anything.
    assert_eq!(
        count_objects(&to),
        0,
        "--dryrun must not write any objects to the dest"
    );
}

// ===========================================================================
// REVIEW ADDITIONS (impl now visible — pin the EXACT branches the src reveals)
// ===========================================================================

/// Clause 1 (literal tokens): the impl's missing-store message is exactly
/// `no store configured: pass --store <uri> or set the SNAPDIR_STORE environment
/// variable` (crates/snapdir-cli/src/cli.rs). The feature-suite tests lowercase
/// before matching; this pins the ACTUAL-CASE literal tokens `--store` AND
/// `SNAPDIR_STORE` (uppercase env name) so a future refactor cannot drop either
/// the flag or the env hint while still passing a case-folded check.
#[test]
fn missing_store_push_names_both_tokens_literally() {
    let cache = temp_dir("ms-lit-cache");
    let src = temp_dir("ms-lit-src");
    build_tree(&src, &[("a.txt", b"hello")]);
    let src_str = src.to_string_lossy().into_owned();

    let out = run_raw(&["push", &src_str], &cache, &[]);
    assert!(!out.status.success(), "push with no store must fail");
    let err = stderr_of(&out);
    assert!(
        err.contains("--store"),
        "missing-store error must literally name `--store`; got: {err}"
    );
    // The env fallback name is uppercase in the real message; pin it literally so
    // it cannot be silently down-cased or dropped.
    assert!(
        err.contains("SNAPDIR_STORE"),
        "missing-store error must literally name the `SNAPDIR_STORE` env var \
         (uppercase); got: {err}"
    );
}

/// Clause 2 (literal tokens): the split-read hint the impl adds
/// (`Engine::split_read_hint`) names BOTH alternative flags —
/// `--objects-store` and `--from-objects` — so the user learns the read-side and
/// sync-side names. Pin both literal tokens (the feature suite only requires one
/// of several alternatives), so neither flag name can be dropped from the hint.
#[test]
fn split_fetch_hint_names_both_objects_flags_literally() {
    let cache = temp_dir("split-lit-cache");
    let src = temp_dir("split-lit-src");
    let mani = temp_dir("split-lit-mani");
    let pool = temp_dir("split-lit-pool");
    build_tree(&src, &[("a.txt", b"hello"), ("b.txt", b"world")]);
    let src_str = src.to_string_lossy().into_owned();
    let mani_url = file_url(&mani);
    let pool_url = file_url(&pool);

    let id = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &mani_url,
            &src_str,
        ],
        &cache,
    );

    let fresh = temp_dir("split-lit-fresh");
    let out = run_raw(&["fetch", "--store", &mani_url, "--id", &id], &fresh, &[]);
    assert!(!out.status.success(), "split fetch without pool must fail");
    let err = stderr_of(&out);
    assert!(
        err.contains("--objects-store"),
        "the split hint must literally name `--objects-store`; got: {err}"
    );
    assert!(
        err.contains("--from-objects"),
        "the split hint must also literally name `--from-objects` (the sync-side \
         flag) so the user learns both names; got: {err}"
    );
}

/// Clause 2 (INVERSE branch the impl reveals): `split_read_hint` only fires when
/// NO objects pool was supplied (`self.globals.objects_store.is_none()`). When the
/// user DID pass `--objects-store` and the object is GENUINELY missing (the pool is
/// the wrong/empty one), the error must be the PLAIN `object not found` cause with
/// NO split hint — otherwise the hint would spuriously tell the user to do exactly
/// what they already did, masking a real corruption/wrong-pool error.
#[test]
fn missing_object_with_objects_store_gives_plain_error_no_split_hint() {
    let cache = temp_dir("inv-cache");
    let src = temp_dir("inv-src");
    let mani = temp_dir("inv-mani");
    let pool = temp_dir("inv-pool");
    build_tree(&src, &[("a.txt", b"hello"), ("b.txt", b"world")]);
    let src_str = src.to_string_lossy().into_owned();
    let mani_url = file_url(&mani);
    let pool_url = file_url(&pool);

    // Split push: manifest -> mani, objects -> pool.
    let id = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &mani_url,
            &src_str,
        ],
        &cache,
    );
    assert!(count_objects(&pool) > 0, "pool must hold the blobs");

    // Fetch WITH an --objects-store supplied, but pointed at a DIFFERENT, EMPTY
    // pool: the objects are genuinely missing there. Because a pool WAS supplied,
    // the impl must NOT add the split hint — it is the wrong advice here.
    let wrong_pool = temp_dir("inv-wrong-pool");
    let wrong_pool_url = file_url(&wrong_pool);
    assert_eq!(count_objects(&wrong_pool), 0, "the wrong pool is empty");

    let fresh = temp_dir("inv-fresh");
    let out = run_raw(
        &[
            "fetch",
            "--store",
            &mani_url,
            "--objects-store",
            &wrong_pool_url,
            "--id",
            &id,
        ],
        &fresh,
        &[],
    );
    assert!(
        !out.status.success(),
        "a genuinely missing object must still fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    // The plain cause must surface...
    assert!(
        err.contains("object not found") || err.contains("not found"),
        "a genuine missing object (pool supplied) must give the plain \
         'object not found' cause; got: {}",
        stderr_of(&out)
    );
    // ...and the split hint must NOT fire (it would be misleading: the user already
    // passed --objects-store). Guard against the hint's distinctive phrasing.
    assert!(
        !err.contains("re-run with --objects-store") && !err.contains("pushed with a split"),
        "the split hint must NOT fire when --objects-store was already supplied \
         (it would tell the user to do what they already did); got: {}",
        stderr_of(&out)
    );
}

/// Clause 6 (stronger dedup): a tree with SIX file references collapsing to THREE
/// unique objects (AAAA×2, BBBB×3, CCCC×1) must report `copied == 3` (the unique
/// count), NOT 6 (file refs), with 0 skipped on a fresh dest. A wider fan-out than
/// the 4→2 base case rules out an accidental "halve the count" coincidence and
/// pins that the dedup is by-checksum across an arbitrary multiplicity.
#[test]
fn sync_counts_three_unique_objects_across_six_refs() {
    let cache = temp_dir("mc3-cache");
    let src = temp_dir("mc3-src");
    // 6 file entries -> 3 unique blobs: AAAA (×2), BBBB (×3), CCCC (×1).
    build_tree(
        &src,
        &[
            ("a1.txt", b"AAAA"),
            ("a2.txt", b"AAAA"),
            ("b1.txt", b"BBBB"),
            ("b2.txt", b"BBBB"),
            ("b3.txt", b"BBBB"),
            ("c1.txt", b"CCCC"),
        ],
    );
    let src_str = src.to_string_lossy().into_owned();
    let from = temp_dir("mc3-from");
    let from_url = file_url(&from);
    let id = run_ok(&["push", "--store", &from_url, &src_str], &cache);

    // Ground truth: exactly 3 unique objects landed in the source store.
    let unique = count_objects(&from);
    assert_eq!(
        unique, 3,
        "6 file refs must collapse to 3 unique objects; got {unique}"
    );

    let to = temp_dir("mc3-to");
    let to_url = file_url(&to);
    let out = run_raw(
        &["sync", "--id", &id, "--from", &from_url, "--to", &to_url],
        &cache,
        &[],
    );
    assert!(
        out.status.success(),
        "the dedup sync must succeed; stderr: {}",
        stderr_of(&out)
    );
    let stderr = stderr_of(&out);
    let summary = stderr
        .lines()
        .find(|l| l.contains("copied"))
        .unwrap_or_else(|| panic!("expected a sync summary with a copied count:\n{stderr}"));

    // Fresh dest => 0 skipped.
    if let Some(skipped) = parse_count(summary, "skipped") {
        assert_eq!(
            skipped, 0,
            "a first sync into an EMPTY dest must report 0 skipped; got:\n{summary}"
        );
    }
    // copied must be the UNIQUE count (3), never the 6 file references.
    let copied = parse_count(summary, "copied")
        .unwrap_or_else(|| panic!("no copied count in summary:\n{summary}"));
    assert_eq!(
        copied, 3,
        "the 'copied' count must be the 3 UNIQUE objects, not the 6 file \
         references; got:\n{summary}"
    );
    // And the dest must physically hold exactly 3 unique blobs.
    assert_eq!(
        count_objects(&to),
        3,
        "the dest must hold exactly 3 unique objects after the sync"
    );
}
