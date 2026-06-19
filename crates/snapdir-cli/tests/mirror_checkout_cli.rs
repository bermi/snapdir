//! ADVERSARY black-box spec suite (`assert_cmd`) for `checkout`/`pull` exact-mirror
//! `--delete` + `--exclude`, and the carried-forward remote-source `--linked`
//! refusal. Phase 32.
//!
//! AUTHORED FROM SPEC ONLY — no `--delete`/mirror implementation was read (none
//! exists yet). These tests are EXPECTED TO FAIL until the impl lands; they pin
//! the contract and must NOT be weakened to go green.
//!
//! Assumed CLI surface (stated in the handoff): `checkout`/`pull` gain two new
//! flags on the transfer family — `--delete` (a bool: prune the destination to
//! an EXACT mirror of the manifest, removing anything not in the manifest) and
//! `--exclude <PATTERN>` (repeatable / comma-delimited, extended-regex like the
//! core `ExcludeMatcher`, protecting a matching otherwise-extraneous path from
//! pruning). The store URI is supplied via `--store <uri>` / `$SNAPDIR_STORE`,
//! the snapshot via `--id <id>`, and the destination directory is the trailing
//! positional arg. `--dryrun` lists the deletion set and removes nothing.
//!
//! SAFETY: the dangerous-dest tests (`/`, `$HOME`, cache dir, store path) are
//! scoped via an env'd `HOME`/`XDG_CACHE_HOME`/`SNAPDIR_CACHE_DIR` pointing at
//! tempdirs and sentinel files placed OUTSIDE the dest. They assert the command
//! REFUSES (non-zero exit) and that the sentinel survives. They are designed so
//! that even a buggy impl cannot delete a real `$HOME` or `/`, because `HOME`
//! is redirected to a tempdir for the `$HOME` case and the `/` case relies on
//! the refusal firing before any deletion (a sentinel under the env'd HOME is
//! asserted intact). No test ever points a real prune at `/` or the real home.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;

