//! Integration test: the new rate-limit / retry `SNAPDIR_*` env vars surface in
//! `snapdir defaults`.
//!
//! `snapdir defaults` enumerates every `SNAPDIR*` environment variable
//! (excluding `*VERSION*`) reformatted into `--option-name=value`. The
//! rate-limit / retry knobs are plain env vars, so when set they must appear in
//! that listing — this pins that they are not silently dropped.

use std::process::Command;

use assert_cmd::prelude::*;

/// A `snapdir` command with the inherited environment cleared, then only `PATH`
/// re-set (so the process loader works). Tests add the `SNAPDIR*` vars under test
/// so the output is deterministic and never depends on the host env.
fn snapdir_clean_env() -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd
}

#[test]
fn ratelimit_defaults_lists_new_env_vars() {
    let mut cmd = snapdir_clean_env();
    cmd.env("SNAPDIR_MAX_REQUESTS", "3")
        .env("SNAPDIR_MAX_RETRIES", "7")
        .env("SNAPDIR_RETRY_BASE_MS", "100")
        .env("SNAPDIR_RETRY_MAX_MS", "9000");

    let out = cmd.arg("defaults").output().expect("run snapdir defaults");
    assert!(
        out.status.success(),
        "snapdir defaults failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let lines: Vec<&str> = stdout.lines().collect();

    // The env reformat is: leading `SNAPDIR_` -> `--`, `_` -> `-`, lowercased,
    // emitted as `--option=value`.
    for expected in [
        "--max-requests=3",
        "--max-retries=7",
        "--retry-base-ms=100",
        "--retry-max-ms=9000",
    ] {
        assert!(
            lines.contains(&expected),
            "expected {expected} in `snapdir defaults`:\n{stdout}",
        );
    }
}
