//! Fast-walk CLI contract tests (snapdir-cli, black-box) — ADVERSARY.
//!
//! Authored from the gate SPEC ONLY, driving the `snapdir` BINARY (no impl
//! visibility). Mirrors the existing CLI harness (`assert_cmd` + `assert_fs`,
//! cache pinned via `SNAPDIR_CACHE_DIR`) used by `tests/e2e.rs`.
//!
//! The contract: a new global `--walk-jobs <N>` flag + `SNAPDIR_WALK_JOBS` env
//! controls cross-file hashing parallelism and is SEPARATE from `--jobs` /
//! `SNAPDIR_JOBS` (transfer concurrency). It must NEVER change a snapshot id:
//! `snapdir id <fixture>` is byte-identical across `--walk-jobs 1`,
//! `--walk-jobs 8`, and `SNAPDIR_WALK_JOBS` unset, and across repeated runs.
//!
//! These tests reference `--walk-jobs` / `SNAPDIR_WALK_JOBS`, which do NOT exist
//! in the current binary (`snapdir id --walk-jobs 4` currently errors with
//! "unexpected argument '--walk-jobs'"), so the suite cannot pass until the CLI
//! lane lands the flag.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` command with the cache pinned so tests never touch the
/// user's real cache, and with the transfer-jobs / walk-jobs env vars cleared so
/// the ambient environment can't perturb a run.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env_remove("SNAPDIR_WALK_JOBS");
    cmd.env_remove("SNAPDIR_JOBS");
    cmd
}

/// Runs `snapdir <args>` (cache pinned), asserts success, returns trimmed stdout.
fn id_ok(cache: &Path, args: &[&str]) -> String {
    let out = snapdir(cache).args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// The deterministic bench-style ramp (`(i*31 + 7) & 0xff`), so the materialized
/// fixture (and thus its golden id) is reproducible across runs and machines.
fn deterministic_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i.wrapping_mul(31).wrapping_add(7) & 0xff).expect("masked to u8"))
        .collect()
}

#[cfg(unix)]
fn pin_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, PermissionsExt::from_mode(mode)).expect("set_permissions");
}
#[cfg(not(unix))]
fn pin_mode(_path: &Path, _mode: u32) {}

/// Materializes a fixture tree mixing MANY small files + a FEW large (> 256 KiB,
/// exercising the blake3 mmap path) + empty files + a unicode/space path, with
/// every file/dir mode pinned (umask-independent) so the golden id is stable.
///
/// This is the SAME recipe used to capture `GOLDEN_ID` from the current binary,
/// at two different parent locations (the id was identical and stable across
/// repeated runs), so the hardcoded golden is legitimate and location-independent.
fn build_fixture(root: &TempDir) {
    let base = root.path();
    pin_mode(base, 0o755);

    for i in 0..20 {
        let f = root.child(format!("small_{i:03}.bin"));
        f.write_binary(&deterministic_bytes(64)).unwrap();
        pin_mode(f.path(), 0o644);
    }
    // A few large files > 256 KiB so the parallel mmap path is exercised.
    for i in 0..3 {
        let f = root.child(format!("large_{i:02}.bin"));
        f.write_binary(&deterministic_bytes(300 * 1024)).unwrap();
        pin_mode(f.path(), 0o644);
    }
    // Empty files.
    for name in ["empty_a.bin", "empty_b.bin"] {
        let f = root.child(name);
        f.write_binary(&[]).unwrap();
        pin_mode(f.path(), 0o644);
    }
    // Unicode + space paths.
    let uni_dir = root.child("uni dir");
    uni_dir.create_dir_all().unwrap();
    pin_mode(uni_dir.path(), 0o755);
    let uf = root.child("uni dir/déjà vu.txt");
    uf.write_binary(&deterministic_bytes(128)).unwrap();
    pin_mode(uf.path(), 0o644);
    let crab = root.child("🦀 crab.bin");
    crab.write_binary(&deterministic_bytes(700 * 1024)).unwrap();
    pin_mode(crab.path(), 0o644);
    // A sub dir mixing sizes (incl. a > 256 KiB file with a space in the name).
    let sub = root.child("sub");
    sub.create_dir_all().unwrap();
    pin_mode(sub.path(), 0o755);
    let ssmall = root.child("sub/s small.bin");
    ssmall.write_binary(&deterministic_bytes(10)).unwrap();
    pin_mode(ssmall.path(), 0o644);
    let slarge = root.child("sub/s large.bin");
    slarge
        .write_binary(&deterministic_bytes(512 * 1024))
        .unwrap();
    pin_mode(slarge.path(), 0o644);
}

/// Golden snapshot id of `build_fixture`'s tree, captured from the CURRENT
/// (pre-change) `snapdir` binary on the byte-identical fixture. Location- and
/// run-independent (verified). `--walk-jobs` must NEVER change it.
const GOLDEN_ID: &str = "3d890b612c02d71c0c51aeb18a04357702f869da85f7691a709e1b88aea657ba";

// ===========================================================================
// CLI clause 1: byte-identical id across --walk-jobs 1 / 8 / unset + repeated runs.
// ===========================================================================

