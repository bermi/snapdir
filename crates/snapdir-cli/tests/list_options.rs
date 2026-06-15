//! Integration tests for the multi-value list CLI options (`--exclude`).
//!
//! Gate `cli-list-options-multi` (phase 12): `--exclude` accepts BOTH repeated
//! occurrences (`--exclude a --exclude b`) AND comma-delimited values
//! (`--exclude a,b`), OR-combined (a path is dropped if it matches ANY
//! pattern). These drive the compiled `snapdir manifest` binary over a known
//! scratch tree and assert which `PATH` lines survive in stdout.
//!
//! Every test fn is named with `list_options` so
//! `cargo test -p snapdir-cli --locked list_options` selects exactly this file.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;

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

/// Creates a unique temp directory for a test tree and returns its path.
fn temp_tree(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let unique = format!(
        "snapdir-cli-list-{tag}-{}-{:?}",
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

/// Builds a scratch tree with several distinctly-named files/dirs so excludes
/// can be checked independently:
///
/// ```text
/// <root>/alpha.txt
/// <root>/beta.txt
/// <root>/gamma.txt
/// <root>/keep.txt
/// ```
fn build_tree(root: &Path) {
    fs::write(root.join("alpha.txt"), b"a").unwrap();
    fs::write(root.join("beta.txt"), b"b").unwrap();
    fs::write(root.join("gamma.txt"), b"g").unwrap();
    fs::write(root.join("keep.txt"), b"k").unwrap();
}

/// Runs `snapdir manifest <args> <root>` and returns stdout as a String.
fn manifest_stdout(root: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(snapdir_bin());
    cmd.arg("manifest");
    cmd.args(args);
    cmd.arg(root.to_string_lossy().into_owned());
    let out = cmd.output().expect("run snapdir manifest");
    assert!(
        out.status.success(),
        "snapdir manifest failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

/// Asserts that `stdout` mentions exactly the expected file basenames among
/// `alpha beta gamma keep` (each as a `./<name>` PATH).
fn assert_present(stdout: &str, present: &[&str], absent: &[&str]) {
    for name in present {
        assert!(
            stdout.contains(&format!("./{name}")),
            "expected {name} present in:\n{stdout}"
        );
    }
    for name in absent {
        assert!(
            !stdout.contains(&format!("./{name}")),
            "expected {name} absent in:\n{stdout}"
        );
    }
}

#[test]
fn list_options_single_exclude_unchanged() {
    // A single `--exclude` must behave exactly as before: drop only alpha.
    let root = temp_tree("single");
    build_tree(&root);
    let out = manifest_stdout(&root, &["--exclude", "alpha"]);
    assert_present(&out, &["beta.txt", "gamma.txt", "keep.txt"], &["alpha.txt"]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn list_options_repeated_exclude() {
    // Repeated `--exclude a --exclude b` OR-combines: drop alpha AND beta.
    let root = temp_tree("repeated");
    build_tree(&root);
    let out = manifest_stdout(&root, &["--exclude", "alpha", "--exclude", "beta"]);
    assert_present(&out, &["gamma.txt", "keep.txt"], &["alpha.txt", "beta.txt"]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn list_options_comma_delimited() {
    // A comma list `--exclude a,b` drops the same set as the repeated form.
    let root = temp_tree("comma");
    build_tree(&root);
    let out = manifest_stdout(&root, &["--exclude", "alpha,beta"]);
    assert_present(&out, &["gamma.txt", "keep.txt"], &["alpha.txt", "beta.txt"]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn list_options_mixed_comma_and_repeated() {
    // Mixed `--exclude a,b --exclude c` OR-combines all three.
    let root = temp_tree("mixed");
    build_tree(&root);
    let out = manifest_stdout(&root, &["--exclude", "alpha,beta", "--exclude", "gamma"]);
    assert_present(&out, &["keep.txt"], &["alpha.txt", "beta.txt", "gamma.txt"]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn list_options_repeated_and_comma_match_same_set() {
    // The comma form and the repeated form must drop the identical path set.
    let root_a = temp_tree("eq-a");
    let root_b = temp_tree("eq-b");
    build_tree(&root_a);
    build_tree(&root_b);
    let comma = manifest_stdout(&root_a, &["--exclude", "alpha,gamma"]);
    let repeated = manifest_stdout(&root_b, &["--exclude", "alpha", "--exclude", "gamma"]);
    // Compare the surviving basenames (the absolute root differs between trees,
    // but the `./`-relative PATHs are identical for the same exclude set).
    assert_present(
        &comma,
        &["beta.txt", "keep.txt"],
        &["alpha.txt", "gamma.txt"],
    );
    assert_present(
        &repeated,
        &["beta.txt", "keep.txt"],
        &["alpha.txt", "gamma.txt"],
    );
    fs::remove_dir_all(&root_a).ok();
    fs::remove_dir_all(&root_b).ok();
}

#[test]
fn list_options_macro_combined_with_literal() {
    // A `%common%` macro combined with a literal pattern proves per-pattern
    // macro expansion: the macro expands to the common dir set (which includes
    // `node_modules`) and still OR-combines with the literal `alpha`.
    // `%common%` expands to `(/(…|node_modules|…)($|/))`, so a `node_modules/`
    // subdir is dropped; `alpha.txt` is dropped by the literal; everything else
    // survives. If the raw patterns were `|`-joined before expansion the macro
    // token would be corrupted and `node_modules` would NOT be excluded.
    let root = temp_tree("macro");
    build_tree(&root);
    fs::create_dir(root.join("node_modules")).unwrap();
    fs::write(root.join("node_modules").join("pkg.txt"), b"p").unwrap();

    let out = manifest_stdout(&root, &["--exclude", "%common%", "--exclude", "alpha"]);
    // alpha dropped by the literal; the node_modules subtree dropped by %common%.
    assert_present(&out, &["beta.txt", "gamma.txt", "keep.txt"], &["alpha.txt"]);
    assert!(
        !out.contains("node_modules"),
        "node_modules excluded by %common% in:\n{out}"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn list_options_global_exclude_before_subcommand_is_rejected() {
    // Approach-B (phase 30): `--exclude` is no longer a global flag, so placing
    // it BEFORE the subcommand is a hard CLI error. Only the subcommand-scoped
    // `--exclude` (placed after `manifest`) is accepted; that path still drops
    // its targets. (Replaces the old global-vs-subcommand precedence test, which
    // covered a surface that no longer exists.)
    let root = temp_tree("precedence");
    build_tree(&root);

    // Global placement is rejected before the subcommand even runs.
    let mut bad = Command::new(snapdir_bin());
    bad.arg("--exclude").arg("alpha"); // no longer a global flag
    bad.arg("manifest");
    bad.arg(root.to_string_lossy().into_owned());
    let bad_out = bad.output().expect("run snapdir");
    assert!(
        !bad_out.status.success(),
        "global --exclude before the subcommand must be rejected"
    );

    // Subcommand-scoped `--exclude` still works: gamma is dropped, the rest kept.
    let stdout = manifest_stdout(&root, &["--exclude", "gamma"]);
    assert_present(
        &stdout,
        &["alpha.txt", "beta.txt", "keep.txt"],
        &["gamma.txt"],
    );
    fs::remove_dir_all(&root).ok();
}
