//! Integration tests for `snapdir manifest` / `snapdir id` against the frozen
//! Bash oracle.
//!
//! Each test builds a known tiny scratch tree in a temp directory, runs the
//! compiled `snapdir` binary, and asserts its stdout is **byte-identical** to
//! the live `./snapdir-manifest` (the raw frozen-format oracle) and
//! `./snapdir id`. A diff versus the oracle is a real wiring failure.
//!
//! The oracle's `./snapdir manifest` wrapper injects `--cache` and a default
//! `--exclude=system`, so for byte-identity we pin to `./snapdir-manifest`
//! directly (the gate's interop note). For the snapshot id we compare to
//! `./snapdir id`, which is the b3sum of the comment-stripped manifest text.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Absolute path to the repository root (the crate is at `<root>/crates/snapdir-cli`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives two levels under the repo root")
        .to_path_buf()
}

/// Path to a frozen oracle script at the repo root, or `None` when it is not
/// present (e.g. a crate-only checkout); such tests no-op rather than fail.
fn oracle(name: &str) -> Option<PathBuf> {
    let path = repo_root().join(name);
    path.is_file().then_some(path)
}

/// Path to the compiled `snapdir` binary under test.
fn snapdir_bin() -> &'static str {
    env!("CARGO_BIN_EXE_snapdir")
}

/// Creates a unique temp directory for a test tree and returns its path.
fn temp_tree(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let unique = format!(
        "snapdir-cli-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    dir.push(unique);
    fs::create_dir_all(&dir).expect("create temp tree");
    dir
}

/// Builds the standard scratch tree used by most tests:
///
/// ```text
/// <root>/a.txt        ("hello")
/// <root>/empty        ("")
/// <root>/sub/b.txt    ("world!!")
/// ```
fn build_basic_tree(root: &Path) {
    fs::write(root.join("a.txt"), b"hello").unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub").join("b.txt"), b"world!!").unwrap();
}

/// Runs a command and returns its stdout as a `String`, asserting success.
fn run_stdout(program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    assert!(
        output.status.success(),
        "{program} {args:?} exited with {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// Runs the oracle with the given args (path-first, matching the oracle's
/// flag-after-path parsing quirk for `--no-follow`).
fn run_oracle(script: &Path, args: &[&str]) -> String {
    run_stdout(&script.to_string_lossy(), args)
}

#[test]
fn manifest_matches_oracle_default_b3() {
    let Some(script) = oracle("snapdir-manifest") else {
        eprintln!("skip: ./snapdir-manifest not present");
        return;
    };
    let root = temp_tree("b3");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = run_oracle(&script, &[&root_str]);
    let actual = run_stdout(snapdir_bin(), &["manifest", &root_str]);

    assert_eq!(actual, expected, "manifest stdout differs from oracle");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_matches_oracle_absolute() {
    let Some(script) = oracle("snapdir-manifest") else {
        eprintln!("skip: ./snapdir-manifest not present");
        return;
    };
    let root = temp_tree("abs");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = run_oracle(&script, &["--absolute", &root_str]);
    let actual = run_stdout(snapdir_bin(), &["manifest", "--absolute", &root_str]);

    assert_eq!(
        actual, expected,
        "--absolute manifest stdout differs from oracle"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_matches_oracle_md5() {
    let Some(script) = oracle("snapdir-manifest") else {
        eprintln!("skip: ./snapdir-manifest not present");
        return;
    };
    let root = temp_tree("md5");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = run_oracle(&script, &["--checksum-bin", "md5sum", &root_str]);
    let actual = run_stdout(
        snapdir_bin(),
        &["manifest", "--checksum-bin", "md5sum", &root_str],
    );

    assert_eq!(
        actual, expected,
        "--checksum-bin md5sum manifest stdout differs from oracle"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_matches_oracle_sha256() {
    let Some(script) = oracle("snapdir-manifest") else {
        eprintln!("skip: ./snapdir-manifest not present");
        return;
    };
    let root = temp_tree("sha256");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = run_oracle(&script, &["--checksum-bin", "sha256sum", &root_str]);
    let actual = run_stdout(
        snapdir_bin(),
        &["manifest", "--checksum-bin", "sha256sum", &root_str],
    );

    assert_eq!(
        actual, expected,
        "--checksum-bin sha256sum manifest stdout differs from oracle"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_matches_oracle_exclude() {
    let Some(script) = oracle("snapdir-manifest") else {
        eprintln!("skip: ./snapdir-manifest not present");
        return;
    };
    let root = temp_tree("exclude");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = run_oracle(&script, &["--exclude", "sub", &root_str]);
    let actual = run_stdout(snapdir_bin(), &["manifest", "--exclude", "sub", &root_str]);

    assert_eq!(
        actual, expected,
        "--exclude manifest stdout differs from oracle"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_matches_oracle_keyed_context() {
    let Some(script) = oracle("snapdir-manifest") else {
        eprintln!("skip: ./snapdir-manifest not present");
        return;
    };
    let root = temp_tree("keyed");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = {
        let out = Command::new(&script)
            .arg(&root_str)
            .env("SNAPDIR_MANIFEST_CONTEXT", "sekret")
            .output()
            .expect("run oracle");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let actual = {
        let out = Command::new(snapdir_bin())
            .args(["manifest", &root_str])
            .env("SNAPDIR_MANIFEST_CONTEXT", "sekret")
            .output()
            .expect("run snapdir");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };

    assert_eq!(
        actual, expected,
        "keyed (SNAPDIR_MANIFEST_CONTEXT) manifest stdout differs from oracle"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_id_matches_oracle() {
    let Some(script) = oracle("snapdir") else {
        eprintln!("skip: ./snapdir not present");
        return;
    };
    let root = temp_tree("id");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = run_oracle(&script, &["id", &root_str]);
    let actual = run_stdout(snapdir_bin(), &["id", &root_str]);

    assert_eq!(actual, expected, "snapshot id differs from oracle");
    // Sanity: the id is a 64-hex-char b3sum plus a trailing newline.
    assert_eq!(actual.trim_end().len(), 64, "id should be 64 hex chars");
    fs::remove_dir_all(&root).ok();
}