#[test]
fn id_is_byte_identical_across_walk_jobs_and_unset() {
    // Spec clause: `snapdir id <fixture>` is byte-identical across --walk-jobs 1,
    // --walk-jobs 8, and SNAPDIR_WALK_JOBS unset, and across repeated runs.
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_fixture(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    // Baseline: no walk-jobs flag, no env (the "unset" case) — must equal golden.
    let unset = id_ok(cache.path(), &["id", &src_str]);
    assert_eq!(
        unset, GOLDEN_ID,
        "baseline id must equal the recorded golden"
    );

    // --walk-jobs 1 and 8 must each equal the baseline (and golden).
    let j1 = id_ok(cache.path(), &["id", "--walk-jobs", "1", &src_str]);
    let j8 = id_ok(cache.path(), &["id", "--walk-jobs", "8", &src_str]);
    assert_eq!(j1, unset, "--walk-jobs 1 must match the unset id");
    assert_eq!(j8, unset, "--walk-jobs 8 must match the unset id");
    assert_eq!(j1, j8, "--walk-jobs 1 and 8 must agree");

    // Repeated runs are stable.
    let rerun = id_ok(cache.path(), &["id", "--walk-jobs", "8", &src_str]);
    assert_eq!(rerun, j8, "repeated --walk-jobs 8 run must be stable");
}

#[test]
fn id_via_env_var_matches_flag_and_unset() {
    // Spec clause: SNAPDIR_WALK_JOBS env is honored and produces the same id as
    // the flag and the unset case (env and flag are equivalent knobs).
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_fixture(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    let unset = id_ok(cache.path(), &["id", &src_str]);

    // Env-driven walk-jobs (note: snapdir() clears the env, so set it explicitly).
    let env_run = {
        let out = snapdir(cache.path())
            .env("SNAPDIR_WALK_JOBS", "3")
            .args(["id", &src_str])
            .output()
            .expect("run snapdir");
        assert!(
            out.status.success(),
            "snapdir id with SNAPDIR_WALK_JOBS=3 failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
    };
    assert_eq!(env_run, unset, "SNAPDIR_WALK_JOBS must not change the id");
    assert_eq!(env_run, GOLDEN_ID);
}

// ===========================================================================
// CLI clause 2: sort stability over a tree with small + large + empty + unicode/space.
// ===========================================================================

#[test]
fn sort_stability_over_mixed_unicode_and_space_tree() {
    // Spec clause: cover a tree mixing many small + a few large (>256KiB) + empty
    // + unicode/space paths; sort stability must hold (the id is stable run over
    // run and across job counts, which is the observable proof of stable sort).
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_fixture(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    let mut ids = Vec::new();
    for jobs in ["1", "2", "4", "8", "16"] {
        ids.push(id_ok(cache.path(), &["id", "--walk-jobs", jobs, &src_str]));
    }
    for id in &ids {
        assert_eq!(id, &ids[0], "id must be stable across every job count");
        assert_eq!(id, GOLDEN_ID, "id must equal the recorded golden");
    }
}

// ===========================================================================
// CLI clause 3: --walk-jobs is SEPARATE from --jobs (both may be set, no conflict).
// ===========================================================================

#[test]
fn walk_jobs_is_separate_from_transfer_jobs() {
    // Spec clause: --walk-jobs is SEPARATE from --jobs (transfer concurrency).
    // Setting BOTH on `id` must NOT error and must produce the same golden id —
    // proving they are independent knobs, not aliases.
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_fixture(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    // Both flags together: must succeed (no "conflicting/duplicate argument").
    let both = id_ok(
        cache.path(),
        &["id", "--walk-jobs", "4", "--jobs", "2", &src_str],
    );
    assert_eq!(
        both, GOLDEN_ID,
        "combining --walk-jobs and --jobs must not change the id"
    );

    // --walk-jobs alone must not error even though it differs from --jobs.
    let walk_only = id_ok(cache.path(), &["id", "--walk-jobs", "4", &src_str]);
    assert_eq!(walk_only, both);

    // Both env vars set together: still no conflict, same id.
    let env_both = {
        let out = snapdir(cache.path())
            .env("SNAPDIR_WALK_JOBS", "4")
            .env("SNAPDIR_JOBS", "2")
            .args(["id", &src_str])
            .output()
            .expect("run snapdir");
        assert!(
            out.status.success(),
            "both env vars set must not conflict: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
    };
    assert_eq!(env_both, GOLDEN_ID);
}

#[test]
fn walk_jobs_flag_is_advertised_in_help() {
    // Spec corollary: the CLI lane documents --walk-jobs / SNAPDIR_WALK_JOBS in
    // help text. Assert the top-level help mentions the flag and the env var.
    let cache = TempDir::new().unwrap();
    let out = snapdir(cache.path())
        .arg("--help")
        .output()
        .expect("run snapdir --help");
    assert!(out.status.success(), "--help must succeed");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--walk-jobs"),
        "help must advertise the --walk-jobs flag"
    );
    assert!(
        help.contains("SNAPDIR_WALK_JOBS"),
        "help must advertise the SNAPDIR_WALK_JOBS env var"
    );
}
