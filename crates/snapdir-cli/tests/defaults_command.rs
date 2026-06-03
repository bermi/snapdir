//! Integration tests for the `defaults` subcommand, using `assert_cmd`.
//!
//! `snapdir defaults` emits three groups of lines — the manifest tool's
//! non-option defaults, every `SNAPDIR*` environment variable (excluding
//! `*VERSION*`) reformatted into `--option-name=value`, and a `SNAPDIR_BIN_PATH=…`
//! line for the running binary — combined under a final `sort -u`.
//!
//! Every test runs under a *controlled* environment: the inherited host env is
//! cleared and only the vars under test (plus `PATH`/`HOME` so the process can
//! run) are set, so the assertions are deterministic and never depend on the
//! tester's ambient `SNAPDIR*` variables.

use std::collections::HashSet;
use std::process::Command;

use assert_cmd::prelude::*;

/// A `snapdir` command with the inherited environment cleared, then only `PATH`
/// (so the process loader works) re-set. Tests add the `SNAPDIR*` vars they want
/// — nothing leaks in from the host, so the output is fully deterministic.
fn snapdir_clean_env() -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd
}

/// Runs `snapdir defaults` and returns its (asserted-success) stdout lines.
fn defaults_lines(cmd: &mut Command) -> Vec<String> {
    let out = cmd.arg("defaults").output().expect("run snapdir defaults");
    assert!(
        out.status.success(),
        "snapdir defaults failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout)
        .expect("utf8 stdout")
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn defaults_command_reformats_env_vars_and_excludes_version() {
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_CACHE_DIR", "/tmp/x")
        .env("SNAPDIR_CATALOG", "foo")
        // A *VERSION* var must be dropped entirely.
        .env("SNAPDIR_VERSION", "9.9.9")
        // A non-SNAPDIR var must never appear.
        .env("UNRELATED_VAR", "should-not-appear");

    let lines = defaults_lines(&mut cmd);

    // The env vars are reformatted: leading `SNAPDIR_` → `--`, `_`→`-`,
    // lowercased, as `--option=value`.
    assert!(
        lines.iter().any(|l| l == "--cache-dir=/tmp/x"),
        "expected --cache-dir=/tmp/x in:\n{}",
        lines.join("\n"),
    );
    assert!(
        lines.iter().any(|l| l == "--catalog=foo"),
        "expected --catalog=foo in:\n{}",
        lines.join("\n"),
    );

    // No *VERSION* SNAPDIR var leaks through, in any shape.
    assert!(
        !lines.iter().any(|l| l.to_lowercase().contains("version")),
        "no VERSION line may appear:\n{}",
        lines.join("\n"),
    );
    // Unrelated host vars never appear.
    assert!(
        !lines.iter().any(|l| l.contains("should-not-appear")),
        "non-SNAPDIR vars must not appear:\n{}",
        lines.join("\n"),
    );

    // The running-binary line is always emitted (group 3).
    assert!(
        lines.iter().any(|l| l.starts_with("SNAPDIR_BIN_PATH=")),
        "expected a SNAPDIR_BIN_PATH= line:\n{}",
        lines.join("\n"),
    );
}

#[test]
fn defaults_command_includes_manifest_default_lines() {
    let mut cmd = snapdir_clean_env();
    let lines = defaults_lines(&mut cmd);

    // Group 1: the manifest tool's non-option defaults (`grep -v "^-"` leaves
    // the `SNAPDIR_MANIFEST_*=` key lines). With an empty controlled env,
    // CONTEXT and EXCLUDE default to empty; the manifest bin path is present.
    assert!(
        lines.iter().any(|l| l == "SNAPDIR_MANIFEST_CONTEXT="),
        "expected SNAPDIR_MANIFEST_CONTEXT= in:\n{}",
        lines.join("\n"),
    );
    assert!(
        lines.iter().any(|l| l == "SNAPDIR_MANIFEST_EXCLUDE="),
        "expected SNAPDIR_MANIFEST_EXCLUDE= in:\n{}",
        lines.join("\n"),
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("SNAPDIR_MANIFEST_BIN_PATH=")),
        "expected SNAPDIR_MANIFEST_BIN_PATH= in:\n{}",
        lines.join("\n"),
    );
}

#[test]
fn defaults_command_manifest_context_reflects_env() {
    // When SNAPDIR_MANIFEST_CONTEXT is set, group 1 echoes it (the Rust manifest
    // walk honors it for keyed BLAKE3). NB: the same var also appears reformatted
    // in group 2 as `--manifest-context=…`; both lines are valid output.
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_MANIFEST_CONTEXT", "mykey");
    let lines = defaults_lines(&mut cmd);

    assert!(
        lines.iter().any(|l| l == "SNAPDIR_MANIFEST_CONTEXT=mykey"),
        "expected SNAPDIR_MANIFEST_CONTEXT=mykey in:\n{}",
        lines.join("\n"),
    );
}

#[test]
fn defaults_command_output_is_sorted_and_unique() {
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_CACHE_DIR", "/tmp/x")
        .env("SNAPDIR_CATALOG", "foo")
        .env("SNAPDIR_STORE", "file:///tmp/store");

    let lines = defaults_lines(&mut cmd);

    // `sort -u`: the output must equal its own sort, with no duplicates.
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(
        lines,
        sorted,
        "output must be sorted:\n{}",
        lines.join("\n")
    );

    let unique: HashSet<&String> = lines.iter().collect();
    assert_eq!(
        unique.len(),
        lines.len(),
        "output must be unique (no duplicate lines):\n{}",
        lines.join("\n"),
    );

    // No two adjacent lines are equal (sorted-unique invariant).
    for pair in lines.windows(2) {
        assert_ne!(pair[0], pair[1], "adjacent duplicate line: {:?}", pair[0]);
    }
}

#[test]
fn defaults_command_exits_zero() {
    snapdir_clean_env().arg("defaults").assert().success();
}

#[test]
fn defaults_command_reformats_store_env_var() {
    // The `--option=value` env reformat applies to every SNAPDIR* var, including
    // SNAPDIR_STORE -> --store=… (pins the shared env-derived option shape).
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_CACHE_DIR", "/tmp/x")
        .env("SNAPDIR_STORE", "file:///tmp/store");
    let lines = defaults_lines(&mut cmd);

    for expected in ["--cache-dir=/tmp/x", "--store=file:///tmp/store"] {
        assert!(
            lines.iter().any(|l| l == expected),
            "expected {expected} in:\n{}",
            lines.join("\n"),
        );
    }
}
