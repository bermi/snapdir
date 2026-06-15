//! Integration tests for the `defaults` subcommand, using `assert_cmd`.
//!
//! `snapdir defaults` prints the EFFECTIVE configuration: for every knob, its
//! resolved value and a source tag (`flag` | `env` | `default`), reflecting
//! flag/env overrides with flag>env>default precedence. A trailing `other-env:`
//! section surfaces any remaining set `SNAPDIR_*` var (a superset of the old
//! "echo env" behavior); the legacy `SNAPDIR_MANIFEST_*` vars are shown there
//! only when set and only under an explicit `legacy` label — never as live
//! knobs, and never as the old empty `SNAPDIR_MANIFEST_*=` cruft.
//!
//! Every test runs under a *controlled* environment: the inherited host env is
//! cleared and only the vars under test (plus `PATH`/`HOME` so the process can
//! run) are set, so the assertions are deterministic and never depend on the
//! tester's ambient `SNAPDIR*` variables.

use std::process::Command;

use assert_cmd::prelude::*;

/// A `snapdir` command with the inherited environment cleared, then only `PATH`
/// and a stable `HOME` re-set (so nothing resolves into the developer's real
/// home). Tests add the `SNAPDIR*` vars they want — nothing leaks in from the
/// host, so the output is fully deterministic.
fn snapdir_clean_env() -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", "/tmp/snapdir-defaults-test-home");
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
fn defaults_command_exits_zero() {
    snapdir_clean_env().arg("defaults").assert().success();
}

#[test]
fn defaults_command_reports_env_set_knobs_with_value_and_source() {
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_CACHE_DIR", "/tmp/x")
        .env("SNAPDIR_CATALOG", "foo")
        // A *VERSION* var must never leak into the effective report.
        .env("SNAPDIR_VERSION", "9.9.9")
        // A non-SNAPDIR var must never appear.
        .env("UNRELATED_VAR", "should-not-appear");

    let lines = defaults_lines(&mut cmd);

    // The env-set knobs are reported with their resolved value AND tagged `env`.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("cache-dir") && l.contains("/tmp/x") && l.contains("env")),
        "expected an env-tagged cache-dir=/tmp/x line in:\n{}",
        lines.join("\n"),
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("catalog") && l.contains("foo") && l.contains("env")),
        "expected an env-tagged catalog=foo line in:\n{}",
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
}

#[test]
fn defaults_command_legacy_manifest_vars_not_live_knobs() {
    // On a clean env the legacy SNAPDIR_MANIFEST_* vars must NOT appear at all
    // (no empty `=`-suffixed cruft, the prior behavior).
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_CACHE_DIR", "/tmp/x");
    let lines = defaults_lines(&mut cmd);

    for legacy in ["SNAPDIR_MANIFEST_CONTEXT", "SNAPDIR_MANIFEST_EXCLUDE"] {
        assert!(
            !lines.iter().any(|l| l.trim() == format!("{legacy}=")),
            "the old empty `{legacy}=` legacy line must not appear in:\n{}",
            lines.join("\n"),
        );
    }
}

#[test]
fn defaults_command_legacy_manifest_var_when_set_is_labeled_legacy() {
    // When SNAPDIR_MANIFEST_CONTEXT is set it is still surfaced (superset), but
    // only under an explicit `legacy` label — never as a live effective knob.
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_CACHE_DIR", "/tmp/x")
        .env("SNAPDIR_MANIFEST_CONTEXT", "mykey");
    let lines = defaults_lines(&mut cmd);

    let manifest_line = lines
        .iter()
        .find(|l| l.contains("SNAPDIR_MANIFEST_CONTEXT"))
        .expect("a set SNAPDIR_MANIFEST_CONTEXT must still be surfaced");
    assert!(
        manifest_line.contains("mykey"),
        "the surfaced legacy line must carry its value: {manifest_line:?}",
    );
    assert!(
        manifest_line.to_lowercase().contains("legacy"),
        "the surfaced legacy line must be labeled legacy: {manifest_line:?}",
    );
}

#[test]
fn defaults_command_is_deterministic() {
    let mut a = snapdir_clean_env();
    a.env("SNAPDIR_CACHE_DIR", "/tmp/x")
        .env("SNAPDIR_STORE", "file:///tmp/store");
    let out_a = a.arg("defaults").output().expect("run a");

    let mut b = snapdir_clean_env();
    b.env("SNAPDIR_CACHE_DIR", "/tmp/x")
        .env("SNAPDIR_STORE", "file:///tmp/store");
    let out_b = b.arg("defaults").output().expect("run b");

    assert_eq!(
        out_a.stdout, out_b.stdout,
        "two `defaults` runs on the same env must be byte-identical",
    );
}

#[test]
fn defaults_command_reports_store_env_var() {
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_CACHE_DIR", "/tmp/x")
        .env("SNAPDIR_STORE", "file:///tmp/store");
    let lines = defaults_lines(&mut cmd);

    assert!(
        lines
            .iter()
            .any(|l| l.contains("store") && l.contains("file:///tmp/store") && l.contains("env")),
        "expected an env-tagged store=file:///tmp/store line in:\n{}",
        lines.join("\n"),
    );
}
