//! Deep end-to-end correctness tests for `snapdir sync` (the 15th subcommand),
//! focused on the NO-LOCAL-STAGING property: a `sync` copies a snapshot's
//! manifest + every referenced object directly store→store, streaming through
//! memory — it must NEVER write through the local cache or any scratch/staging
//! area. These complement the lighter `sync_cmd*` suite in
//! `tests/sync_command.rs`.
//!
//! Every fn name contains `sync_e2e` so `cargo test -p snapdir-cli --locked
//! sync_e2e` selects exactly this suite. All stores/caches/dirs live under
//! `assert_fs` temp dirs removed on drop, so the suite is hermetic and needs no
//! network or credentials.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` command with the cache pinned under `cache` so tests never
/// touch the user's real `$HOME/.cache/snapdir`. We pass `--cache-dir`
/// explicitly *and* the env var so the no-staging assertion is unambiguous.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd
}

/// Builds a *multi-file* tree with several files and nested directories, all
/// with deterministic permissions so a checked-out copy must restore contents +
/// perms to re-manifest to the same id. Returns the number of distinct file
/// bodies (a lower bound on the object count the snapshot references).
fn build_multi_tree(dir: &TempDir) {
    let files = [
        ("top1.txt", "alpha", 0o644),
        ("top2.bin", "bravo-bravo", 0o600),
        ("dir_a/a1.txt", "charlie", 0o644),
        ("dir_a/a2.txt", "delta-delta-delta", 0o640),
        ("dir_a/nested/deep.txt", "echo!!", 0o644),
        ("dir_b/b1.txt", "foxtrot", 0o600),
        ("dir_b/b2.txt", "golf", 0o644),
        ("dir_b/sub/c/leaf.dat", "hotel-hotel", 0o644),
    ];
    for (rel, body, mode) in files {
        dir.child(rel).write_str(body).unwrap();
        std::fs::set_permissions(dir.child(rel).path(), PermissionsExt::from_mode(mode)).unwrap();
    }
    for d in ["dir_a", "dir_a/nested", "dir_b", "dir_b/sub", "dir_b/sub/c"] {
        std::fs::set_permissions(dir.child(d).path(), PermissionsExt::from_mode(0o755)).unwrap();
    }
    std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o755)).unwrap();
}

/// Runs `snapdir <args>` (cache pinned), asserts success, returns trimmed stdout.
fn stdout_ok(cache: &Path, args: &[&str]) -> String {
    let out = snapdir(cache).args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// Recursively collects every path (files AND directories) under `dir` as a
/// set, relative to `dir`. Returns an empty set if `dir` does not exist. Used to
/// snapshot a tree before/after an operation and diff exactly what was created.
fn path_set(dir: &Path) -> BTreeSet<PathBuf> {
    fn walk(base: &Path, cur: &Path, out: &mut BTreeSet<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(cur) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            out.insert(path.strip_prefix(base).unwrap().to_path_buf());
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

/// Counts the regular files anywhere under `dir` (0 if absent). A
/// content-addressable store/cache materializes its state as files, so a count
/// of zero means nothing was written.
fn count_files(dir: &Path) -> usize {
    let mut total = 0;
    if !dir.exists() {
        return 0;
    }
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            total += count_files(&path);
        } else {
            total += 1;
        }
    }
    total
}

/// `sync` mirrors a snapshot store→store: push a multi-file tree into store A,
/// sync it to store B (exit 0), then confirm B serves the snapshot — `pull` from
/// B re-materializes the tree byte-identically and `id` over the pulled tree
/// equals the original source id. Also asserts B physically holds a `.manifests`
/// subtree and the same number of `.objects` files as store A.
#[test]
fn sync_e2e_mirror_roundtrips() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_multi_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    // Push into A to obtain the snapshot id; B is still empty.
    let src_id = stdout_ok(cache.path(), &["push", "--store", &a_url, &src_str]);
    assert_eq!(
        count_files(store_b.path()),
        0,
        "store B must start empty before the sync"
    );

    // sync A -> B: id on stdout, human summary on stderr.
    let out = snapdir(cache.path())
        .args(["sync", "--id", &src_id, "--from", &a_url, "--to", &b_url])
        .output()
        .expect("run snapdir sync");
    assert!(
        out.status.success(),
        "sync A->B must succeed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim_end(),
        src_id,
        "sync must print the snapshot id to stdout"
    );

    // B now physically holds the manifest + every object A holds.
    assert!(
        store_b.path().join(".manifests").exists(),
        "store B must hold the snapshot manifest after sync"
    );
    assert_eq!(
        count_files(&store_a.path().join(".objects")),
        count_files(&store_b.path().join(".objects")),
        "store B must hold the same object count as store A after a full mirror"
    );

    // B serves the snapshot: pull from B (fresh dest + cache) and confirm a
    // byte-identical re-materialization that re-manifests to the source id.
    let pullcache = TempDir::new().unwrap();
    snapdir(pullcache.path())
        .args(["pull", "--store", &b_url, "--id", &src_id, &dest_str])
        .assert()
        .success();
    dest.child("top1.txt").assert("alpha");
    dest.child("dir_a/nested/deep.txt").assert("echo!!");
    dest.child("dir_b/sub/c/leaf.dat").assert("hotel-hotel");
    assert_eq!(
        stdout_ok(pullcache.path(), &["id", &dest_str]),
        src_id,
        "tree pulled from the sync target must re-manifest to the source id"
    );
}

