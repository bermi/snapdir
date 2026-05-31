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
fn snapdir_bin() -> &'static str {
    env!("CARGO_BIN_EXE_snapdir")
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
