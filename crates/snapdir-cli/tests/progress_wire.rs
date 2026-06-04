//! Non-regression + flag-plumbing tests for the wired live progress dashboard.
//!
//! The live progress line is drawn ONLY to a TTY stderr; `assert_cmd` runs the
//! binary with piped (non-TTY) stdio, so progress is auto-OFF for every case
//! here. That is exactly what we want to assert: wiring the `Meter` +
//! `ProgressReporter` (and the `--no-progress`/`--quiet`/`--color` flags) into
//! every transfer command must NOT perturb the scriptable stdout (id-only),
//! must emit no ANSI escapes / carriage-return redraws anywhere, and `--quiet`
//! must additionally swallow the `--verbose` banner. Every fn name contains
//! `progress_wire` so `cargo test -p snapdir-cli --locked progress_wire`
//! selects exactly this suite.
//!
//! All stores/caches/dirs live under `assert_fs` temp dirs removed on drop, so
//! the suite is hermetic (no network, no credentials, no `$HOME` writes).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` command with the cache pinned under `cache` so tests never
/// touch the user's real `$HOME/.cache/snapdir`. `TERM`/`NO_COLOR` are also
/// neutralized so the run is deterministic regardless of the host environment.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env_remove("NO_COLOR");
    cmd.env("TERM", "xterm-256color");
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

/// Runs `snapdir <args>` (cache pinned), asserting success and returning the
/// captured `Output` (stdout + stderr) for inspection.
fn run_ok(cache: &Path, args: &[&str]) -> Output {
    let out = snapdir(cache).args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

fn stdout_str(out: &Output) -> String {
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .trim_end()
        .to_owned()
}

/// True if the bytes carry an ANSI CSI escape (`\x1b[`) or an in-place
/// carriage-return redraw (`\r`).
fn has_ansi_or_redraw(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|w| w == b"\x1b[") || bytes.contains(&b'\r')
}

#[test]
fn progress_wire_piped_stdout_is_id_only() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    // push -> stdout is EXACTLY the 64-hex id, no ANSI / redraw anywhere.
    let push = run_ok(cache.path(), &["push", "--store", &a_url, &src_str]);
    let id = stdout_str(&push);
    assert_eq!(id.len(), 64, "push stdout must be the bare id: {id:?}");
    assert!(
        id.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "push stdout must be lowercase hex: {id:?}"
    );
    assert!(
        !has_ansi_or_redraw(&push.stdout),
        "push stdout must carry no ANSI/redraw"
    );
    assert!(
        !has_ansi_or_redraw(&push.stderr),
        "push stderr must carry no ANSI/redraw (piped => progress off)"
    );

    // sync -> stdout is EXACTLY the id, no ANSI / redraw anywhere.
    let sync = run_ok(
        cache.path(),
        &["sync", "--id", &id, "--from", &a_url, "--to", &b_url],
    );
    assert_eq!(stdout_str(&sync), id, "sync stdout must be the bare id");
    assert!(
        !has_ansi_or_redraw(&sync.stdout) && !has_ansi_or_redraw(&sync.stderr),
        "sync output must carry no ANSI/redraw"
    );
}

#[test]
fn progress_wire_no_progress_silent() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let url = format!("file://{}", store.path().display());

    let out = run_ok(
        cache.path(),
        &["--no-progress", "push", "--store", &url, &src_str],
    );
    let id = stdout_str(&out);
    assert_eq!(
        id.len(),
        64,
        "stdout must be the bare id under --no-progress"
    );
    assert!(!has_ansi_or_redraw(&out.stdout) && !has_ansi_or_redraw(&out.stderr));
}

#[test]
fn progress_wire_quiet_silent() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let url = format!("file://{}", store.path().display());

    // --quiet alone: id on stdout, no progress.
    let out = run_ok(
        cache.path(),
        &["--quiet", "push", "--store", &url, &src_str],
    );
    let id = stdout_str(&out);
    assert_eq!(id.len(), 64, "stdout must be the bare id under --quiet");
    assert!(!has_ansi_or_redraw(&out.stdout) && !has_ansi_or_redraw(&out.stderr));

    // --verbose --quiet: --quiet wins for the banner — stderr must NOT carry the
    // `transfers:` transfer-config line.
    let store2 = TempDir::new().unwrap();
    let url2 = format!("file://{}", store2.path().display());
    let out2 = run_ok(
        cache.path(),
        &["--verbose", "--quiet", "push", "--store", &url2, &src_str],
    );
    assert_eq!(
        stdout_str(&out2),
        id,
        "id must be unchanged under --verbose --quiet"
    );
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        !stderr2.contains("transfers:"),
        "--quiet must suppress the --verbose banner; stderr was: {stderr2:?}"
    );
    assert!(!has_ansi_or_redraw(&out2.stderr));
}

#[test]
fn progress_wire_color_never_no_ansi() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let url = format!("file://{}", store.path().display());

    let out = run_ok(
        cache.path(),
        &["--color", "never", "push", "--store", &url, &src_str],
    );
    assert_eq!(stdout_str(&out).len(), 64);
    assert!(
        !has_ansi_or_redraw(&out.stdout) && !has_ansi_or_redraw(&out.stderr),
        "--color never must yield no ANSI anywhere"
    );
}

#[test]
fn progress_wire_id_unchanged() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    let store_c = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());
    let c_url = format!("file://{}", store_c.path().display());

    // Plain push vs push with all progress flags present => identical id.
    let plain = stdout_str(&run_ok(
        cache.path(),
        &["push", "--store", &a_url, &src_str],
    ));
    let flagged = stdout_str(&run_ok(
        cache.path(),
        &[
            "--no-progress",
            "--quiet",
            "--color",
            "never",
            "push",
            "--store",
            &b_url,
            &src_str,
        ],
    ));
    assert_eq!(
        plain, flagged,
        "push id must not depend on the progress flags"
    );

    // sync id matches the source id with the flags present.
    let synced = run_ok(
        cache.path(),
        &[
            "--quiet", "sync", "--id", &plain, "--from", &a_url, "--to", &c_url,
        ],
    );
    assert_eq!(
        stdout_str(&synced),
        plain,
        "sync id must equal the source id with the flags present"
    );
}