/// Running the SAME sync twice is content-addressed: the 2nd run copies nothing
/// (`0 copied` on stderr) and leaves store B physically unchanged.
#[test]
fn sync_e2e_incremental_second_sync_copies_nothing() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    build_multi_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    let src_id = stdout_ok(cache.path(), &["push", "--store", &a_url, &src_str]);

    // First sync populates B.
    snapdir(cache.path())
        .args(["sync", "--id", &src_id, "--from", &a_url, "--to", &b_url])
        .assert()
        .success();
    let after_first = path_set(store_b.path());
    assert!(
        !after_first.is_empty(),
        "the first sync must populate store B"
    );

    // Second sync of the same id: 0 copied, B unchanged.
    let out2 = snapdir(cache.path())
        .args(["sync", "--id", &src_id, "--from", &a_url, "--to", &b_url])
        .output()
        .expect("run snapdir sync #2");
    assert!(out2.status.success(), "second sync must succeed");
    let stderr2 = String::from_utf8(out2.stderr).unwrap();
    assert!(
        stderr2.contains("0 copied"),
        "the second sync must report 0 copied (skip-present):\n{stderr2}"
    );
    assert_eq!(
        path_set(store_b.path()),
        after_first,
        "the second (no-op) sync must leave store B physically unchanged"
    );
}

/// `sync --dryrun` against an empty store B is read-only: exit 0, stderr mentions
/// "would copy", and afterward B has NO objects and NO manifest (nothing written
/// anywhere under B).
#[test]
fn sync_e2e_dryrun_leaves_dest_untouched() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    build_multi_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    let src_id = stdout_ok(cache.path(), &["push", "--store", &a_url, &src_str]);

    let out = snapdir(cache.path())
        .args([
            "sync", "--dryrun", "--id", &src_id, "--from", &a_url, "--to", &b_url,
        ])
        .output()
        .expect("run snapdir sync --dryrun");
    assert!(out.status.success(), "dry-run sync must succeed");
    assert!(
        String::from_utf8(out.stdout).unwrap().trim().is_empty(),
        "dry-run sync must not print the id to stdout"
    );
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("would copy"),
        "dry-run sync must report a would-copy summary to stderr"
    );

    assert!(
        !store_b.path().join(".manifests").exists(),
        "dry-run sync must not write a manifest to the destination"
    );
    assert!(
        !store_b.path().join(".objects").exists(),
        "dry-run sync must not write objects to the destination"
    );
    assert_eq!(
        count_files(store_b.path()),
        0,
        "dry-run sync must leave the destination store completely empty"
    );
}