/// Unique temp dir under the OS temp root, removed by the caller on drop.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-mirror-{tag}-{}-{:?}",
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
/// scrubbed: `SNAPDIR_STORE`/`SNAPDIR_OBJECTS_STORE` removed so leakage cannot
/// mask a bug, and `HOME`/`XDG_CACHE_HOME` redirected at the given `home` temp
/// so the "default cache dir" / "$HOME" computations resolve INSIDE the test
/// sandbox (never the real home).
fn snapdir(cache: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env("HOME", home);
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
    cmd.env_remove("SNAPDIR_STORE");
    cmd.env_remove("SNAPDIR_OBJECTS_STORE");
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

/// Build a tiny deterministic source tree (stable perms so a checkout
/// re-manifests to the same id) and return it.
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

/// Best-effort recursive cleanup of test scratch dirs.
fn cleanup(dirs: &[&Path]) {
    for d in dirs {
        fs::remove_dir_all(d).ok();
    }
}

// ---------------------------------------------------------------------------
// PRUNING — extraneous content is removed, nested + idempotent + no-op cases.
// ---------------------------------------------------------------------------

/// SPEC: `--delete` prunes an extraneous top-level file from the dest, making it
/// an exact mirror of the manifest; in-manifest files survive untouched.
#[test]
fn delete_prunes_extraneous_top_level_file() {
    let src = build_src("prune-top-src");
    let store = temp_dir("prune-top-store");
    let dest = temp_dir("prune-top-dest");
    let cache = temp_dir("prune-top-cache");
    let home = temp_dir("prune-top-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);

    // First plain checkout to populate the dest, then add an extraneous file.
    let dest_str = dest.to_string_lossy().into_owned();
    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );
    fs::write(dest.join("EXTRANEOUS.txt"), b"junk").unwrap();
    assert!(dest.join("EXTRANEOUS.txt").exists());

    // Re-checkout WITH --delete: the extraneous file must be pruned; the
    // in-manifest files must remain.
    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );
    assert!(
        !dest.join("EXTRANEOUS.txt").exists(),
        "--delete must prune the extraneous file to make an exact mirror"
    );
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
    assert_eq!(
        fs::read(dest.join("sub").join("b.txt")).unwrap(),
        b"world!!"
    );
    // And the mirror re-manifests back to the source id.
    assert_eq!(ok_stdout(snapdir(&cache, &home), &["id", &dest_str]), id);

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

/// SPEC: nested extraneous directories are removed deepest-first (the prune-set
/// is deepest-first), so a populated extraneous subtree is fully gone.
#[test]
fn delete_prunes_nested_extraneous_dirs_deepest_first() {
    let src = build_src("prune-nest-src");
    let store = temp_dir("prune-nest-store");
    let dest = temp_dir("prune-nest-dest");
    let cache = temp_dir("prune-nest-cache");
    let home = temp_dir("prune-nest-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );
    // A non-empty extraneous nested subtree.
    fs::create_dir_all(dest.join("junk").join("deep")).unwrap();
    fs::write(dest.join("junk").join("deep").join("x"), b"x").unwrap();
    fs::write(dest.join("junk").join("y"), b"y").unwrap();

    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );
    assert!(
        !dest.join("junk").exists(),
        "the entire extraneous subtree must be removed deepest-first"
    );
    assert_eq!(ok_stdout(snapdir(&cache, &home), &["id", &dest_str]), id);

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

/// SPEC: `--delete` is idempotent — a second `--delete` run over an
/// already-exact mirror succeeds (exit 0) and removes nothing further.
#[test]
fn delete_is_idempotent_on_exact_mirror() {
    let src = build_src("idem-src");
    let store = temp_dir("idem-store");
    let dest = temp_dir("idem-dest");
    let cache = temp_dir("idem-cache");
    let home = temp_dir("idem-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );
    // Two consecutive --delete runs; both must succeed and the second is a no-op.
    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );
    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );
    assert_eq!(ok_stdout(snapdir(&cache, &home), &["id", &dest_str]), id);

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

/// SPEC: a dest that already EXACTLY matches the manifest yields no deletions
/// (the prune-set is empty); the checkout succeeds and the mirror is intact.
#[test]
fn delete_no_op_when_dest_already_matches() {
    let src = build_src("match-src");
    let store = temp_dir("match-store");
    let dest = temp_dir("match-dest");
    let cache = temp_dir("match-cache");
    let home = temp_dir("match-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // Checkout WITH --delete straight onto an empty dest, then again: a freshly
    // materialized dest already matches, so --delete prunes nothing.
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--delete", &dest_str,
        ],
    );
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
    assert_eq!(
        fs::read(dest.join("sub").join("b.txt")).unwrap(),
        b"world!!"
    );
    assert_eq!(ok_stdout(snapdir(&cache, &home), &["id", &dest_str]), id);

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

// ---------------------------------------------------------------------------
// DRYRUN — lists deletions, removes nothing.
// ---------------------------------------------------------------------------

/// SPEC: `--delete --dryrun` LISTS the exact deletion set (on stdout or stderr)
/// and deletes NOTHING — the extraneous file must still exist afterward.
#[test]
fn delete_dryrun_lists_deletions_and_removes_nothing() {
    let src = build_src("dry-src");
    let store = temp_dir("dry-store");
    let dest = temp_dir("dry-dest");
    let cache = temp_dir("dry-cache");
    let home = temp_dir("dry-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );
    fs::write(dest.join("WOULD_DELETE.txt"), b"junk").unwrap();

    let out = snapdir(&cache, &home)
        .args(["checkout", "--id", &id, "--delete", "--dryrun", &dest_str])
        .output()
        .expect("run snapdir");
    assert!(
        out.status.success(),
        "dryrun checkout --delete should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("WOULD_DELETE.txt"),
        "--dryrun must LIST the exact path it would delete; got: {combined}"
    );
    assert!(
        dest.join("WOULD_DELETE.txt").exists(),
        "--dryrun must NOT actually delete anything"
    );

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

// ---------------------------------------------------------------------------
// DEST-ABSENT — behaves as a plain checkout (nothing to prune, no error).
// ---------------------------------------------------------------------------

/// SPEC: when the dest does not exist yet, `--delete` is a plain checkout — the
/// tree is materialized, nothing is pruned, and there is no error.
#[test]
fn delete_with_absent_dest_is_plain_checkout() {
    let src = build_src("absent-src");
    let store = temp_dir("absent-store");
    let cache = temp_dir("absent-cache");
    let home = temp_dir("absent-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);

    // A dest path that does NOT exist (parent exists, leaf absent).
    let parent = temp_dir("absent-parent");
    let dest = parent.join("not-yet");
    assert!(!dest.exists());
    let dest_str = dest.to_string_lossy().into_owned();

    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--delete", &dest_str,
        ],
    );
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
    assert_eq!(ok_stdout(snapdir(&cache, &home), &["id", &dest_str]), id);

    cleanup(&[&src, &store, &parent, &cache, &home]);
}

// ---------------------------------------------------------------------------
// EXCLUDE — protects matching extraneous paths; non-matching siblings still pruned.
// ---------------------------------------------------------------------------

/// SPEC: `--exclude <glob>` PROTECTS an otherwise-extraneous matching path from
/// pruning, while a non-matching extraneous sibling IS still pruned.
#[test]
fn delete_exclude_protects_match_but_prunes_sibling() {
    let src = build_src("excl-src");
    let store = temp_dir("excl-store");
    let dest = temp_dir("excl-dest");
    let cache = temp_dir("excl-cache");
    let home = temp_dir("excl-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );
    // Two extraneous files: one matches the exclude (protected), one does not.
    fs::write(dest.join("keep.log"), b"protected").unwrap();
    fs::write(dest.join("drop.tmp"), b"prunable").unwrap();

    ok_stdout(
        snapdir(&cache, &home),
        &[
            "checkout",
            "--id",
            &id,
            "--delete",
            "--exclude",
            // Extended-regex (core ExcludeMatcher): match the .log path.
            "keep\\.log",
            &dest_str,
        ],
    );
    assert!(
        dest.join("keep.log").exists(),
        "--exclude must PROTECT the matching extraneous path from pruning"
    );
    assert!(
        !dest.join("drop.tmp").exists(),
        "a non-matching extraneous sibling must still be pruned"
    );

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

// ---------------------------------------------------------------------------
// SYMLINK ESCAPE — a dest symlink to OUTSIDE the dest root is unlinked, not
// followed; nothing outside the dest root is ever deleted.
// ---------------------------------------------------------------------------

/// SPEC: an extraneous symlink in the dest pointing OUTSIDE the dest root is
/// removed-not-followed: the link itself is unlinked, but its external target
/// (and the file inside it) survives — nothing outside the dest root is deleted.
#[test]
fn delete_removes_escaping_symlink_without_following_it() {
    let src = build_src("link-src");
    let store = temp_dir("link-store");
    let dest = temp_dir("link-dest");
    let cache = temp_dir("link-cache");
    let home = temp_dir("link-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );

    // An external directory OUTSIDE the dest, with a canary file inside it.
    let outside = temp_dir("link-outside");
    let canary = outside.join("CANARY.txt");
    fs::write(&canary, b"must survive").unwrap();

    // An extraneous symlink in the dest that escapes to the external dir.
    let escape = dest.join("escape");
    symlink(&outside, &escape).unwrap();
    assert!(escape.symlink_metadata().unwrap().file_type().is_symlink());

    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );

    // The link itself was unlinked...
    assert!(
        escape.symlink_metadata().is_err(),
        "the escaping symlink must be unlinked"
    );
    // ...but the external target dir and its canary are untouched (the link was
    // removed, not followed into a recursive delete of the target).
    assert!(
        canary.exists(),
        "deleting must NOT follow the symlink: the external target must survive"
    );
    assert!(outside.exists(), "the external dir itself must survive");

    cleanup(&[&src, &store, &dest, &cache, &home, &outside]);
}

// ---------------------------------------------------------------------------
// HARD-REFUSE dangerous dests — non-zero exit, nothing deleted, NO --force bypass.
// ---------------------------------------------------------------------------

/// Asserts a `checkout --delete <dest>` (optionally with `--force`) REFUSES:
/// non-zero exit, and a sentinel placed under the dest survives. Used for every
/// dangerous-dest case so the refusal is proven to fire BEFORE any deletion.
fn assert_refuses_dangerous(cache: &Path, home: &Path, dest: &Path, with_force: bool) {
    let dest_str = dest.to_string_lossy().into_owned();
    // A sentinel inside the dangerous dest: if the impl wrongly pruned, this
    // would vanish. The refusal must fire first, so it MUST survive.
    let sentinel = dest.join("DO_NOT_DELETE.sentinel");
    fs::create_dir_all(dest).ok();
    fs::write(&sentinel, b"sentinel").unwrap();

    let id = "0".repeat(64); // shape-valid; the refusal must precede any lookup.
    let mut args = vec!["checkout", "--id"];
    args.push(&id);
    args.push("--delete");
    if with_force {
        args.push("--force");
    }
    args.push(&dest_str);

    let out = snapdir(cache, home)
        .args(&args)
        .output()
        .expect("run snapdir");
    assert!(
        !out.status.success(),
        "checkout --delete on dangerous dest {} (force={with_force}) MUST refuse with non-zero exit",
        dest.display()
    );
    assert!(
        sentinel.exists(),
        "the refusal must fire BEFORE any deletion: sentinel under {} must survive (force={with_force})",
        dest.display()
    );
}

/// SPEC: `--delete` HARD-REFUSES dest = `$HOME` (non-zero exit, nothing deleted),
/// and `--force` does NOT bypass the refusal. HOME is env-redirected to a temp
/// dir so a buggy impl cannot touch the real home.
#[test]
fn delete_refuses_home_dir_even_with_force() {
    let cache = temp_dir("home-cache");
    let home = temp_dir("home-home"); // env'd HOME -> this temp dir
                                      // dest == the env'd $HOME
    assert_refuses_dangerous(&cache, &home, &home, false);
    assert_refuses_dangerous(&cache, &home, &home, true);
    cleanup(&[&cache, &home]);
}

/// SPEC: `--delete` HARD-REFUSES dest = `/` (filesystem root): non-zero exit,
/// nothing deleted, no `--force` bypass. We do NOT place a sentinel at real `/`;
/// the refusal must fire on the path identity alone, so we only assert the
/// non-zero exit (a buggy impl that DID prune `/` would also be caught by CI's
/// inability to write `/`, but the contract is the refusal itself).
#[test]
fn delete_refuses_filesystem_root_even_with_force() {
    let cache = temp_dir("root-cache");
    let home = temp_dir("root-home");
    for force in [false, true] {
        let mut args = vec!["checkout", "--id"];
        let id = "0".repeat(64);
        args.push(&id);
        args.push("--delete");
        if force {
            args.push("--force");
        }
        args.push("/");
        let out = snapdir(&cache, &home)
            .args(&args)
            .output()
            .expect("run snapdir");
        assert!(
            !out.status.success(),
            "checkout --delete / (force={force}) MUST hard-refuse with non-zero exit"
        );
    }
    cleanup(&[&cache, &home]);
}

/// SPEC: `--delete` HARD-REFUSES dest = the cache dir (`$SNAPDIR_CACHE_DIR`):
/// non-zero exit, nothing deleted, no `--force` bypass. The cache dir holds the
/// content-addressable objects — pruning it would corrupt the cache.
#[test]
fn delete_refuses_cache_dir_even_with_force() {
    let cache = temp_dir("cachedir-cache");
    let home = temp_dir("cachedir-home");
    // dest == the explicitly-configured SNAPDIR_CACHE_DIR.
    assert_refuses_dangerous(&cache, &home, &cache, false);
    assert_refuses_dangerous(&cache, &home, &cache, true);
    cleanup(&[&cache, &home]);
}

/// SPEC: `--delete` HARD-REFUSES dest = the default cache dir
/// (`$XDG_CACHE_HOME/snapdir`) too — not just the explicit `--cache-dir`. The
/// default location must be guarded identically.
#[test]
fn delete_refuses_default_cache_dir_even_with_force() {
    let home = temp_dir("defcache-home");
    // The default cache dir derived from the env'd XDG_CACHE_HOME.
    let default_cache = home.join(".cache").join("snapdir");
    fs::create_dir_all(&default_cache).unwrap();
    // Use a SEPARATE explicit cache so the dest equals the DEFAULT location, not
    // the configured one — but env the same HOME/XDG so the default resolves
    // inside the sandbox. We pass the default cache as dest while leaving
    // SNAPDIR_CACHE_DIR pointed elsewhere is not possible via snapdir(); instead
    // run with the default cache as both the resolved cache and the dest.
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("HOME", &home);
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
    cmd.env_remove("SNAPDIR_CACHE_DIR");
    cmd.env_remove("SNAPDIR_STORE");
    let sentinel = default_cache.join("DO_NOT_DELETE.sentinel");
    fs::write(&sentinel, b"sentinel").unwrap();
    let id = "0".repeat(64);
    let dest_str = default_cache.to_string_lossy().into_owned();
    let out = cmd
        .args(["checkout", "--id", &id, "--delete", "--force", &dest_str])
        .output()
        .expect("run snapdir");
    assert!(
        !out.status.success(),
        "checkout --delete on the DEFAULT cache dir must hard-refuse (no --force bypass)"
    );
    assert!(sentinel.exists(), "default cache dir must not be pruned");
    cleanup(&[&home]);
}

/// SPEC: `--delete` HARD-REFUSES dest = a store path (the local `file://` store
/// backing this run): non-zero exit, nothing deleted, no `--force` bypass. The
/// dest must never be allowed to mirror-prune the very store it reads from.
#[test]
fn delete_refuses_store_path_even_with_force() {
    let src = build_src("storepath-src");
    let store = temp_dir("storepath-store");
    let cache = temp_dir("storepath-cache");
    let home = temp_dir("storepath-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);

    // dest == the store's on-disk root. The store holds .objects/.manifests; a
    // mirror-prune of it would destroy the store. Sentinel = the landed manifest.
    let manifest_key = store.join(format!(
        ".manifests/{}/{}/{}/{}",
        &id[0..3],
        &id[3..6],
        &id[6..9],
        &id[9..]
    ));
    assert!(manifest_key.is_file(), "precondition: manifest landed");
    let store_str = store.to_string_lossy().into_owned();

    for force in [false, true] {
        let mut args = vec!["checkout", "--store", &store_url, "--id", &id, "--delete"];
        if force {
            args.push("--force");
        }
        args.push(&store_str);
        let out = snapdir(&cache, &home)
            .args(&args)
            .output()
            .expect("run snapdir");
        assert!(
            !out.status.success(),
            "checkout --delete onto the store path (force={force}) MUST hard-refuse"
        );
        assert!(
            manifest_key.is_file(),
            "the store's manifest must survive the refusal (force={force})"
        );
    }

    cleanup(&[&src, &store, &cache, &home]);
}

// ---------------------------------------------------------------------------
// CARRIED FROM materialize-modes: `--linked` against a REMOTE object source is
// a HARD ERROR at the CLI/router layer (can't symlink to a remote object).
// ---------------------------------------------------------------------------

/// SPEC (carried): `checkout --linked` against a NON-LOCAL store
/// (`s3://`/`gs://`/`b2://`/`ssh://`/`sftp://`) is a HARD ERROR at the CLI —
/// clear message, non-zero exit, no partial dest written. Pinned across every
/// remote scheme.
#[test]
fn linked_against_remote_store_is_a_hard_error() {
    let cache = temp_dir("linkremote-cache");
    let home = temp_dir("linkremote-home");
    let parent = temp_dir("linkremote-parent");

    for (i, remote) in [
        "s3://bucket/prefix",
        "gs://bucket/prefix",
        "b2://bucket/prefix",
        "ssh://host/srv/store",
        "sftp://host/srv/store",
    ]
    .iter()
    .enumerate()
    {
        let dest = parent.join(format!("dest-{i}"));
        let dest_str = dest.to_string_lossy().into_owned();
        let id = "0".repeat(64);
        let out = snapdir(&cache, &home)
            .args([
                "checkout", "--store", remote, "--id", &id, "--linked", &dest_str,
            ])
            .output()
            .expect("run snapdir");
        assert!(
            !out.status.success(),
            "checkout --linked against remote {remote} MUST be a hard error"
        );
        assert!(
            !dest.exists() || fs::read_dir(&dest).map_or(true, |mut d| d.next().is_none()),
            "no partial dest tree may be materialized for remote {remote}"
        );
    }

    cleanup(&[&cache, &home, &parent]);
}

/// SPEC (carried): `pull --linked` against a remote store is likewise a hard
/// error at the CLI (pull = fetch + checkout; the linked-from-remote refusal
/// must surface, not silently fall back to copies).
#[test]
fn pull_linked_against_remote_store_is_a_hard_error() {
    let cache = temp_dir("pulllink-cache");
    let home = temp_dir("pulllink-home");
    let parent = temp_dir("pulllink-parent");
    let dest = parent.join("dest");
    let dest_str = dest.to_string_lossy().into_owned();
    let id = "0".repeat(64);

    let out = snapdir(&cache, &home)
        .args([
            "pull",
            "--store",
            "s3://bucket/prefix",
            "--id",
            &id,
            "--linked",
            &dest_str,
        ])
        .output()
        .expect("run snapdir");
    assert!(
        !out.status.success(),
        "pull --linked against a remote store MUST be a hard error"
    );

    cleanup(&[&cache, &home, &parent]);
}

// ---------------------------------------------------------------------------
// IMPL-REVEALED (review-phase additions). The impl made `--linked` from a LOCAL
// store symlink each file entry into the shared 0444 content object, and SKIP
// `restore_permissions` in Linked mode (re-chmod'ing the link would follow into
// and corrupt the shared object). It also canonicalizes the dangerous-dest key.
// These cases pin those now-visible branches. All remain black-box assert_cmd.
// ---------------------------------------------------------------------------

/// REVIEW: local `--linked` happy path. `checkout --linked` from a LOCAL
/// `file://` store into a FRESH dir materializes each file entry as a SYMLINK
/// (not a real-inode copy), the linked file reads the correct content, and the
/// shared object the link points at is read-only `0444`. Pins the impl's
/// `MaterializeMode::Linked` wiring for a local source.
#[test]
fn linked_local_checkout_creates_symlinks_to_readonly_objects() {
    let src = build_src("linkok-src");
    let store = temp_dir("linkok-store");
    let dest = temp_dir("linkok-dest");
    let cache = temp_dir("linkok-cache");
    let home = temp_dir("linkok-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // `--linked` against the LOCAL file:// store must succeed (local objects to
    // point at) and materialize symlinks, not copies. `pull --linked` = fetch
    // (populate the cache with manifest + objects) + linked checkout in one.
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--linked", &dest_str,
        ],
    );

    let a = dest.join("a.txt");
    let a_meta = a.symlink_metadata().expect("a.txt exists");
    assert!(
        a_meta.file_type().is_symlink(),
        "--linked must materialize a.txt as a SYMLINK, not a real-inode copy"
    );
    let b = dest.join("sub").join("b.txt");
    assert!(
        b.symlink_metadata().unwrap().file_type().is_symlink(),
        "--linked must materialize the nested sub/b.txt as a symlink too"
    );

    // The linked files read the correct content (the link resolves into the
    // shared object).
    assert_eq!(fs::read(&a).unwrap(), b"hello");
    assert_eq!(fs::read(&b).unwrap(), b"world!!");

    // The shared object the link points at is read-only `0444` (hardened so a
    // write THROUGH the link cannot corrupt the shared bytes). The link target
    // is the object on disk; follow it and assert its real mode.
    let target = fs::canonicalize(&a).expect("resolve linked target");
    let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o444,
        "the shared linked object must be read-only 0444 (got {mode:o})"
    );

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

/// REVIEW: confirms the impl's `restore_permissions`-skip in Linked mode is
/// CORRECT — a linked checkout must NOT chmod the shared object up to the
/// manifest's writable (`0644`) mode. The source `a.txt` is `0644`; were the
/// CLI to run `restore_permissions` in Linked mode it would follow the link and
/// re-mode the shared `0444` object to `0644`, corrupting it for every other
/// link. We pin that the resolved target stays `0444` and is NOT the manifest's
/// `0644`.
#[test]
fn linked_checkout_does_not_rechmod_shared_object_to_manifest_mode() {
    let src = build_src("linkperm-src"); // a.txt is 0644 in the source/manifest
    let store = temp_dir("linkperm-store");
    let dest = temp_dir("linkperm-dest");
    let cache = temp_dir("linkperm-cache");
    let home = temp_dir("linkperm-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--linked", &dest_str,
        ],
    );

    let target = fs::canonicalize(dest.join("a.txt")).expect("resolve linked target");
    let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o444,
        "Linked mode must SKIP restore_permissions: the shared object stays 0444 \
         and must NOT be chmod'd to the manifest's writable 0644 (got {mode:o})"
    );
    assert_ne!(
        mode, 0o644,
        "the shared object must NOT be re-moded to the manifest's writable mode"
    );

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

/// REVIEW: `--linked --delete` interaction. A linked checkout that also prunes
/// must remove the extraneous dest entry while LEAVING the materialized symlinks
/// (which are in-manifest) intact and still readable. Pins the
/// `checkout_inner` branch where Linked materialize is followed by `prune_dest`.
#[test]
fn linked_with_delete_prunes_extraneous_but_keeps_links() {
    let src = build_src("linkdel-src");
    let store = temp_dir("linkdel-store");
    let dest = temp_dir("linkdel-dest");
    let cache = temp_dir("linkdel-cache");
    let home = temp_dir("linkdel-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    // First a linked pull (populate cache + materialize links), then drop an
    // extraneous file alongside.
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--linked", &dest_str,
        ],
    );
    fs::write(dest.join("EXTRA_LINKED.txt"), b"junk").unwrap();

    // Re-run linked WITH --delete: prune the extraneous file, keep the links.
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "checkout", "--store", &store_url, "--id", &id, "--linked", "--delete", &dest_str,
        ],
    );
    assert!(
        !dest.join("EXTRA_LINKED.txt").exists(),
        "--linked --delete must prune the extraneous file"
    );
    assert!(
        dest.join("a.txt")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "the in-manifest link must survive the prune"
    );
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

/// REVIEW (guard canonicalization alias): the dangerous-dest guard compares the
/// CANONICAL real path, so `$HOME` given with a trailing slash AND `$HOME/.`
/// (both canonicalize to `$HOME`) must STILL be hard-refused. A purely lexical
/// guard would miss these aliases. HOME is env-redirected to a tempdir, so a
/// buggy impl cannot touch the real home; a sentinel proves nothing was pruned.
#[test]
fn delete_refuses_home_canonicalization_aliases_even_with_force() {
    let cache = temp_dir("alias-cache");
    let home = temp_dir("alias-home");
    let sentinel = home.join("DO_NOT_DELETE.sentinel");
    fs::write(&sentinel, b"sentinel").unwrap();

    let home_str = home.to_string_lossy().into_owned();
    // Aliases that all canonicalize to the env'd HOME.
    for alias in [format!("{home_str}/"), format!("{home_str}/."), {
        // A symlink that resolves to HOME — a canonicalizing guard must follow
        // it; a lexical-only guard would not.
        let parent = home.parent().unwrap();
        let link = parent.join(format!("home-alias-{}", std::process::id()));
        fs::remove_file(&link).ok();
        symlink(&home, &link).unwrap();
        link.to_string_lossy().into_owned()
    }] {
        let id = "0".repeat(64);
        let out = snapdir(&cache, &home)
            .args(["checkout", "--id", &id, "--delete", "--force", &alias])
            .output()
            .expect("run snapdir");
        assert!(
            !out.status.success(),
            "checkout --delete onto a canonicalization-alias of $HOME ({alias}) \
             MUST hard-refuse (canonical guard, no --force bypass)"
        );
        assert!(
            sentinel.exists(),
            "the refusal must fire before any deletion for alias {alias}"
        );
    }

    // Clean up the symlink alias we created under home's parent.
    let parent = home.parent().unwrap();
    fs::remove_file(parent.join(format!("home-alias-{}", std::process::id()))).ok();
    cleanup(&[&cache, &home]);
}

/// REVIEW (exclude regex semantics): `--exclude` is EXTENDED-REGEX (confirmed by
/// the impl, resolving the gate's glob-vs-regex ambiguity). A regex METACHAR
/// pattern protects as a regex (a `.` matches any char; an alternation matches
/// either), and MULTIPLE `--exclude` flags ALL apply (each is its own protect
/// pattern). A non-matching extraneous sibling is still pruned.
#[test]
fn delete_exclude_regex_metachars_and_multiple_flags_all_apply() {
    let src = build_src("exclre-src");
    let store = temp_dir("exclre-store");
    let dest = temp_dir("exclre-dest");
    let cache = temp_dir("exclre-cache");
    let home = temp_dir("exclre-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();

    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );
    // Three extraneous files. `keep1.log` is protected by an alternation regex,
    // `keepX.cfg` by a metachar (`.` = any char) regex via a SECOND --exclude,
    // and `drop.bin` matches neither and must be pruned.
    fs::write(dest.join("keep1.log"), b"a").unwrap();
    fs::write(dest.join("keepX.cfg"), b"b").unwrap();
    fs::write(dest.join("drop.bin"), b"c").unwrap();

    ok_stdout(
        snapdir(&cache, &home),
        &[
            "checkout",
            "--id",
            &id,
            "--delete",
            // Extended-regex alternation: protect either *.log or *.bak.
            "--exclude",
            "(keep1\\.log|keep1\\.bak)",
            // SECOND --exclude, regex metachar `.` = any single char.
            "--exclude",
            "keep.\\.cfg",
            &dest_str,
        ],
    );
    assert!(
        dest.join("keep1.log").exists(),
        "the alternation-regex --exclude must protect keep1.log"
    );
    assert!(
        dest.join("keepX.cfg").exists(),
        "the SECOND --exclude (metachar regex) must ALSO apply and protect keepX.cfg"
    );
    assert!(
        !dest.join("drop.bin").exists(),
        "a sibling matching NEITHER exclude must still be pruned"
    );

    cleanup(&[&src, &store, &dest, &cache, &home]);
}
