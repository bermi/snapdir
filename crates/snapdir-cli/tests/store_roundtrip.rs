//! Integration test for the `push` → `checkout` store round-trip over a temp
//! `file://` store.
//!
//! Builds a scratch source tree, pushes it to a temporary `file://` store,
//! checks it out to a fresh destination, and asserts the destination tree
//! reproduces the source: same files, same contents, and — critically — the
//! same snapshot id when re-manifested (which requires the checked-out
//! permissions to match). Also asserts the objects and manifest landed at the
//! exact sharded keys the store layout demands.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use snapdir_core::{Blake3Hasher, Hasher};

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
        "snapdir-cli-roundtrip-{tag}-{}-{:?}",
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

/// `.objects/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>` — the frozen sharded layout.
fn sharded(prefix: &str, hex: &str) -> String {
    format!(
        "{prefix}/{}/{}/{}/{}",
        &hex[0..3],
        &hex[3..6],
        &hex[6..9],
        &hex[9..]
    )
}

#[test]
fn store_roundtrip_push_then_checkout_reproduces_tree() {
    let src = temp_dir("src");
    let store = temp_dir("store");
    let dest = temp_dir("dest");
    let cache = temp_dir("cache");

    // Build a small tree with explicit, deterministic permissions so the
    // checked-out tree must restore them to re-manifest to the same id.
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
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();

    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();

    // The id the source tree manifests to (independent of the store).
    let src_id = run_snapdir(&["id", &src_str], &cache);
    assert_eq!(src_id.len(), 64, "snapshot id should be 64 hex chars");

    // Push to the store; the printed id must equal the source id.
    let pushed_id = run_snapdir(&["push", "--store", &store_url, &src_str], &cache);
    assert_eq!(pushed_id, src_id, "push must print the source snapshot id");

    // The manifest landed at its exact sharded key.
    let manifest_key = store.join(sharded(".manifests", &src_id));
    assert!(
        manifest_key.is_file(),
        "manifest must land at sharded key {}",
        manifest_key.display()
    );

    // Each file's object landed at its content-addressed sharded key, with
    // matching bytes.
    for (rel, bytes) in [("a.txt", &b"hello"[..]), ("sub/b.txt", &b"world!!"[..])] {
        let sum = Blake3Hasher::new().hash_hex(bytes);
        let obj = store.join(sharded(".objects", &sum));
        assert!(
            obj.is_file(),
            "object for {rel} must land at {}",
            obj.display()
        );
        assert_eq!(fs::read(&obj).unwrap(), bytes, "object bytes for {rel}");
    }

    // Checkout: fetch into the cache, then materialize at dest.
    run_snapdir(
        &["pull", "--store", &store_url, "--id", &src_id, &dest_str],
        &cache,
    );

    // The destination reproduces the source contents.
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
    assert_eq!(
        fs::read(dest.join("sub").join("b.txt")).unwrap(),
        b"world!!"
    );

    // And re-manifests to the SAME snapshot id (contents + permissions match).
    let dest_id = run_snapdir(&["id", &dest_str], &cache);
    assert_eq!(
        dest_id, src_id,
        "checked-out tree must re-manifest to the source snapshot id"
    );

    for dir in [&src, &store, &dest, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}

/// Regression: `snapdir push --store … --id <id>` (no PATH) must push the
/// *staged* snapshot named by `--id`. It used to ignore `--id` and fall through
/// to `resolve_root(None)`, snapshotting the current working directory instead.
#[test]
fn push_by_staged_id_pushes_the_staged_snapshot_not_cwd() {
    let src = temp_dir("src-staged");
    let store = temp_dir("store-staged");
    let dest = temp_dir("dest-staged");
    let cache = temp_dir("cache-staged");

    fs::write(src.join("a.txt"), b"hello").unwrap();
    fs::set_permissions(src.join("a.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();

    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();

    // Stage the tree: its objects + manifest land in the local cache, no store.
    let staged_id = run_snapdir(&["stage", &src_str], &cache);
    assert_eq!(staged_id.len(), 64, "staged id should be 64 hex chars");

    // Push BY ID with no PATH. The printed id must equal the staged snapshot's
    // id — proof it pushed the staged snapshot and not a snapshot of the CWD.
    let pushed_id = run_snapdir(&["push", "--store", &store_url, "--id", &staged_id], &cache);
    assert_eq!(
        pushed_id, staged_id,
        "push --id must push the staged snapshot, not the working directory"
    );

    // The staged snapshot's manifest + object really landed in the store.
    assert!(
        store.join(sharded(".manifests", &staged_id)).is_file(),
        "manifest must land in the store under its sharded key"
    );
    let obj = store.join(sharded(".objects", &Blake3Hasher::new().hash_hex(b"hello")));
    assert!(obj.is_file(), "the file object must land in the store");

    // And it pulls back to the same id from a fresh restore.
    run_snapdir(
        &["pull", "--store", &store_url, "--id", &staged_id, &dest_str],
        &cache,
    );
    assert_eq!(
        run_snapdir(&["id", &dest_str], &cache),
        staged_id,
        "restore from the store must re-manifest to the staged id"
    );

    for dir in [&src, &store, &dest, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}

/// Regression: `push --id <unknown>` must fail with an actionable error instead
/// of silently snapshotting the current working directory.
#[test]
fn push_by_unknown_id_errors_without_walking_cwd() {
    let store = temp_dir("store-unknown");
    let cache = temp_dir("cache-unknown");
    let store_url = format!("file://{}", store.display());
    let unknown = "0".repeat(64); // valid shape, never staged

    let output = Command::new(snapdir_bin())
        .args(["push", "--store", &store_url, "--id", &unknown])
        .env("SNAPDIR_CACHE_DIR", &cache)
        .output()
        .expect("run snapdir");
    assert!(
        !output.status.success(),
        "push --id with an unknown id must fail, not push a snapshot of the CWD"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found in the local cache"),
        "error should explain the id was never staged/fetched; got: {stderr}"
    );

    for dir in [&store, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}

#[test]
fn store_roundtrip_fetch_then_checkout_separately() {
    let src = temp_dir("src2");
    let store = temp_dir("store2");
    let dest = temp_dir("dest2");
    let cache = temp_dir("cache2");

    fs::write(src.join("only.txt"), b"solo").unwrap();
    fs::set_permissions(src.join("only.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();

    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();

    let id = run_snapdir(&["push", "--store", &store_url, &src_str], &cache);

    // fetch populates the cache; checkout works offline from the cache only.
    run_snapdir(&["fetch", "--store", &store_url, "--id", &id], &cache);
    let cache_manifest = cache.join(sharded(".manifests", &id));
    assert!(cache_manifest.is_file(), "fetch must cache the manifest");

    run_snapdir(&["checkout", "--id", &id, &dest_str], &cache);
    assert_eq!(fs::read(dest.join("only.txt")).unwrap(), b"solo");
    assert_eq!(run_snapdir(&["id", &dest_str], &cache), id);

    for dir in [&src, &store, &dest, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}