/// THE KEY PROPERTY: `sync` does NO local staging. With a FRESH, EMPTY cache dir
/// dedicated to the sync, a full mirror A->B must (1) leave that cache dir still
/// empty (no `.objects`/`.manifests` created — sync streams store→store, never
/// through the local cache), and (2) create files ONLY inside store B — nothing
/// anywhere else under the test root (source tree, store A, the sync cache). We
/// snapshot the path set of each guarded dir before and after the sync and
/// assert the only growth is inside store B's `.objects`/`.manifests`.
#[test]
fn sync_e2e_no_local_staging() {
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    build_multi_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    // Push into A with its OWN cache (so the push's caching is not conflated with
    // the sync's). The sync below gets a separate, pristine cache.
    let pushcache = TempDir::new().unwrap();
    let src_id = stdout_ok(pushcache.path(), &["push", "--store", &a_url, &src_str]);

    // A FRESH, EMPTY cache exclusively for the sync.
    let synccache = TempDir::new().unwrap();
    assert_eq!(
        count_files(synccache.path()),
        0,
        "the sync cache must start empty"
    );

    // Snapshot the guarded dirs before the sync.
    let src_before = path_set(src.path());
    let src_store_before = path_set(store_a.path());
    let dest_store_before = path_set(store_b.path());

    // Pass --cache-dir explicitly *and* the env var (snapdir() sets the env var)
    // so there is no ambiguity about which cache the sync would use if it tried
    // to stage locally.
    let out = snapdir(synccache.path())
        .args([
            "sync",
            "--cache-dir",
            &synccache.path().to_string_lossy(),
            "--id",
            &src_id,
            "--from",
            &a_url,
            "--to",
            &b_url,
        ])
        .output()
        .expect("run snapdir sync");
    assert!(
        out.status.success(),
        "sync must succeed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // (1) The dedicated sync cache must still be EMPTY — sync did not stage
    // objects or manifests through the local cache.
    assert_eq!(
        count_files(synccache.path()),
        0,
        "sync must NOT stage anything through the local cache (cache dir must stay empty)"
    );
    assert!(
        !synccache.path().join(".objects").exists(),
        "sync must not create a cache .objects subtree"
    );
    assert!(
        !synccache.path().join(".manifests").exists(),
        "sync must not create a cache .manifests subtree"
    );

    // (2) Nothing was created outside store B: the source tree and store A are
    // byte-for-byte unchanged in their path sets.
    assert_eq!(
        path_set(src.path()),
        src_before,
        "sync must not create anything under the source tree"
    );
    assert_eq!(
        path_set(store_a.path()),
        src_store_before,
        "sync must not create anything under the source store A"
    );

    // The only growth is inside store B, and every newly created path is under
    // B's `.objects` or `.manifests` (the store's own layout), never a staging
    // scratch dir.
    let dest_store_after = path_set(store_b.path());
    let new_in_b: BTreeSet<_> = dest_store_after
        .difference(&dest_store_before)
        .cloned()
        .collect();
    assert!(
        !new_in_b.is_empty(),
        "the sync must have written the snapshot into store B"
    );
    for p in &new_in_b {
        assert!(
            p.starts_with(".objects") || p.starts_with(".manifests"),
            "every path created in store B must be under .objects/.manifests, got {p:?}"
        );
    }
}

/// With store B pre-populated with SOME of A's objects (by syncing a snapshot
/// that shares objects), a sync of a larger overlapping snapshot copies ONLY the
/// missing objects and skips the present ones — `objects_copied` is strictly
/// less than the full object count, and `objects_skipped` is non-zero.
#[test]
fn sync_e2e_partial_overlap_only_copies_missing() {
    let cache = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();

    // Small tree: a strict subset of the big tree's file bodies.
    let small = TempDir::new().unwrap();
    small.child("top1.txt").write_str("alpha").unwrap();
    small.child("dir_a/a1.txt").write_str("charlie").unwrap();
    std::fs::set_permissions(
        small.child("top1.txt").path(),
        PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    std::fs::set_permissions(
        small.child("dir_a/a1.txt").path(),
        PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    std::fs::set_permissions(
        small.child("dir_a").path(),
        PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::fs::set_permissions(small.path(), PermissionsExt::from_mode(0o755)).unwrap();

    // Big tree shares top1.txt ("alpha") and dir_a/a1.txt ("charlie") bodies.
    let big = TempDir::new().unwrap();
    build_multi_tree(&big);

    let small_str = small.path().to_string_lossy().into_owned();
    let big_str = big.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    // Push BOTH snapshots into A.
    let small_id = stdout_ok(cache.path(), &["push", "--store", &a_url, &small_str]);
    let big_id = stdout_ok(cache.path(), &["push", "--store", &a_url, &big_str]);

    // Pre-populate B by syncing the small snapshot first (shared objects land).
    snapdir(cache.path())
        .args(["sync", "--id", &small_id, "--from", &a_url, "--to", &b_url])
        .assert()
        .success();
    let b_objects_after_small = count_files(&store_b.path().join(".objects"));
    assert!(
        b_objects_after_small > 0,
        "the small sync must seed some shared objects into B"
    );

    // Now sync the big snapshot: it must skip the already-present shared objects
    // and copy only the missing ones.
    let out = snapdir(cache.path())
        .args(["sync", "--id", &big_id, "--from", &a_url, "--to", &b_url])
        .output()
        .expect("run big sync");
    assert!(out.status.success(), "big sync must succeed");
    let stderr = String::from_utf8(out.stderr).unwrap();

    // Parse "synced <id>: N copied, M skipped (B bytes)".
    let summary = stderr
        .lines()
        .find(|l| l.contains("synced") && l.contains("copied"))
        .unwrap_or_else(|| panic!("expected a synced summary line:\n{stderr}"));
    let copied = parse_count(summary, "copied");
    let skipped = parse_count(summary, "skipped");

    let total_objects = count_files(&store_a.path().join(".objects"));
    assert!(
        skipped > 0,
        "the overlapping objects must be skipped (>0 skipped):\n{summary}"
    );
    assert!(
        copied < total_objects,
        "a partial-overlap sync must copy fewer than all {total_objects} objects, copied={copied}:\n{summary}"
    );

    // And B must now serve the big snapshot too: pull + re-manifest to big_id.
    let dest = TempDir::new().unwrap();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let pullcache = TempDir::new().unwrap();
    snapdir(pullcache.path())
        .args(["pull", "--store", &b_url, "--id", &big_id, &dest_str])
        .assert()
        .success();
    assert_eq!(
        stdout_ok(pullcache.path(), &["id", &dest_str]),
        big_id,
        "after the partial sync, B must fully serve the big snapshot"
    );
}

/// Parses the integer immediately preceding `word` in a "N copied, M skipped"
/// style summary line (e.g. `parse_count("3 copied, 2 skipped", "copied")`
/// returns 3).
fn parse_count(line: &str, word: &str) -> usize {
    let idx = line
        .find(word)
        .unwrap_or_else(|| panic!("word {word:?} not found in {line:?}"));
    line[..idx]
        .split_whitespace()
        .next_back()
        .and_then(|tok| tok.parse().ok())
        .unwrap_or_else(|| panic!("no count before {word:?} in {line:?}"))
}
