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

/// The transfer-tuning flags (`--jobs` / `--limit-rate`) are accepted on a real
/// `push` then `pull` round-trip to a `file://` store and do not change the
/// outcome: the pushed id equals the source id and the pulled tree re-manifests
/// to it. This exercises the full flag → `TransferConfig` → store threading.
#[test]
fn transfer_flags_push_pull_roundtrip() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["id", &src_str]);

    // push with concurrency + bandwidth caps set explicitly.
    let pushed = stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store_url,
            "--jobs",
            "2",
            "--limit-rate",
            "1M",
            &src_str,
        ],
    );
    assert_eq!(pushed, src_id, "push with transfer flags must print the id");

    // pull with the short `-j` alias and a sequential cap.
    snapdir(cache.path())
        .args([
            "pull",
            "--store",
            &store_url,
            "--id",
            &src_id,
            "-j",
            "1",
            "--limit-rate",
            "512K",
            &dest_str,
        ])
        .assert()
        .success();

    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    assert_eq!(
        stdout_ok(cache.path(), &["id", &dest_str]),
        src_id,
        "tree pulled with transfer flags must re-manifest to the source id"
    );
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

// ---------------------------------------------------------------------------
// pull-push-correctness-suite: binary-level regression tests for the four
// correctness properties the operator was worried about. These build on top of
// the existing round-trip tests above; each is hermetic (temp `file://` store +
// temp cache, both removed on drop).
// ---------------------------------------------------------------------------

/// Recursively counts the regular files anywhere under `dir` (0 if absent). A
/// content-addressable store/cache/destination materializes its state as files,
/// so a count of zero means nothing was written.
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

/// Scenario 1 — **push → pull → pull idempotency.** Pulling the same id into
/// the SAME destination twice must be a stable no-op: both pulls exit 0, and
/// after each the destination re-manifests to the source id (contents +
/// permissions intact). This complements `fetch_cached_skips_store_objects`
/// (which proves the 2nd pull needs no store objects); here we focus on the
/// positive idempotency / destination stability across repeated pulls.
#[test]
fn push_pull_pull_is_idempotent() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["push", "--store", &store_url, &src_str]);

    // Pull #1 into the destination.
    snapdir(cache.path())
        .args(["pull", "--store", &store_url, "--id", &src_id, &dest_str])
        .assert()
        .success();
    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    assert_eq!(
        stdout_ok(cache.path(), &["id", &dest_str]),
        src_id,
        "first pull must reproduce the source id"
    );

    // Pull #2 into the SAME destination — must be a stable, idempotent no-op.
    snapdir(cache.path())
        .args(["pull", "--store", &store_url, "--id", &src_id, &dest_str])
        .assert()
        .success();
    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    assert_eq!(
        stdout_ok(cache.path(), &["id", &dest_str]),
        src_id,
        "repeated pull must leave the destination re-manifesting to the same id"
    );
}

/// Scenario 2 — **--dryrun makes no writes (e2e level).** A `push --dryrun`
/// against an empty `file://` store must leave the store empty (no `.objects`,
/// no `.manifests`), and a `pull --dryrun` into a fresh destination must leave
/// that destination empty. Intentionally overlaps `tests/dryrun.rs` so THIS
/// gate's verification — which runs only e2e + `store_roundtrip` — exercises the
/// dry-run invariant.
#[test]
fn dryrun_push_leaves_store_empty_e2e() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    // push --dryrun: still prints the (pure-computation) id, writes nothing.
    let id = stdout_ok(
        cache.path(),
        &["push", "--dryrun", "--store", &store_url, &src_str],
    );
    assert_eq!(id.len(), 64, "push --dryrun must still print the id");
    assert!(
        !store.path().join(".objects").exists(),
        "push --dryrun must not create any store objects"
    );
    assert!(
        !store.path().join(".manifests").exists(),
        "push --dryrun must not create any store manifests"
    );
    assert_eq!(
        count_files(store.path()),
        0,
        "store must remain empty after push --dryrun"
    );

    // A real push so a dryrun pull has something to (not) materialize.
    let realstore = TempDir::new().unwrap();
    let real_url = format!("file://{}", realstore.path().display());
    let pushcache = TempDir::new().unwrap();
    let real_id = stdout_ok(pushcache.path(), &["push", "--store", &real_url, &src_str]);

    // pull --dryrun into a fresh dest + fresh cache must leave both empty.
    let dest = TempDir::new().unwrap();
    let pullcache = TempDir::new().unwrap();
    let dest_str = dest.path().to_string_lossy().into_owned();
    snapdir(pullcache.path())
        .args([
            "pull", "--dryrun", "--store", &real_url, "--id", &real_id, &dest_str,
        ])
        .assert()
        .success();
    assert_eq!(
        count_files(dest.path()),
        0,
        "pull --dryrun must not materialize any destination files"
    );
    assert_eq!(
        count_files(pullcache.path()),
        0,
        "pull --dryrun must not write to the cache"
    );
}

