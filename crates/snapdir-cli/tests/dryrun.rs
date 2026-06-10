//! Integration tests for the global `--dryrun` flag.
//!
//! `--dryrun` must make every store/FS-mutating command a no-op-writes mode:
//! zero persistent writes (no store objects/manifests, no cache writes, no
//! destination files, no cache flush, no catalog events) while still printing
//! the intended action and the pure-computation snapshot id. These tests assert
//! the hard invariant — ZERO writes — for each guarded command, and confirm a
//! successful (exit 0) run. Every test fn name contains `dryrun` so
//! `cargo test -p snapdir-cli --locked dryrun` selects them.
//!
//! Mirrors the `file://` store + cache-dir setup pattern from
//! `tests/store_roundtrip.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the compiled `snapdir` binary under test.
///
/// The bin target lives in the `snapdir` crate (`crates/snapdir`), so
/// `CARGO_BIN_EXE_snapdir` is not set for snapdir-cli tests; `assert_cmd`'s
/// lookup falls back to the shared target dir. Under `cargo test --workspace`
/// the binary is always built first; for a standalone
/// `cargo test -p snapdir-cli`, run `cargo build -p snapdir` once before.
fn snapdir_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// Creates a unique temp directory and returns its path.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-cli-dryrun-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Runs the `snapdir` binary, asserting success and returning trimmed stdout.
fn run_snapdir(args: &[&str], cache: &Path) -> String {
    let output = Command::new(snapdir_bin())
        .args(args)
        .env("SNAPDIR_CACHE_DIR", cache)
        .output()
        .expect("run snapdir");
    assert!(
        output.status.success(),
        "snapdir {args:?} exited with {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout)
        .expect("stdout is UTF-8")
        .trim_end()
        .to_owned()
}

/// Builds a small, deterministic source tree under `src`.
fn build_src_tree(src: &Path) {
    fs::write(src.join("a.txt"), b"hello").unwrap();
    fs::set_permissions(src.join("a.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::create_dir(src.join("sub")).unwrap();
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(src.join("sub").join("b.txt"), b"world!!").unwrap();
    fs::set_permissions(
        src.join("sub").join("b.txt"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::set_permissions(src, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Counts the regular files anywhere under `dir` (recursively). A
/// content-addressable store/cache materializes objects + manifests as files,
/// so a count of zero means nothing was persisted.
fn count_files(dir: &Path) -> usize {
    let mut total = 0;
    if !dir.exists() {
        return 0;
    }
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            total += count_files(&path);
        } else {
            total += 1;
        }
    }
    total
}

/// `push --dryrun` against an empty `file://` store must leave the store empty:
/// no `.objects`/`.manifests` entries are written.
#[test]
fn dryrun_push_writes_nothing() {
    let src = temp_dir("push-src");
    let store = temp_dir("push-store");
    let cache = temp_dir("push-cache");

    build_src_tree(&src);
    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();

    // The id is a pure computation and must still be printed to stdout.
    let id = run_snapdir(
        &["push", "--dryrun", "--store", &store_url, &src_str],
        &cache,
    );
    assert_eq!(id.len(), 64, "push --dryrun must still print the id");

    assert!(
        !store.join(".objects").exists(),
        "push --dryrun must not write any objects"
    );
    assert!(
        !store.join(".manifests").exists(),
        "push --dryrun must not write any manifests"
    );
    assert_eq!(
        count_files(&store),
        0,
        "store must remain empty after dryrun"
    );
    // And the cache must not be written to either.
    assert_eq!(
        count_files(&cache),
        0,
        "cache must remain empty after dryrun"
    );

    for dir in [&src, &store, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}

/// `stage --dryrun` with an empty cache must leave the cache empty.
#[test]
fn dryrun_stage_writes_nothing() {
    let src = temp_dir("stage-src");
    let cache = temp_dir("stage-cache");

    build_src_tree(&src);
    let src_str = src.to_string_lossy().into_owned();

    let id = run_snapdir(&["stage", "--dryrun", &src_str], &cache);
    assert_eq!(id.len(), 64, "stage --dryrun must still print the id");

    assert_eq!(
        count_files(&cache),
        0,
        "stage --dryrun must not write anything to the cache"
    );

    for dir in [&src, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}

/// `flush-cache --dryrun` must NOT empty a populated cache.
#[test]
fn dryrun_flush_cache_keeps_objects() {
    let src = temp_dir("flush-src");
    let cache = temp_dir("flush-cache");

    build_src_tree(&src);
    let src_str = src.to_string_lossy().into_owned();

    // Real (non-dryrun) stage to populate the cache.
    run_snapdir(&["stage", &src_str], &cache);
    let before = count_files(&cache);
    assert!(before > 0, "real stage must populate the cache");

    // Dry-run flush must be a no-op.
    run_snapdir(&["flush-cache", "--dryrun"], &cache);
    let after = count_files(&cache);
    assert_eq!(
        after, before,
        "flush-cache --dryrun must leave the cache unchanged"
    );

    for dir in [&src, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}

/// `checkout --dryrun` with a populated cache must leave the destination empty.
#[test]
fn dryrun_checkout_writes_nothing() {
    let src = temp_dir("co-src");
    let cache = temp_dir("co-cache");
    let dest = temp_dir("co-dest");

    build_src_tree(&src);
    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();

    // Real stage so the snapshot is available for checkout from the cache.
    let id = run_snapdir(&["stage", &src_str], &cache);

    // Dry-run checkout must not materialize anything at the destination.
    run_snapdir(&["checkout", "--dryrun", "--id", &id, &dest_str], &cache);
    assert_eq!(
        count_files(&dest),
        0,
        "checkout --dryrun must not write any files to the destination"
    );

    for dir in [&src, &cache, &dest] {
        fs::remove_dir_all(dir).ok();
    }
}

/// `pull --dryrun` (= fetch + checkout) must write neither the cache nor the
/// destination.
#[test]
fn dryrun_pull_writes_nothing() {
    let src = temp_dir("pull-src");
    let store = temp_dir("pull-store");
    let pushcache = temp_dir("pull-pushcache");
    let cache = temp_dir("pull-cache");
    let dest = temp_dir("pull-dest");

    build_src_tree(&src);
    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();

    // Real push to populate the store (uses a throwaway cache).
    let id = run_snapdir(&["push", "--store", &store_url, &src_str], &pushcache);

    // Dry-run pull from the store into a fresh, empty cache + dest.
    run_snapdir(
        &[
            "pull", "--dryrun", "--store", &store_url, "--id", &id, &dest_str,
        ],
        &cache,
    );
    assert_eq!(
        count_files(&cache),
        0,
        "pull --dryrun must not write to the cache"
    );
    assert_eq!(
        count_files(&dest),
        0,
        "pull --dryrun must not write to the destination"
    );

    for dir in [&src, &store, &pushcache, &cache, &dest] {
        fs::remove_dir_all(dir).ok();
    }
}
