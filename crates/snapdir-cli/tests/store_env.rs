//! Integration tests for `--store` (and `sync --from`) defaulting to
//! `$SNAPDIR_STORE` when the flag is omitted.
//!
//! Phase 20 bug fix: the global `--store` arg and the `sync --from` arg both
//! carry `env = "SNAPDIR_STORE"`, so an unset flag falls back to the env var,
//! while an explicit flag still overrides it. `sync --to` stays REQUIRED (a
//! sync needs two distinct stores) and the from/to-must-differ check is intact;
//! with neither flag nor env the existing required/"missing --store" error is
//! preserved.
//!
//! Every fn name contains `store_env` so
//! `cargo test -p snapdir-cli --locked store_env` selects exactly this suite.
//! These drive the wired binary against temp `file://` stores (removed on drop)
//! so they are hermetic and need no network or credentials. Each command
//! explicitly sets or removes `SNAPDIR_STORE` so the suite never leaks the
//! developer's environment.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;
use predicates::prelude::*;

/// A fresh `snapdir` command with the cache pinned under `cache` (so tests
/// never touch the user's real cache) and `SNAPDIR_STORE` removed by default,
/// so leakage from the developer's environment can't mask a bug. Tests that
/// exercise the env default re-add it explicitly with `.env(...)`.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env_remove("SNAPDIR_STORE");
    cmd
}

/// Like [`snapdir`] but with `SNAPDIR_STORE` set to `store`, returned by value.
fn snapdir_with_store(cache: &Path, store: &str) -> Command {
    let mut cmd = snapdir(cache);
    cmd.env("SNAPDIR_STORE", store);
    cmd
}

/// Builds a known tiny tree with explicit, deterministic permissions so a
/// checked-out copy must restore them to re-manifest to the same id.
fn build_tree(dir: &TempDir) {
    dir.child("a.txt").write_str("hello").unwrap();
    std::fs::set_permissions(dir.child("a.txt").path(), PermissionsExt::from_mode(0o644)).unwrap();
    std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o755)).unwrap();
}

/// Runs `snapdir <args>`, asserts success, returns trimmed stdout.
fn stdout_ok(mut cmd: Command, args: &[&str]) -> String {
    let out = cmd.args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// 1. With `SNAPDIR_STORE` set and `--store` OMITTED, the resolved store equals
///    the env value: a `push` with no `--store` lands the manifest + object in
///    the env-named `file://` store, and a `pull` (again no `--store`) restores
///    it byte-identically (re-manifests to the source id).
#[test]
fn store_env_default_used_when_flag_omitted() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    // Push with NO --store; the env value supplies the store.
    let id = stdout_ok(
        snapdir_with_store(cache.path(), &store_url),
        &["push", &src_str],
    );
    assert_eq!(id.len(), 64, "push must print a 64-hex snapshot id");

    // Proof the env store really received the snapshot: the manifest landed.
    let manifest = store.path().join(format!(
        ".manifests/{}/{}/{}/{}",
        &id[0..3],
        &id[3..6],
        &id[6..9],
        &id[9..]
    ));
    assert!(
        manifest.is_file(),
        "manifest must land in the SNAPDIR_STORE-named store at {}",
        manifest.display()
    );

    // Pull with NO --store; the env value supplies the store again.
    stdout_ok(
        snapdir_with_store(cache.path(), &store_url),
        &["pull", "--id", &id, &dest_str],
    );
    let dest_id = stdout_ok(snapdir(cache.path()), &["id", &dest_str]);
    assert_eq!(
        dest_id, id,
        "restore from the env store must re-manifest to the source id"
    );
}

/// 2. An explicit `--store <other>` OVERRIDES `SNAPDIR_STORE`: the snapshot
///    lands in the flag store, NOT the env store.
#[test]
fn store_env_explicit_flag_overrides_env() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let env_store = TempDir::new().unwrap();
    let flag_store = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let env_url = format!("file://{}", env_store.path().display());
    let flag_url = format!("file://{}", flag_store.path().display());

    let id = stdout_ok(
        snapdir_with_store(cache.path(), &env_url),
        &["push", "--store", &flag_url, &src_str],
    );

    let manifest_rel = format!(
        ".manifests/{}/{}/{}/{}",
        &id[0..3],
        &id[3..6],
        &id[6..9],
        &id[9..]
    );
    assert!(
        flag_store.path().join(&manifest_rel).is_file(),
        "explicit --store must receive the snapshot"
    );
    assert!(
        !env_store.path().join(&manifest_rel).exists(),
        "the SNAPDIR_STORE env store must be left untouched when --store is explicit"
    );
}

/// 3a. `sync --from` honors `SNAPDIR_STORE` (so `--from` may be omitted) while
///     `--to` stays REQUIRED: with the env set, a sync omitting `--from` but
///     supplying a distinct `--to` succeeds and mirrors the snapshot.
#[test]
fn store_env_sync_from_honors_env_to_required() {
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

    // Seed store A (the env source) via an explicit --store push.
    let id = stdout_ok(
        snapdir(cache.path()),
        &["push", "--store", &a_url, &src_str],
    );

    // Sync with NO --from; SNAPDIR_STORE supplies the source. --to is explicit.
    snapdir(cache.path())
        .env("SNAPDIR_STORE", &a_url)
        .args(["sync", "--id", &id, "--to", &b_url])
        .assert()
        .success();

    // Store B now holds the snapshot: pull it from B and confirm the id.
    stdout_ok(
        snapdir(cache.path()),
        &["pull", "--store", &b_url, "--id", &id, &dest_str],
    );
    assert_eq!(
        stdout_ok(snapdir(cache.path()), &["id", &dest_str]),
        id,
        "synced snapshot must re-materialize from store B to the source id"
    );
}

/// 3b. Omitting `--to` still errors even when `SNAPDIR_STORE` is set: `--to`
///     does NOT default to the env (a sync needs two distinct stores).
#[test]
fn store_env_sync_to_still_required() {
    let cache = TempDir::new().unwrap();
    let a = TempDir::new().unwrap();
    let a_url = format!("file://{}", a.path().display());

    snapdir(cache.path())
        .env("SNAPDIR_STORE", &a_url)
        .args(["sync", "--id", &"0".repeat(64)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--to"));
}

/// 3c. With `SNAPDIR_STORE` set and `--to` resolving to the SAME store, the
///     from/to-must-differ check still fires (env-supplied `--from` == `--to`).
#[test]
fn store_env_sync_from_to_must_differ_still_fires() {
    let cache = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let url = format!("file://{}", store.path().display());

    snapdir(cache.path())
        .env("SNAPDIR_STORE", &url)
        .args(["sync", "--id", &"0".repeat(64), "--to", &url])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--from and --to must differ"));
}

/// 4. With NEITHER `--store` flag NOR `SNAPDIR_STORE` env, the existing
///    "missing --store option" error is preserved exactly (push needs a store).
#[test]
fn store_env_missing_flag_and_env_preserves_error() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    // snapdir() already removes SNAPDIR_STORE, so neither flag nor env is set.
    snapdir(cache.path())
        .args(["push", &src_str])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing --store option"));
}

/// 4b. With neither flag nor env, `sync --to <x>` (omitting `--from`) fails on
///     the clap-required `--from` arg (no env to satisfy it).
#[test]
fn store_env_sync_missing_from_flag_and_env_errors() {
    let cache = TempDir::new().unwrap();
    let to = TempDir::new().unwrap();
    let to_url = format!("file://{}", to.path().display());

    snapdir(cache.path())
        .args(["sync", "--id", &"0".repeat(64), "--to", &to_url])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--from"));
}
