//! Integration tests for the cache subcommands `stage`, `verify-cache`, and
//! `flush-cache`, using `assert_cmd` + `assert_fs`.
//!
//! These exercise the *wired* cache commands against a real temp tree and a
//! temp cache directory (the cache is itself a `file://`-shaped content-
//! addressable store with the frozen `.objects`/`.manifests` sharded layout):
//!
//! - `stage <tree>` prints the same 64-hex snapshot id as `id <tree>` and
//!   populates the cache at the exact sharded `.manifests`/`.objects` keys.
//! - `verify-cache` on a freshly-staged cache exits 0.
//! - tampering a cached object makes `verify-cache` exit non-zero and report it;
//!   `--purge` removes the corrupt object and reports it.
//! - `flush-cache` empties the cache (objects + manifests gone) and is
//!   idempotent on an already-empty cache.
//!
//! The cache lives under an `assert_fs` temp dir removed on drop, so the tests
//! are hermetic and never touch the user's real `$HOME/.cache/snapdir`.

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

/// Builds a known tiny tree with explicit, deterministic permissions.
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

/// `<prefix>/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>` — the frozen sharded layout.
fn sharded(prefix: &str, hex: &str) -> String {
    format!(
        "{prefix}/{}/{}/{}/{}",
        &hex[0..3],
        &hex[3..6],
        &hex[6..9],
        &hex[9..]
    )
}

/// The BLAKE3 content address of `bytes` (objects are filed under this).
fn blake3_hex(bytes: &[u8]) -> String {
    use snapdir_core::{Blake3Hasher, Hasher};
    Blake3Hasher::new().hash_hex(bytes)
}

#[test]
fn cache_commands_stage_prints_id_and_populates_cache_at_sharded_keys() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    // `stage` prints the same snapshot id as `id` (store-independent).
    let staged = stdout_ok(cache.path(), &["stage", &src_str]);
    let id = stdout_ok(cache.path(), &["id", &src_str]);
    assert_eq!(
        staged.len(),
        64,
        "staged id must be 64 hex chars: {staged:?}"
    );
    assert!(
        staged
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "staged id must be lowercase hex: {staged:?}"
    );
    assert_eq!(staged, id, "stage must print the snapshot id, like `id`");

    // The manifest landed at its exact sharded `.manifests` key.
    let manifest_key = cache.path().join(sharded(".manifests", &staged));
    assert!(
        manifest_key.is_file(),
        "manifest must land at sharded key {}",
        manifest_key.display()
    );

    // Each file's object landed at its content-addressed sharded `.objects`
    // key with matching bytes — exactly what `verify-cache` then checks.
    for (rel, bytes) in [("a.txt", &b"hello"[..]), ("sub/b.txt", &b"world!!"[..])] {
        let sum = blake3_hex(bytes);
        let obj = cache.path().join(sharded(".objects", &sum));
        assert!(
            obj.is_file(),
            "object for {rel} must land at {}",
            obj.display()
        );
        assert_eq!(
            std::fs::read(&obj).unwrap(),
            bytes,
            "object bytes for {rel}"
        );
    }
}

#[test]
fn cache_commands_verify_cache_passes_on_freshly_staged_cache() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    stdout_ok(cache.path(), &["stage", &src_str]);

    // A clean, freshly-staged cache verifies (round-trip: stage -> verify-cache).
    snapdir(cache.path()).arg("verify-cache").assert().success();
}

#[test]
fn cache_commands_verify_cache_detects_tampered_object_and_purge_removes_it() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    stdout_ok(cache.path(), &["stage", &src_str]);

    // Tamper one cached object in place (its path/address is unchanged, but its
    // bytes no longer hash to that address).
    let sum = blake3_hex(b"hello");
    let obj = cache.path().join(sharded(".objects", &sum));
    assert!(obj.is_file(), "object to tamper must exist");
    std::fs::write(&obj, b"TAMPERED").unwrap();

    // verify-cache now exits non-zero and reports the corrupt object's address.
    snapdir(cache.path())
        .arg("verify-cache")
        .assert()
        .failure()
        .stderr(predicate::str::contains(&sum));

    // The corrupt object is still on disk (no purge yet).
    assert!(obj.exists(), "corrupt object must survive without --purge");

    // --purge removes the corrupt object and reports it.
    snapdir(cache.path())
        .args(["verify-cache", "--purge"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(&sum));
    assert!(!obj.exists(), "corrupt object must be purged by --purge");

    // No CORRUPT object remains (the only one was purged). But the staged
    // manifest still references that object's address, so the cache is now
    // INCOMPLETE: a re-scan reports the purged object as MISSING and exits
    // non-zero (manifest-aware presence check — stricter than the oracle's
    // byte-only scan, which is blind to a referenced-but-absent object). The
    // missing report no longer carries the "Checksum mismatch" corrupt wording.
    snapdir(cache.path())
        .arg("verify-cache")
        .assert()
        .failure()
        .stderr(predicate::str::contains(&sum))
        .stderr(predicate::str::contains("Missing object"));

    // `flush-cache` clears the dangling manifest, after which a clean cache (no
    // objects, no manifests) verifies green again.
    snapdir(cache.path()).arg("flush-cache").assert().success();
    snapdir(cache.path()).arg("verify-cache").assert().success();
}

#[test]
fn cache_commands_flush_cache_empties_cache_and_is_idempotent() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    stdout_ok(cache.path(), &["stage", &src_str]);
    assert!(
        cache.path().join(".objects").exists(),
        "staged objects exist"
    );
    assert!(
        cache.path().join(".manifests").exists(),
        "staged manifests exist"
    );

    // flush-cache empties the cache (objects + manifests gone).
    snapdir(cache.path()).arg("flush-cache").assert().success();
    assert!(
        !cache.path().join(".objects").exists(),
        "objects must be gone after flush"
    );
    assert!(
        !cache.path().join(".manifests").exists(),
        "manifests must be gone after flush"
    );

    // Idempotent: flushing an already-empty cache is a clean no-op.
    snapdir(cache.path()).arg("flush-cache").assert().success();
}