/// Scenario 3 — **corrupted local file is detected and repaired on pull.**
/// After a push + pull populates the cache and the destination, overwrite one
/// destination file with wrong bytes. A second pull into the SAME destination
/// must repair it: the destination re-manifests to the source id and the
/// corrupted file's contents are restored. The 2nd pull's fetch leg is a
/// cache-hit no-op (manifest already cached); the checkout leg notices the
/// corrupt file's local checksum no longer matches and rewrites it from the
/// cache — so the repair works offline. We prove offline by amputating the
/// store's `.objects` before the repairing pull.
#[test]
fn pull_repairs_corrupted_dest_file() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["push", "--store", &store_url, &src_str]);

    // Pull #1 populates the cache and the destination.
    snapdir(cache.path())
        .args(["pull", "--store", &store_url, "--id", &src_id, &dest_str])
        .assert()
        .success();
    dest.child("a.txt").assert("hello");
    assert_eq!(stdout_ok(cache.path(), &["id", &dest_str]), src_id);

    // Corrupt a destination file in place (wrong bytes, wrong length).
    dest.child("a.txt")
        .write_str("CORRUPTED-WRONG-BYTES")
        .unwrap();
    assert_ne!(
        stdout_ok(cache.path(), &["id", &dest_str]),
        src_id,
        "corrupting the file must change the re-manifested id"
    );

    // Amputate the store's objects to prove the repair is served from the
    // cache (offline) and not by re-reading the store.
    let objects = store.path().join(".objects");
    assert!(objects.exists(), "store must have an .objects subtree");
    std::fs::remove_dir_all(&objects).expect("remove store .objects subtree");

    // Pull #2 into the SAME destination must repair the corrupted file.
    snapdir(cache.path())
        .args(["pull", "--store", &store_url, "--id", &src_id, &dest_str])
        .assert()
        .success();
    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    assert_eq!(
        stdout_ok(cache.path(), &["id", &dest_str]),
        src_id,
        "repairing pull must restore the destination to the source id"
    );
}

// ---------------------------------------------------------------------------
// transfer-concurrency-verification (phase 13): end-to-end proof that
// `--jobs` / `--limit-rate` are wired through the binary and that concurrency
// does not change the materialized result. All hermetic via a temp `file://`
// store (the aggregate `RateLimiter` is network-only; `FileStore` does NOT throttle
// local copies, so the deterministic throttle proof lives in transfer-config's
// `RateLimiter` timing unit test, not here). These tests prove flag acceptance,
// threading, and byte-identical correctness through the concurrent `FileStore`
// path. Fn names start with `transfer_concurrency` so
// `cargo test -p snapdir-cli --locked transfer_concurrency` selects them.
// ---------------------------------------------------------------------------

/// Builds a *multi-file* tree (several files + nested dirs) with deterministic
/// permissions so a concurrent push/pull has real fan-out to exercise, and a
/// checked-out copy must restore contents + perms to re-manifest to the same id.
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
    // Pin a couple of directory modes so directory perms are part of the id too.
    for d in ["dir_a", "dir_a/nested", "dir_b", "dir_b/sub", "dir_b/sub/c"] {
        std::fs::set_permissions(dir.child(d).path(), PermissionsExt::from_mode(0o755)).unwrap();
    }
    std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o755)).unwrap();
}

/// `push --jobs 4` a multi-file tree to a `file://` store, then `pull --jobs 4`
/// into a fresh dest: exit 0, the pushed id equals the source id, and the pulled
/// tree re-manifests to the same id (byte-identical materialization through the
/// concurrent `FileStore` path).
#[test]
fn transfer_concurrency_jobs4_roundtrip() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_multi_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["id", &src_str]);

    let pushed = stdout_ok(
        cache.path(),
        &["push", "--store", &store_url, "--jobs", "4", &src_str],
    );
    assert_eq!(pushed, src_id, "push --jobs 4 must print the source id");

    snapdir(cache.path())
        .args([
            "pull", "--store", &store_url, "--id", &src_id, "--jobs", "4", &dest_str,
        ])
        .assert()
        .success();

    dest.child("dir_a/nested/deep.txt").assert("echo!!");
    dest.child("dir_b/sub/c/leaf.dat").assert("hotel-hotel");
    assert_eq!(
        stdout_ok(cache.path(), &["id", &dest_str]),
        src_id,
        "tree pulled with --jobs 4 must re-manifest to the source id"
    );
}

