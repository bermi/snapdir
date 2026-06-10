//! Parity tests for the flagship `snapdir` binary (gate `snapdir-name-crate`).
//!
//! The bin target moved here from `snapdir-cli` (cargo warns "output filename
//! collision" when two workspace packages emit a `snapdir` bin, so only this
//! crate owns it — design (b)). These tests pin the shim binary to the
//! EXPECTED outputs the old `snapdir-cli` binary produced:
//!
//! - `version` / `--version` print the exact frozen `snapdir <semver>` line
//!   (the workspace version, shared by `snapdir` and `snapdir-cli`);
//! - `--help` stays byte-identical to the documented surface pinned by the
//!   `snapdir-cli` trycmd snapshot (`tests/cmd/help.trycmd`), so the shim can
//!   never drift from the snapshot suite that guards the implementation;
//! - `defaults` keeps the oracle's `sort -u` shape under a controlled env;
//! - an `id`/`push`/`fetch`/`checkout` round-trip over a temp `file://`
//!   store with an isolated `SNAPDIR_CACHE_DIR` reproduces the snapshot id
//!   (condensed from `snapdir-cli/tests/store_roundtrip.rs`).
//!
//! `env!("CARGO_BIN_EXE_snapdir")` resolves here because this crate defines
//! the bin target.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Path to the compiled `snapdir` binary under test.
fn snapdir_bin() -> &'static str {
    env!("CARGO_BIN_EXE_snapdir")
}

/// Creates a unique temp directory and returns its path.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-parity-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Runs the binary with an isolated cache dir and returns the raw output.
fn run_raw(args: &[&str], cache: &Path) -> Output {
    Command::new(snapdir_bin())
        .args(args)
        .env("SNAPDIR_CACHE_DIR", cache)
        .output()
        .expect("run snapdir")
}

/// Runs the binary, asserting success + empty stderr, returning trimmed stdout.
fn run_ok(args: &[&str], cache: &Path) -> String {
    let output = run_raw(args, cache);
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

/// `version` and `--version` print the exact frozen `snapdir <semver>` line
/// on stdout (nothing on stderr, exit 0). The semver is the workspace
/// version, identical for the `snapdir` shim and the `snapdir-cli`
/// implementation, so this matches what the old binary printed byte-for-byte.
#[test]
fn version_prints_the_frozen_line() {
    let cache = temp_dir("version-cache");
    let expected = format!("snapdir {}\n", env!("CARGO_PKG_VERSION"));
    for args in [&["version"][..], &["--version"][..]] {
        let output = run_raw(args, &cache);
        assert!(output.status.success(), "snapdir {args:?} must exit 0");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "snapdir {args:?} stdout"
        );
        assert!(
            output.stderr.is_empty(),
            "snapdir {args:?} must print nothing to stderr"
        );
    }
    fs::remove_dir_all(&cache).ok();
}

/// `--help` must stay byte-identical to the surface documented by the
/// `snapdir-cli` trycmd snapshot (`tests/cmd/help.trycmd`): the flagship shim
/// and the snapshot suite guard the SAME binary surface and can never drift
/// apart. (Compared modulo trailing newlines — trycmd pads the fenced block.)
#[test]
fn help_matches_the_documented_trycmd_surface() {
    let snapshot = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../snapdir-cli/tests/cmd/help.trycmd"
    );
    let raw = fs::read_to_string(snapshot).expect("read snapdir-cli help.trycmd");
    // Minimal trycmd parse: drop the opening fence + `$ snapdir --help` line,
    // keep everything up to the closing fence.
    let mut lines = raw.lines();
    assert_eq!(lines.next(), Some("```"), "snapshot opens with a fence");
    assert_eq!(
        lines.next(),
        Some("$ snapdir --help"),
        "snapshot pins the --help invocation"
    );
    let expected: Vec<&str> = lines.take_while(|line| *line != "```").collect();
    let expected = expected.join("\n");

    // Cleared env (like trycmd): clap echoes live `SNAPDIR*` values into the
    // `[env: …]` lines, so nothing may leak in from the host.
    let output = Command::new(snapdir_bin())
        .arg("--help")
        .env_clear()
        .envs(std::env::var("PATH").map(|p| ("PATH".to_owned(), p)))
        .output()
        .expect("run snapdir --help");
    assert!(output.status.success(), "--help must exit 0");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(
        stdout.trim_end(),
        expected.trim_end(),
        "snapdir --help drifted from snapdir-cli/tests/cmd/help.trycmd"
    );
}

