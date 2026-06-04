//! End-to-end tests for `snapdir sync --id <id> --from <store> --to <store>`,
//! the 15th subcommand: a direct store→store copy of a snapshot (manifest +
//! every referenced object), streaming through memory with no local staging.
//!
//! These drive the wired binary against temp `file://` stores (removed on drop)
//! so they are hermetic and need no network or credentials. Every fn name
//! contains `sync_cmd` so `cargo test -p snapdir-cli --locked sync_cmd` selects
//! exactly this suite.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;
use predicates::prelude::*;

/// A fresh `snapdir` command with the cache pinned under `cache` so tests never
/// touch the user's real `$HOME/.cache/snapdir`.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd
}

/// Builds a known tiny tree with explicit, deterministic permissions so a
/// checked-out copy must restore them to re-manifest to the same id.
fn build_tree(dir: &TempDir) {
    dir.child("a.txt").write_str("hello").unwrap();
    std::fs::set_permissions(dir.child("a.txt").path(), PermissionsExt::from_mode(0o644)).unwrap();
    dir.child("sub/b.txt").write_str("world!!").unwrap();
    std::fs::set_permissions(
        dir.child("sub/b.txt").path(),
        PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    std::fs::set_permissions(dir.child("sub").path(), PermissionsExt::from_mode(0o755)).unwrap();
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

/// An external/unknown protocol source URL must be rejected: `sync` requires
/// in-process stores, so a `snapdir-*-store` (external) `--from` fails with an
/// actionable error naming the unsupported scheme.
#[test]
fn sync_cmd_rejects_external() {
    let cache = TempDir::new().unwrap();
    let to = TempDir::new().unwrap();
    let to_url = format!("file://{}", to.path().display());

    snapdir(cache.path())
        .args([
            "sync",
            "--id",
            &"0".repeat(64),
            "--from",
            "rsync://example/x",
            "--to",
            &to_url,
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("sync requires in-process stores")
                .and(predicate::str::contains("not supported")),
        );
}

/// `--from` and `--to` resolving to the same store must be rejected before any
/// transfer is attempted.
#[test]
fn sync_cmd_rejects_same_from_to() {
    let cache = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let url = format!("file://{}", store.path().display());

    snapdir(cache.path())
        .args([
            "sync",
            "--id",
            &"0".repeat(64),
            "--from",
            &url,
            "--to",
            &url,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--from and --to must differ"));
}

/// `sync` without `--id` must fail with the clear missing-option message (the id
/// comes from the global `--id`, not a sync-local arg).
#[test]
fn sync_cmd_requires_id() {
    let cache = TempDir::new().unwrap();
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    let a_url = format!("file://{}", a.path().display());
    let b_url = format!("file://{}", b.path().display());

    snapdir(cache.path())
        .args(["sync", "--from", &a_url, "--to", &b_url])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing --id option"));
}

/// e2e: push a tree into store A, `sync` it to store B, then `pull` from B and
/// confirm byte-identical re-materialization (re-manifests to the source id). A
/// second `sync` of the same id reports 0 copied (content-addressed skip).
#[test]
fn sync_cmd_mirrors_between_file_stores() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    // Push into store A to obtain the snapshot id.
    let src_id = stdout_ok(cache.path(), &["push", "--store", &a_url, &src_str]);

    // sync A -> B: prints the id to stdout and a human summary to stderr.
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
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains(&format!("synced {src_id}")) && stderr.contains("copied"),
        "sync must print a human summary to stderr:\n{stderr}"
    );

    // Store B now serves the snapshot: pull from B into a fresh dest + cache and
    // confirm byte-identical re-materialization.
    let pullcache = TempDir::new().unwrap();
    snapdir(pullcache.path())
        .args(["pull", "--store", &b_url, "--id", &src_id, &dest_str])
        .assert()
        .success();
    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    assert_eq!(
        stdout_ok(pullcache.path(), &["id", &dest_str]),
        src_id,
        "tree pulled from the sync target must re-manifest to the source id"
    );

    // A second sync of the same id is a content-addressed no-op: 0 copied.
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
}

/// `sync --dryrun` performs no writes: the destination store stays empty and the
/// summary goes to stderr (no id on stdout for a dry run).
#[test]
fn sync_cmd_dryrun_writes_nothing() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    build_tree(&src);

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
            .contains("dry-run: would copy"),
        "dry-run sync must report a would-copy summary to stderr"
    );

    // The destination store must remain empty (no manifest, no objects).
    assert!(
        !store_b.path().join(".manifests").exists() && !store_b.path().join(".objects").exists(),
        "dry-run sync must not write to the destination store"
    );
}