/// Same round-trip with `--jobs 1` (the sequential path): identical id/result.
/// Also asserts the `--jobs 1` and `--jobs 4` runs produce the SAME snapshot id,
/// proving concurrency does not change the output.
#[test]
fn transfer_concurrency_jobs1_roundtrip() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_multi_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["id", &src_str]);

    let pushed = stdout_ok(
        cache.path(),
        &["push", "--store", &store_url, "--jobs", "1", &src_str],
    );
    assert_eq!(pushed, src_id, "push --jobs 1 must print the source id");

    snapdir(cache.path())
        .args([
            "pull", "--store", &store_url, "--id", &src_id, "--jobs", "1", &dest_str,
        ])
        .assert()
        .success();

    dest.child("dir_a/a2.txt").assert("delta-delta-delta");
    dest.child("dir_b/b1.txt").assert("foxtrot");
    let dest_id = stdout_ok(cache.path(), &["id", &dest_str]);
    assert_eq!(
        dest_id, src_id,
        "tree pulled with --jobs 1 must re-manifest to the source id"
    );

    // Push the same tree to a SECOND store with --jobs 4; the printed id must
    // match the --jobs 1 push — concurrency must not change the snapshot id.
    let parallel_store = TempDir::new().unwrap();
    let parallel_url = format!("file://{}", parallel_store.path().display());
    let pushed4 = stdout_ok(
        cache.path(),
        &["push", "--store", &parallel_url, "--jobs", "4", &src_str],
    );
    assert_eq!(
        pushed4, pushed,
        "--jobs 4 and --jobs 1 pushes must yield the same snapshot id"
    );
}

/// `push --jobs 2 --limit-rate 1M` + `pull --limit-rate 512K` round-trip to a
/// `file://` store succeeds and re-manifests to the same id. This proves the
/// flag parses, threads into `TransferConfig`, and does not break correctness.
/// There is NO timing assertion: `FileStore` does not throttle local copies (the
/// `RateLimiter` is network-only; its deterministic timing proof lives in
/// transfer-config's `RateLimiter` unit test).
#[test]
fn transfer_concurrency_limit_rate_accepted() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_multi_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["id", &src_str]);

    let pushed = stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store_url,
            "--jobs",
            "2",
            "--limit-rate",
            "1M",
            &src_str,
        ],
    );
    assert_eq!(
        pushed, src_id,
        "push --jobs 2 --limit-rate 1M must print the source id"
    );

    snapdir(cache.path())
        .args([
            "pull",
            "--store",
            &store_url,
            "--id",
            &src_id,
            "--limit-rate",
            "512K",
            &dest_str,
        ])
        .assert()
        .success();

    dest.child("top1.txt").assert("alpha");
    dest.child("dir_b/sub/c/leaf.dat").assert("hotel-hotel");
    assert_eq!(
        stdout_ok(cache.path(), &["id", &dest_str]),
        src_id,
        "tree pulled with --limit-rate must re-manifest to the source id"
    );
}