/// `defaults` under a controlled environment keeps the oracle's shape:
/// `SNAPDIR*` env reformatted to `--option=value`, the manifest defaults, a
/// `SNAPDIR_BIN_PATH=` line naming THIS binary, all under a final `sort -u`.
#[test]
fn defaults_keeps_the_oracle_shape() {
    let output = Command::new(snapdir_bin())
        .arg("defaults")
        .env_clear()
        .envs(std::env::var("PATH").map(|p| ("PATH".to_owned(), p)))
        .env("SNAPDIR_CACHE_DIR", "/tmp/parity-cache")
        .output()
        .expect("run snapdir defaults");
    assert!(
        output.status.success(),
        "defaults failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();

    assert!(
        lines.contains(&"--cache-dir=/tmp/parity-cache"),
        "SNAPDIR_CACHE_DIR must be reformatted to --cache-dir=…; got:\n{stdout}"
    );
    let bin_line = format!("SNAPDIR_BIN_PATH={}", snapdir_bin());
    assert!(
        lines.contains(&bin_line.as_str()),
        "defaults must name the running binary ({bin_line}); got:\n{stdout}"
    );
    // The final `sort -u`: sorted, no duplicates.
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(lines, sorted, "defaults output must be `sort -u`-shaped");
}

/// id → push → fetch → checkout round-trip over a temp `file://` store with
/// an isolated cache: the pushed id equals the source id, the manifest lands
/// in the store, and the checked-out tree re-manifests to the SAME id.
#[test]
fn file_store_roundtrip_preserves_the_snapshot_id() {
    let src = temp_dir("src");
    let store = temp_dir("store");
    let dest = temp_dir("dest");
    let cache = temp_dir("cache");

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

    let src_id = run_ok(&["id", &src_str], &cache);
    assert_eq!(src_id.len(), 64, "snapshot id should be 64 hex chars");

    let pushed_id = run_ok(&["push", "--store", &store_url, &src_str], &cache);
    assert_eq!(pushed_id, src_id, "push must print the source snapshot id");

    // The manifest landed at its frozen sharded key in the store.
    let manifest_key = store.join(format!(
        ".manifests/{}/{}/{}/{}",
        &src_id[0..3],
        &src_id[3..6],
        &src_id[6..9],
        &src_id[9..]
    ));
    assert!(
        manifest_key.is_file(),
        "manifest must land at sharded key {}",
        manifest_key.display()
    );

    // fetch into a FRESH cache (proves the store copy is complete), then
    // checkout offline from that cache only.
    let cache2 = temp_dir("cache2");
    run_ok(&["fetch", "--store", &store_url, "--id", &src_id], &cache2);
    run_ok(&["checkout", "--id", &src_id, &dest_str], &cache2);

    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
    assert_eq!(
        fs::read(dest.join("sub").join("b.txt")).unwrap(),
        b"world!!"
    );
    let dest_id = run_ok(&["id", &dest_str], &cache2);
    assert_eq!(
        dest_id, src_id,
        "checked-out tree must re-manifest to the source snapshot id"
    );

    for dir in [&src, &store, &dest, &cache, &cache2] {
        fs::remove_dir_all(dir).ok();
    }
}

/// End-to-end install smoke: `cargo install --path crates/snapdir --root
/// <tempdir> --locked` then run `<root>/bin/snapdir version`.
///
/// `#[ignore]`: `cargo install` rebuilds the full dependency graph in release
/// mode inside its own staging target (several minutes — far over the 60s
/// budget) and must not race the shared `target/` dir while the workspace
/// test suite is running. Run it explicitly:
/// `cargo test -p snapdir -- --ignored cargo_install`.
#[test]
#[ignore = "cargo install rebuilds the workspace in release mode (minutes); run with -- --ignored"]
fn cargo_install_smoke() {
    let root = temp_dir("install-root");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let status = Command::new(env!("CARGO"))
        .args(["install", "--path", manifest_dir, "--locked", "--root"])
        .arg(&root)
        .status()
        .expect("run cargo install");
    assert!(
        status.success(),
        "cargo install --path crates/snapdir failed"
    );

    let installed = root.join("bin").join("snapdir");
    let output = Command::new(&installed)
        .arg("version")
        .output()
        .expect("run installed snapdir");
    assert!(output.status.success(), "installed snapdir version failed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("snapdir {}\n", env!("CARGO_PKG_VERSION")),
        "installed binary must print the frozen version line"
    );

    fs::remove_dir_all(&root).ok();
}
