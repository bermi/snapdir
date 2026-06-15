//! Black-box repro for gate `dx-id-stdin-verify` (phase 30).
//!
//! `snapdir id --help` promises: "Print the manifest ID of a directory or a
//! manifest piped via stdin" and "[PATH] … omit to read a manifest from stdin".
//! The CURRENT binary does NOT honor that: when invoked with no PATH it routes
//! through `resolve_root(None)` which falls back to `current_dir()`, so it walks
//! the CWD and IGNORES stdin entirely. Consequences:
//!
//!   * `snapdir id` reading "from stdin" is non-deterministic for a FIXED stdin
//!     input — its result is the id of whatever the CWD happens to be.
//!   * `snapdir manifest <dir> | snapdir id` never round-trips to
//!     `snapdir id <dir>` (it hashes the CWD, not the piped manifest).
//!
//! The snapshot-id spec (`snapdir-core::merkle::snapshot_id`) is
//! `manifest | grep -v '^#' | b3sum --no-names` over the manifest text plus the
//! `echo` trailing newline. So the INVARIANT the stdin path must satisfy is:
//!
//!   id(stdin = `snapdir manifest <dir>`)  ==  `snapdir id <dir>`
//!
//! and it must depend ONLY on the stdin bytes (not on the CWD).
//!
//! These tests are authored to FAIL against the current binary and PASS once the
//! `id` command's stdin-read path is implemented (lane: cli stdin-read). Do not
//! weaken them to pass.
//!
//! Conventions mirror `tests/dx_args.rs`: drive the built binary with
//! `assert_cmd`, pin the cache under a tempdir, build a hermetic fixture tree.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` with the cache pinned so tests never touch the real cache.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd
}

/// Builds a known tiny tree with explicit perms so `id`/`manifest` reproduce.
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

/// Run `snapdir id <args>` from `cwd`, feeding `stdin_bytes` on stdin; returns
/// trimmed stdout. Asserts exit success.
fn id_with_stdin(cache: &Path, cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> String {
    let mut child = snapdir(cache)
        .arg("id")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn snapdir id");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_bytes)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "`snapdir id` (stdin) must succeed; stderr would be on null"
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Compute the deterministic `snapdir manifest <tree>` text and `snapdir id
/// <tree>` for the fixture, returning `(manifest_bytes, id_dir)`.
fn manifest_and_id(cache: &Path, tree: &Path) -> (Vec<u8>, String) {
    let man = snapdir(cache)
        .arg("manifest")
        .arg(tree)
        .output()
        .unwrap();
    assert!(man.status.success(), "manifest <tree> must succeed");
    let id_dir = snapdir(cache)
        .arg("id")
        .arg(tree)
        .output()
        .unwrap();
    assert!(id_dir.status.success(), "id <tree> must succeed");
    let id = String::from_utf8(id_dir.stdout).unwrap().trim().to_owned();
    (man.stdout, id)
}

/// INVARIANT 1 — fixed-input determinism: feeding the SAME manifest bytes on
/// stdin must yield the SAME id regardless of the process CWD. The current
/// binary fails this because it walks the CWD and ignores stdin.
#[test]
fn id_from_stdin_depends_only_on_stdin_not_cwd() {
    let cache = TempDir::new().unwrap();
    let tree = TempDir::new().unwrap();
    build_tree(&tree);

    // Two unrelated, NON-EMPTY working directories with different contents.
    let cwd_a = TempDir::new().unwrap();
    cwd_a.child("alpha.txt").write_str("A").unwrap();
    let cwd_b = TempDir::new().unwrap();
    cwd_b.child("beta.txt").write_str("BBBB").unwrap();

    let (manifest_bytes, _id_dir) = manifest_and_id(cache.path(), tree.path());

    // Distinct cold caches per run to rule out any cache leakage.
    let c1 = TempDir::new().unwrap();
    let c2 = TempDir::new().unwrap();
    let from_a = id_with_stdin(c1.path(), cwd_a.path(), &[], &manifest_bytes);
    let from_b = id_with_stdin(c2.path(), cwd_b.path(), &[], &manifest_bytes);

    assert_eq!(
        from_a, from_b,
        "`snapdir id` on FIXED stdin must be CWD-independent, \
         but got {from_a} (cwd_a) vs {from_b} (cwd_b) — stdin is being ignored"
    );
}

/// INVARIANT 2 — round-trip: `snapdir manifest <tree> | snapdir id` must equal
/// `snapdir id <tree>` (the snapshot-id spec hashes the #-stripped manifest
/// text). The current binary fails this because it hashes the CWD.
#[test]
fn manifest_piped_to_id_round_trips_to_id_dir() {
    let cache = TempDir::new().unwrap();
    let tree = TempDir::new().unwrap();
    build_tree(&tree);

    // A CWD that is deliberately NOT the fixture tree, to expose the cwd-walk bug.
    let other_cwd = TempDir::new().unwrap();
    other_cwd.child("noise.txt").write_str("noise").unwrap();

    let (manifest_bytes, id_dir) = manifest_and_id(cache.path(), tree.path());

    let c = TempDir::new().unwrap();
    let piped = id_with_stdin(c.path(), other_cwd.path(), &[], &manifest_bytes);

    assert_eq!(
        piped, id_dir,
        "`manifest <tree> | id` ({piped}) must round-trip to `id <tree>` ({id_dir})"
    );
}