/// Scenario 4 — **multi/comma --exclude drops paths from the manifest.** Build
/// a tree with excludable `node_modules/x` and `coverage/y`. The comma form
/// (`--exclude node_modules,coverage`) and the repeated form (`--exclude
/// node_modules --exclude coverage`) must both omit those paths from the
/// manifest stdout AND produce identical output, while a plain `manifest` (no
/// exclude) includes them.
///
/// (The exclude regex matches against the *absolute* scan path, so the chosen
/// exclude tokens must not appear in the temp-dir prefix — e.g. `tmp` would
/// also match `/tmp/...` and nuke the whole walk. `node_modules`/`coverage` are
/// safe distinctive names.)
#[test]
fn manifest_multi_exclude_drops_paths_e2e() {
    let src = TempDir::new().unwrap();
    build_tree(&src);
    // Add excludable subtrees on top of the known tree.
    src.child("node_modules/x").write_str("dep").unwrap();
    src.child("coverage/y").write_str("scratch").unwrap();
    let src_str = src.path().to_string_lossy().into_owned();

    // The cache dir is irrelevant for `manifest` (pure computation) but the
    // harness pins it; reuse a temp cache so we never touch the real $HOME.
    let cache = TempDir::new().unwrap();

    // Plain manifest includes both excludable subtrees.
    let plain = stdout_ok(cache.path(), &["manifest", &src_str]);
    assert!(
        plain.contains("node_modules"),
        "plain manifest should include node_modules:\n{plain}"
    );
    assert!(
        plain.contains("coverage"),
        "plain manifest should include coverage:\n{plain}"
    );

    // Comma form and repeated form.
    let comma = stdout_ok(
        cache.path(),
        &["manifest", "--exclude", "node_modules,coverage", &src_str],
    );
    let repeated = stdout_ok(
        cache.path(),
        &[
            "manifest",
            "--exclude",
            "node_modules",
            "--exclude",
            "coverage",
            &src_str,
        ],
    );

    for (label, out) in [("comma", &comma), ("repeated", &repeated)] {
        assert!(
            !out.contains("node_modules"),
            "{label} --exclude must drop node_modules:\n{out}"
        );
        assert!(
            !out.contains("coverage"),
            "{label} --exclude must drop coverage:\n{out}"
        );
        // The non-excluded known tree files must survive.
        assert!(
            out.contains("./a.txt") && out.contains("./sub/b.txt"),
            "{label} --exclude must keep the non-excluded files:\n{out}"
        );
    }

    assert_eq!(
        comma, repeated,
        "comma and repeated --exclude forms must produce identical manifests"
    );
}

/// `--verbose push --jobs N` prints the effective transfer concurrency to
/// stderr ONCE, while stdout stays exactly the snapshot id (byte-stable).
#[test]
fn verbose_jobs_push_reports_concurrency() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["id", &src_str]);

    let out = snapdir(cache.path())
        .args([
            "push",
            "--jobs",
            "3",
            "--verbose",
            "--store",
            &store_url,
            &src_str,
        ])
        .output()
        .expect("run snapdir");
    assert!(out.status.success(), "verbose push must succeed");

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        stdout.trim_end(),
        src_id,
        "stdout must remain exactly the snapshot id"
    );

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("transfers: 3 concurrent"),
        "stderr must report effective concurrency:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("transfers:").count(),
        1,
        "the transfer-config banner must print exactly once:\n{stderr}"
    );
}

/// `--verbose push --jobs N --limit-rate R` reports both the concurrency and the
/// rate limit on stderr.
#[test]
fn verbose_jobs_limit_rate_reported() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let out = snapdir(cache.path())
        .args([
            "push",
            "--jobs",
            "2",
            "--limit-rate",
            "1M",
            "--verbose",
            "--store",
            &store_url,
            &src_str,
        ])
        .output()
        .expect("run snapdir");
    assert!(out.status.success(), "verbose limited push must succeed");

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("2 concurrent") && stderr.contains("limit 1M"),
        "stderr must report concurrency AND the limit rate:\n{stderr}"
    );
}

/// Without `--verbose`, the transfer-config banner is silent and stdout is still
/// exactly the snapshot id.
#[test]
fn verbose_jobs_silent_without_verbose() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["id", &src_str]);

    let out = snapdir(cache.path())
        .args(["push", "--jobs", "3", "--store", &store_url, &src_str])
        .output()
        .expect("run snapdir");
    assert!(out.status.success(), "non-verbose push must succeed");

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim_end(), src_id, "stdout must be the snapshot id");

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        !stderr.contains("concurrent") && !stderr.contains("transfers:"),
        "non-verbose run must not emit the transfer-config banner:\n{stderr}"
    );
}

/// `pull --jobs N --verbose` (fetch + checkout) emits the transfer-config banner
/// exactly ONCE, not once per leg.
#[test]
fn verbose_jobs_pull_reports_once() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);

    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());

    let src_id = stdout_ok(cache.path(), &["push", "--store", &store_url, &src_str]);

    let out = snapdir(cache.path())
        .args([
            "pull",
            "--jobs",
            "4",
            "--verbose",
            "--store",
            &store_url,
            "--id",
            &src_id,
            &dest_str,
        ])
        .output()
        .expect("run snapdir");
    assert!(out.status.success(), "verbose pull must succeed");

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("transfers: 4 concurrent"),
        "pull --verbose must report concurrency:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("transfers:").count(),
        1,
        "pull must print the transfer-config banner exactly once:\n{stderr}"
    );
}
