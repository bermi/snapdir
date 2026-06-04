//! Stateful end-to-end CLI tests using `assert_cmd` + `assert_fs`.
//!
//! Unlike the static `trycmd` surface snapshots, these drive the *wired*
//! commands against real temp trees and a temp `file://` store, asserting real
//! behavior:
//!
//! - `manifest` / `id` over a known tiny tree: the id is 64 lowercase hex. (The
//!   frozen byte-format contract is pinned separately by
//!   `crates/snapdir-core/tests/compat_golden.rs` against recorded constants.)
//! - a `push -> fetch -> checkout` and `push -> pull` round-trip over a temp
//!   `file://` store: the printed id equals the source id, the checked-out tree
//!   re-manifests to the same id (contents + permissions reproduced), and
//!   `verify` accepts the intact snapshot.
//!
//! The store/cache live under `assert_fs` temp dirs that are removed on drop, so
//! these tests are hermetic and need no network or credentials.

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

#[test]
fn id_is_64_lowercase_hex() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    let id = stdout_ok(cache.path(), &["id", &src_str]);
    assert_eq!(id.len(), 64, "snapshot id must be 64 hex chars: {id:?}");
    assert!(
        id.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "snapshot id must be lowercase hex: {id:?}"
    );
}

#[test]
fn push_fetch_checkout_roundtrip_reproduces_id() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    // id is store-independent; capture it for later equality checks.
    let src_id = stdout_ok(cache.path(), &["id", &src_str]);

    // push prints the source id.
    let pushed = stdout_ok(cache.path(), &["push", "--store", &store_url, &src_str]);
    assert_eq!(pushed, src_id, "push must print the source snapshot id");

    // fetch populates the cache (offline checkout works from the cache only).
    snapdir(cache.path())
        .args(["fetch", "--store", &store_url, "--id", &src_id])
        .assert()
        .success();

    // checkout materializes the tree (no --store needed; reads the cache).
    snapdir(cache.path())
        .args(["checkout", "--id", &src_id, &dest_str])
        .assert()
        .success();

    // The destination reproduces the source contents...
    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    // ...and re-manifests to the SAME id (contents + permissions restored).
    assert_eq!(
        stdout_ok(cache.path(), &["id", &dest_str]),
        src_id,
        "checked-out tree must re-manifest to the source id"
    );

    // verify accepts the intact snapshot in the store.
    snapdir(cache.path())
        .args(["verify", "--store", &store_url, "--id", &src_id])
        .assert()
        .success();
}

/// `verify --purge` must be rejected: the global `--purge` flag is inert on
/// `verify` (a store-based integrity check that never touches the cache), so
/// rather than silently ignore it the command bails with an actionable message
/// pointing at `verify-cache --purge`. The rejection fires before any store
/// resolution, so a bogus store/id still surfaces the purge error.
#[test]
fn verify_purge_is_rejected() {
    let cache = TempDir::new().unwrap();
    let zeros = "0".repeat(64);

    snapdir(cache.path())
        .args([
            "verify",
            "--store",
            "file:///tmp/nonexistent-snapdir-verify-purge",
            "--id",
            &zeros,
            "--purge",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("verify").and(predicate::str::contains("--purge")));
}

/// Sanity: plain `verify` (no `--purge`) does NOT hit the purge rejection. It
/// still fails here (the manifest is missing from the bogus store), but the
/// failure must not be the purge message.
#[test]
fn verify_without_purge_does_not_hit_purge_error() {
    let cache = TempDir::new().unwrap();
    let zeros = "0".repeat(64);

    snapdir(cache.path())
        .args([
            "verify",
            "--store",
            "file:///tmp/nonexistent-snapdir-verify-purge",
            "--id",
            &zeros,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not support --purge").not());
}

#[test]
fn pull_is_fetch_plus_checkout() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["push", "--store", &store_url, &src_str]);

    // pull == fetch + checkout in one step.
    snapdir(cache.path())
        .args(["pull", "--store", &store_url, "--id", &src_id, &dest_str])
        .assert()
        .success();

    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    assert_eq!(stdout_ok(cache.path(), &["id", &dest_str]), src_id);
}

/// A repeat `pull`/`fetch` of an already-cached id must perform ZERO store
/// object reads: the cache holds the manifest, and the manifest-written-last
/// invariant means it holds every referenced object too. We prove "no store
/// reads" the only honest way — by *deleting the store's `.objects` subtree*
/// (keeping `.manifests`) after the first pull, then pulling the SAME id again.
/// If the fetch leg still materialized objects from the store, the second pull
/// would fail with `object not found`; instead it must succeed, and its
/// destination must re-manifest to the same id (correctness, not a silent skip).
#[test]
fn fetch_cached_skips_store_objects() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let dest2 = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let redest_str = dest2.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    // push, then pull #1 — populates BOTH the cache and the first destination.
    let src_id = stdout_ok(cache.path(), &["push", "--store", &store_url, &src_str]);
    snapdir(cache.path())
        .args(["pull", "--store", &store_url, "--id", &src_id, &dest_str])
        .assert()
        .success();
    assert_eq!(stdout_ok(cache.path(), &["id", &dest_str]), src_id);

    // Amputate the store's objects (keep the manifest). Any store object read
    // now fails — so a fetch that hits the store cannot succeed.
    let objects = store.path().join(".objects");
    assert!(objects.exists(), "store must have an .objects subtree");
    std::fs::remove_dir_all(&objects).expect("remove store .objects subtree");
    assert!(store.path().join(".manifests").exists(), "manifest kept");

    // pull #2 of the SAME id into a fresh destination must SUCCEED purely from
    // the cache (zero store object reads), and re-manifest to the same id.
    snapdir(cache.path())
        .args(["pull", "--store", &store_url, "--id", &src_id, &redest_str])
        .assert()
        .success();
    dest2.child("a.txt").assert("hello");
    dest2.child("sub/b.txt").assert("world!!");
    assert_eq!(
        stdout_ok(cache.path(), &["id", &redest_str]),
        src_id,
        "cache-served pull must reproduce the source id"
    );

    // A bare `fetch` of the cached id is likewise a no-op success.
    snapdir(cache.path())
        .args(["fetch", "--store", &store_url, "--id", &src_id])
        .assert()
        .success();
}

#[test]
fn fetch_without_store_fails_with_clear_message() {
    let cache = TempDir::new().unwrap();
    snapdir(cache.path())
        .args(["fetch", "--id", &"0".repeat(64)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing --store option"));
}

#[test]
fn checkout_unknown_id_fails() {
    let cache = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let dest_str = dest.path().to_string_lossy().into_owned();
    // Nothing fetched into this cache, so the manifest is absent.
    snapdir(cache.path())
        .args(["checkout", "--id", &"0".repeat(64), &dest_str])
        .assert()
        .failure();
}
