//! Black-box "argument hygiene, approach B" contract for snapdir 1.8.0
//! (gate `dx-arg-spec-tests`, phase 30).
//!
//! These tests pin the *desired post-fix* CLI contract: flags are restructured
//! into per-command groups so clap NATIVELY rejects inapplicable flags, every
//! command's `--help` shows only that command's flags, `--debug` is gone, and
//! valid invocations are byte-for-byte unchanged. They are authored from the
//! SPEC alone (no `src/` was read) and are EXPECTED TO FAIL against the current
//! binary (where every flag is `global = true`). The implementation lands in the
//! next gate — do not weaken these to pass.
//!
//! Conventions mirror `tests/e2e.rs` / `tests/defaults_command.rs`: drive the
//! built binary with `assert_cmd`, pin the cache under a tempdir so nothing
//! touches the user's real `$HOME/.cache/snapdir`, and build a tiny deterministic
//! fixture tree in-test so the suite is fully hermetic.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` command with the cache pinned under `cache` so tests never
/// touch the user's real `$HOME/.cache/snapdir`.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd
}

/// Builds a known tiny tree (a couple of files + a subdir) with explicit,
/// deterministic permissions so `id`/`manifest` are reproducible.
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

/// A fixture: a temp cache dir and a temp source tree, both removed on drop.
struct Fixture {
    cache: TempDir,
    tree: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let cache = TempDir::new().unwrap();
        let tree = TempDir::new().unwrap();
        build_tree(&tree);
        Fixture { cache, tree }
    }

    fn cmd(&self) -> Command {
        snapdir(self.cache.path())
    }

    fn tree_path(&self) -> &Path {
        self.tree.path()
    }
}

/// Runs `snapdir <args>` (cache pinned), returns the raw `Output`.
fn run(fx: &Fixture, args: &[&str]) -> std::process::Output {
    fx.cmd().args(args).output().expect("run snapdir")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ───────────────────────── 1. Inapplicable flag → exit 2 ─────────────────────
//
// Contract: an inapplicable flag used EXPLICITLY on a command is rejected by
// clap with exit code 2 and a message naming both the flag and the command.

/// Clause 1: a transfer flag (`--jobs`) on a local query command (`id`) → exit 2,
/// error names `--jobs` and `id`.
#[test]
fn id_rejects_transfer_flag_jobs() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let out = run(&fx, &["id", "--jobs", "4", &dir]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`id --jobs` must exit 2; stderr:\n{}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("--jobs"),
        "error must name the offending flag `--jobs`:\n{}",
        stderr_of(&out)
    );
    assert!(
        err.contains("id"),
        "error must name the command `id`:\n{}",
        stderr_of(&out)
    );
}

/// Clause 1: a transfer flag (`--limit-rate`) on a local command (`manifest`) →
/// exit 2, error names `--limit-rate` and `manifest`.
#[test]
fn manifest_rejects_transfer_flag_limit_rate() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let out = run(&fx, &["manifest", "--limit-rate", "1M", &dir]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`manifest --limit-rate` must exit 2; stderr:\n{}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("--limit-rate"),
        "error must name `--limit-rate`:\n{}",
        stderr_of(&out)
    );
    assert!(
        err.contains("manifest"),
        "error must name `manifest`:\n{}",
        stderr_of(&out)
    );
}

/// Clause 1: a staging/transfer flag (`--keep`) on a pure query command
/// (`defaults`) → exit 2, error names `--keep` and `defaults`.
#[test]
fn defaults_rejects_staging_flag_keep() {
    let fx = Fixture::new();
    let out = run(&fx, &["defaults", "--keep"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`defaults --keep` must exit 2; stderr:\n{}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("--keep"),
        "error must name `--keep`:\n{}",
        stderr_of(&out)
    );
    assert!(
        err.contains("defaults"),
        "error must name `defaults`:\n{}",
        stderr_of(&out)
    );
}

/// Clause 1: a transfer/parallelism flag (`--walk-jobs`) on `diff` (a
/// manifests-only command) → exit 2, error names `--walk-jobs` and `diff`.
#[test]
fn diff_rejects_walk_jobs_flag() {
    let fx = Fixture::new();
    // `diff` needs two operands; supply two dirs so the ONLY parse failure is the
    // inapplicable flag, not a missing-argument error.
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let out = run(&fx, &["diff", "--walk-jobs", "2", &dir, &dir]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`diff --walk-jobs` must exit 2; stderr:\n{}",
        stderr_of(&out)
    );
    let err = stderr_of(&out).to_lowercase();
    assert!(
        err.contains("--walk-jobs"),
        "error must name `--walk-jobs`:\n{}",
        stderr_of(&out)
    );
    assert!(
        err.contains("diff"),
        "error must name `diff`:\n{}",
        stderr_of(&out)
    );
}

// ───────────────────────── 2. Per-command --help is scoped ───────────────────
//
// Contract: per-command `--help` shows ONLY that command's flags.

/// Clause 2: `id --help` does NOT advertise transfer/staging flags
/// `--limit-rate`, `--store`, or `--jobs`.
#[test]
fn id_help_is_scoped_no_transfer_flags() {
    let fx = Fixture::new();
    let out = run(&fx, &["id", "--help"]);
    assert!(
        out.status.success(),
        "`id --help` must succeed; stderr:\n{}",
        stderr_of(&out)
    );
    let help = stdout_of(&out);
    for forbidden in ["--limit-rate", "--store", "--jobs"] {
        assert!(
            !help.contains(forbidden),
            "`id --help` must NOT advertise `{forbidden}`:\n{help}"
        );
    }
}

/// Clause 2: `manifest --help` does NOT advertise the transfer flag
/// `--limit-rate`.
#[test]
fn manifest_help_is_scoped_no_limit_rate() {
    let fx = Fixture::new();
    let out = run(&fx, &["manifest", "--help"]);
    assert!(
        out.status.success(),
        "`manifest --help` must succeed; stderr:\n{}",
        stderr_of(&out)
    );
    let help = stdout_of(&out);
    assert!(
        !help.contains("--limit-rate"),
        "`manifest --help` must NOT advertise `--limit-rate`:\n{help}"
    );
}

/// Clause 2 (converse): a transfer command's help (`push --help`) DOES advertise
/// the applicable transfer flags `--jobs` and `--limit-rate`.
#[test]
fn push_help_advertises_transfer_flags() {
    let fx = Fixture::new();
    let out = run(&fx, &["push", "--help"]);
    assert!(
        out.status.success(),
        "`push --help` must succeed; stderr:\n{}",
        stderr_of(&out)
    );
    let help = stdout_of(&out);
    for expected in ["--jobs", "--limit-rate"] {
        assert!(
            help.contains(expected),
            "`push --help` must advertise the applicable flag `{expected}`:\n{help}"
        );
    }
}

// ───────────────────────── 3. --debug is removed ────────────────────────────
//
// Contract: `--debug` is removed; using it is an unknown-argument error (exit 2).

/// Clause 3: `--debug` is an unknown argument on `id` → exit 2.
#[test]
fn debug_removed_on_id() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let out = run(&fx, &["id", "--debug", &dir]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`id --debug` must be an unknown-arg error (exit 2); stderr:\n{}",
        stderr_of(&out)
    );
    assert!(
        stderr_of(&out).to_lowercase().contains("--debug"),
        "error must name the removed `--debug` flag:\n{}",
        stderr_of(&out)
    );
}

/// Clause 3: `--debug` is an unknown argument on `manifest` → exit 2.
#[test]
fn debug_removed_on_manifest() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let out = run(&fx, &["manifest", "--debug", &dir]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`manifest --debug` must be an unknown-arg error (exit 2); stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 3: `--debug` is an unknown argument on `defaults` → exit 2.
#[test]
fn debug_removed_on_defaults() {
    let fx = Fixture::new();
    let out = run(&fx, &["defaults", "--debug"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`defaults --debug` must be an unknown-arg error (exit 2); stderr:\n{}",
        stderr_of(&out)
    );
}

// ─────────────────── 4. Env-set inapplicable SNAPDIR_* stays SILENT ──────────
//
// Contract: ONLY explicit CLI use of an inapplicable flag errors. An exported
// SNAPDIR_* env var that doesn't apply to the command must never break it.

/// Clause 4: `SNAPDIR_LIMIT_RATE=1M snapdir manifest <dir>` exits 0 and its
/// stdout is byte-identical to the same `manifest` with no such env var set.
#[test]
fn env_set_inapplicable_flag_is_silent_and_inert() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();

    let baseline = fx
        .cmd()
        .args(["manifest", &dir])
        .output()
        .expect("baseline manifest");
    assert!(
        baseline.status.success(),
        "baseline `manifest` must succeed; stderr:\n{}",
        stderr_of(&baseline)
    );

    let with_env = fx
        .cmd()
        .env("SNAPDIR_LIMIT_RATE", "1M")
        .args(["manifest", &dir])
        .output()
        .expect("manifest with SNAPDIR_LIMIT_RATE");
    assert!(
        with_env.status.success(),
        "`manifest` with an inapplicable SNAPDIR_LIMIT_RATE env must STILL exit 0; stderr:\n{}",
        stderr_of(&with_env)
    );

    assert_eq!(
        with_env.stdout,
        baseline.stdout,
        "an exported inapplicable env var must not change manifest output\n\
         baseline:\n{}\nwith env:\n{}",
        stdout_of(&baseline),
        stdout_of(&with_env)
    );
}

// ─────────────────── 5. Universal flags accepted everywhere ──────────────────
//
// Contract: `--quiet`, `--color auto`, `--no-progress`, `--verbose` are accepted
// (exit 0) on a representative spread of commands.

/// Clause 5: universal flags are accepted (exit 0) on `id`.
#[test]
fn universal_flags_accepted_on_id() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    for flag_args in [
        vec!["--quiet"],
        vec!["--color", "auto"],
        vec!["--no-progress"],
        vec!["--verbose"],
    ] {
        let mut args = vec!["id"];
        args.extend(flag_args.iter().copied());
        args.push(&dir);
        let out = run(&fx, &args);
        assert!(
            out.status.success(),
            "`id {flag_args:?}` must be accepted (exit 0); stderr:\n{}",
            stderr_of(&out)
        );
    }
}

/// Clause 5: universal flags are accepted (exit 0) on `manifest`.
#[test]
fn universal_flags_accepted_on_manifest() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    for flag_args in [
        vec!["--quiet"],
        vec!["--color", "auto"],
        vec!["--no-progress"],
        vec!["--verbose"],
    ] {
        let mut args = vec!["manifest"];
        args.extend(flag_args.iter().copied());
        args.push(&dir);
        let out = run(&fx, &args);
        assert!(
            out.status.success(),
            "`manifest {flag_args:?}` must be accepted (exit 0); stderr:\n{}",
            stderr_of(&out)
        );
    }
}

/// Clause 5: universal flags are accepted (exit 0) on `defaults`.
#[test]
fn universal_flags_accepted_on_defaults() {
    let fx = Fixture::new();
    for flag_args in [
        vec!["--quiet"],
        vec!["--color", "auto"],
        vec!["--no-progress"],
        vec!["--verbose"],
    ] {
        let mut args = vec!["defaults"];
        args.extend(flag_args.iter().copied());
        let out = run(&fx, &args);
        assert!(
            out.status.success(),
            "`defaults {flag_args:?}` must be accepted (exit 0); stderr:\n{}",
            stderr_of(&out)
        );
    }
}

// ───────────────── 6. KEYSTONE — no behavior regression for valid use ────────
//
// Contract: restructuring flags must NOT change behavior of valid invocations.

/// Clause 6 (KEYSTONE): `id <dir>` is deterministic — two runs produce
/// byte-identical stdout and a 64-hex id — and a baseline computed in-test is
/// reproduced exactly.
#[test]
fn keystone_id_is_deterministic_and_unchanged() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();

    let first = run(&fx, &["id", &dir]);
    assert!(
        first.status.success(),
        "`id` must succeed; stderr:\n{}",
        stderr_of(&first)
    );
    let baseline = first.stdout.clone();

    // Determinism: a second run is byte-identical.
    let second = run(&fx, &["id", &dir]);
    assert!(second.status.success(), "second `id` must succeed");
    assert_eq!(
        second.stdout,
        baseline,
        "`id` must be deterministic across runs\nfirst:\n{}\nsecond:\n{}",
        stdout_of(&first),
        stdout_of(&second)
    );

    // The id itself is a 64-char lowercase hex BLAKE3 digest.
    let id = stdout_of(&first).trim().to_owned();
    assert_eq!(id.len(), 64, "id must be 64 hex chars, got {id:?}");
    assert!(
        id.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "id must be lowercase hex: {id:?}"
    );
}

/// Clause 6 (KEYSTONE): a valid `manifest <dir>` with no flags still works and
/// emits a non-empty manifest (the applicable happy path is intact).
#[test]
fn keystone_manifest_valid_use_still_works() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let out = run(&fx, &["manifest", &dir]);
    assert!(
        out.status.success(),
        "valid `manifest <dir>` must succeed; stderr:\n{}",
        stderr_of(&out)
    );
    assert!(
        !stdout_of(&out).trim().is_empty(),
        "valid `manifest <dir>` must emit a non-empty manifest"
    );
}

// ───────────────── 7. Folded findings — desired post-fix behavior ────────────

/// Clause 7: `manifest --id <ID> <dir>` must NOT silently re-walk cwd. Either
/// `--id` is rejected as inapplicable to `manifest` (preferred) or it is honored
/// — but it must NEVER silently emit a fresh manifest of `<dir>` while `--id` is
/// set. Pin: the output must NOT equal the plain `manifest <dir>` walk.
#[test]
fn manifest_id_flag_is_not_silently_ignored() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();

    // A real-shaped (but not-present) 64-hex id.
    let some_id = "0".repeat(64);

    let plain = run(&fx, &["manifest", &dir]);
    assert!(
        plain.status.success(),
        "plain `manifest <dir>` must succeed for the comparison baseline; stderr:\n{}",
        stderr_of(&plain)
    );

    let with_id = run(&fx, &["manifest", "--id", &some_id, &dir]);

    if with_id.status.success() {
        // If accepted, it must NOT be a silent fresh walk of <dir>: the output
        // must differ from the plain walk (it should reflect the id, not cwd).
        assert_ne!(
            with_id.stdout, plain.stdout,
            "`manifest --id <ID> <dir>` silently re-walked the dir (output identical \
             to plain `manifest <dir>`); --id was ignored, which is forbidden"
        );
    } else {
        // Otherwise it must be a clean rejection (nonzero), not a silent walk.
        assert_ne!(
            with_id.status.code(),
            Some(0),
            "`manifest --id` must be honored or rejected, never silently ignored"
        );
    }
}

/// Clause 7: a bogus `--limit-rate` value is REJECTED (nonzero) rather than
/// silently accepted, on a transfer command where the flag is applicable.
#[test]
fn limit_rate_bogus_value_is_rejected() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    // Use `push` (transfer command) so `--limit-rate` is applicable; the value
    // itself is malformed and must be rejected before/regardless of transfer.
    let store = TempDir::new().unwrap();
    let store_url = format!("file://{}", store.path().to_str().unwrap());
    let out = run(
        &fx,
        &["push", "--store", &store_url, "--limit-rate", "bogus", &dir],
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "`--limit-rate bogus` must be rejected (nonzero), not silently accepted; stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 7: a bogus `--color` value is REJECTED (nonzero) rather than silently
/// accepted.
#[test]
fn color_bogus_value_is_rejected() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let out = run(&fx, &["id", "--color", "bogus", &dir]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "`--color bogus` must be rejected (nonzero), not silently accepted; stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 7: `--paths <nomatch>` on `id` must not be a silent no-op. Either it
/// filters (which changes the hash vs the unfiltered full-tree id) or it is
/// rejected (nonzero). It must NOT return the unchanged full-tree hash while
/// claiming to filter.
#[test]
fn paths_nomatch_is_not_a_silent_noop() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();

    let full = run(&fx, &["id", &dir]);
    assert!(
        full.status.success(),
        "unfiltered `id <dir>` must succeed for the baseline; stderr:\n{}",
        stderr_of(&full)
    );
    let full_id = stdout_of(&full).trim().to_owned();

    let filtered = run(&fx, &["id", "--paths", "does/not/match", &dir]);

    if filtered.status.success() {
        let filtered_id = stdout_of(&filtered).trim().to_owned();
        assert_ne!(
            filtered_id, full_id,
            "`id --paths <nomatch>` returned the unchanged full-tree hash — \
             the filter was a silent no-op, which is forbidden"
        );
    } else {
        assert_ne!(
            filtered.status.code(),
            Some(0),
            "`id --paths <nomatch>` must filter or be rejected, never a silent no-op"
        );
    }
}

// ─────────────────── 8. Impl-revealed branches (review gate) ─────────────────
//
// Added in the `dx-arg-review` gate now that the approach-B impl is visible.
// These pin the per-command flag-grouping quirks the source code reveals:
//   * `--catalog` is carried by `manifest` (own field), `stage`/`push`
//     (TransferArgs) — but NOT by `id` (WalkArgs only), which clap rejects.
//   * the hidden plumbing commands (`objects-needed`/`send-pack`/`receive-pack`)
//     carry `--store` (env `SNAPDIR_STORE`) via PlumbingArgs.
//   * `sync --from` falls back to `$SNAPDIR_STORE`.
//   * `--jobs` is a TransferArgs flag (push) and absent on the pure-walk
//     commands (`id`/`manifest`).
// Every test here must PASS against the current (correct) binary; a failure is a
// REAL BUG to flag, not an assertion to weaken.

/// Returns true iff `stderr` looks like clap's unknown/unexpected-argument
/// rejection for `flag` — i.e. the flag did NOT get past argument parsing.
fn is_unexpected_arg(stderr: &str, flag: &str) -> bool {
    let s = stderr.to_lowercase();
    (s.contains("unexpected argument") || s.contains("unknown argument"))
        && s.contains(&flag.to_lowercase())
}

/// A throwaway `file://` store URI under a fresh tempdir (kept alive by the
/// returned guard) so plumbing/transfer commands have a syntactically valid
/// `--store` to parse.
fn file_store() -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    let uri = format!("file://{}", dir.path().to_str().unwrap());
    (dir, uri)
}

// ── 8a. Catalog scoping quirk: stage/push/manifest ACCEPT --catalog, id REJECTS

/// Clause 8: `manifest --catalog <dir> <tree>` is ACCEPTED (exit 0) and the
/// location is honored — `locations --catalog <dir>` then lists the tree.
#[test]
fn manifest_accepts_and_honors_catalog() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let catalog = TempDir::new().unwrap();
    let cat = catalog
        .child("catalog.redb")
        .path()
        .to_str()
        .unwrap()
        .to_owned();

    let out = run(&fx, &["manifest", "--catalog", &cat, &dir]);
    assert!(
        out.status.success(),
        "`manifest --catalog` must be accepted (exit 0); stderr:\n{}",
        stderr_of(&out)
    );

    // It is HONORED, not silently dropped: the catalog now lists the tree.
    let locs = run(&fx, &["locations", "--catalog", &cat]);
    assert!(
        locs.status.success(),
        "`locations --catalog` must succeed; stderr:\n{}",
        stderr_of(&locs)
    );
    assert!(
        stdout_of(&locs).contains(&dir),
        "`manifest --catalog` must LOG the manifested dir; locations:\n{}",
        stdout_of(&locs)
    );
}

/// Clause 8: `stage --catalog <dir> <tree>` is ACCEPTED (exit 0) — `--catalog`
/// is a `TransferArgs` flag carried by the staging command.
#[test]
fn stage_accepts_catalog() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let catalog = TempDir::new().unwrap();
    let cat = catalog
        .child("catalog.redb")
        .path()
        .to_str()
        .unwrap()
        .to_owned();

    let out = run(&fx, &["stage", "--catalog", &cat, &dir]);
    assert!(
        out.status.success(),
        "`stage --catalog` must be accepted (exit 0); stderr:\n{}",
        stderr_of(&out)
    );
    // And it is honored: the staged base dir is logged.
    let locs = run(&fx, &["locations", "--catalog", &cat]);
    assert!(
        stdout_of(&locs).contains(&dir),
        "`stage --catalog` must LOG the staged dir; locations:\n{}",
        stdout_of(&locs)
    );
}

/// Clause 8: `push --catalog … <tree>` gets PAST arg parsing — `--catalog` is a
/// `TransferArgs` flag on `push`, so clap does not reject it as unexpected.
#[test]
fn push_accepts_catalog_flag() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let catalog = TempDir::new().unwrap();
    let cat = catalog
        .child("catalog.redb")
        .path()
        .to_str()
        .unwrap()
        .to_owned();
    let (_store, store_uri) = file_store();

    let out = run(
        &fx,
        &["push", "--store", &store_uri, "--catalog", &cat, &dir],
    );
    assert!(
        !is_unexpected_arg(&stderr_of(&out), "--catalog"),
        "`push --catalog` must be a known flag (TransferArgs); stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 8 (the quirk): `id` REJECTS `--catalog` — `id` carries only `WalkArgs`,
/// so the catalog selector is an unexpected argument (exit 2).
#[test]
fn id_rejects_catalog_flag() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let catalog = TempDir::new().unwrap();
    let cat = catalog.path().to_str().unwrap().to_owned();

    let out = run(&fx, &["id", "--catalog", &cat, &dir]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`id --catalog` must be rejected (exit 2); stderr:\n{}",
        stderr_of(&out)
    );
    assert!(
        is_unexpected_arg(&stderr_of(&out), "--catalog"),
        "`id --catalog` rejection must name the unexpected `--catalog`; stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 8 (the quirk, observable side): even when a catalog is AVAILABLE via
/// `SNAPDIR_CATALOG`, `id` never logs to it — the catalog stays empty.
#[test]
fn id_does_not_log_even_with_catalog_env() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let catalog = TempDir::new().unwrap();
    let cat = catalog
        .child("catalog.redb")
        .path()
        .to_str()
        .unwrap()
        .to_owned();

    let id_out = fx
        .cmd()
        .env("SNAPDIR_CATALOG", &cat)
        .args(["id", &dir])
        .output()
        .expect("run id with SNAPDIR_CATALOG");
    assert!(
        id_out.status.success(),
        "`id` with SNAPDIR_CATALOG must still exit 0; stderr:\n{}",
        stderr_of(&id_out)
    );

    let locs = run(&fx, &["locations", "--catalog", &cat]);
    assert!(
        locs.status.success(),
        "`locations --catalog` must succeed; stderr:\n{}",
        stderr_of(&locs)
    );
    assert_eq!(
        stdout_of(&locs).trim(),
        "",
        "`id` must NEVER log to the catalog; locations:\n{}",
        stdout_of(&locs)
    );
}

// ── 8b. Plumbing --store / SNAPDIR_STORE on the hidden wire commands ──────────

/// Clause 8: `objects-needed --store file://…` gets PAST arg parsing — the
/// hidden plumbing command carries `--store` via `PlumbingArgs`, so there is no
/// "unexpected argument --store". (Empty stdin → it answers nothing and exits
/// cleanly without blocking on a TTY.)
#[test]
fn objects_needed_accepts_store_flag() {
    let fx = Fixture::new();
    let (_store, store_uri) = file_store();

    let mut child = fx
        .cmd()
        .args(["objects-needed", "--store", &store_uri])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn objects-needed");
    // Empty id list: nothing is needed; the command must not hang.
    child.stdin.take().unwrap().write_all(b"").unwrap();
    let out = child.wait_with_output().expect("wait objects-needed");

    assert!(
        !is_unexpected_arg(&stderr_of(&out), "--store"),
        "`objects-needed --store` must be a known flag (PlumbingArgs); stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 8: `send-pack --store file://…` gets PAST arg parsing (`PlumbingArgs`);
/// no "unexpected argument --store". (`--ids -` with empty stdin keeps it from
/// blocking; any later error must NOT be the arg-parse rejection.)
#[test]
fn send_pack_accepts_store_flag() {
    let fx = Fixture::new();
    let (_store, store_uri) = file_store();

    let mut child = fx
        .cmd()
        .args(["send-pack", "--store", &store_uri, "--ids", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn send-pack");
    child.stdin.take().unwrap().write_all(b"").unwrap();
    let out = child.wait_with_output().expect("wait send-pack");

    assert!(
        !is_unexpected_arg(&stderr_of(&out), "--store"),
        "`send-pack --store` must be a known flag (PlumbingArgs); stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 8: `receive-pack --store file://…` gets PAST arg parsing
/// (`PlumbingArgs`); no "unexpected argument --store".
#[test]
fn receive_pack_accepts_store_flag() {
    let fx = Fixture::new();
    let (_store, store_uri) = file_store();

    let mut child = fx
        .cmd()
        .args(["receive-pack", "--store", &store_uri])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn receive-pack");
    child.stdin.take().unwrap().write_all(b"").unwrap();
    let out = child.wait_with_output().expect("wait receive-pack");

    assert!(
        !is_unexpected_arg(&stderr_of(&out), "--store"),
        "`receive-pack --store` must be a known flag (PlumbingArgs); stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 8: the plumbing `--store` also reads `SNAPDIR_STORE` — exporting the
/// env is accepted in lieu of the explicit flag (no missing-store / unexpected
/// arg failure attributable to the store selector).
#[test]
fn objects_needed_accepts_store_env() {
    let fx = Fixture::new();
    let (_store, store_uri) = file_store();

    let mut child = fx
        .cmd()
        .env("SNAPDIR_STORE", &store_uri)
        .args(["objects-needed"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn objects-needed (env store)");
    child.stdin.take().unwrap().write_all(b"").unwrap();
    let out = child.wait_with_output().expect("wait objects-needed");

    // Empty stdin + a valid env store: nothing is needed → clean exit.
    assert!(
        out.status.success(),
        "`SNAPDIR_STORE=… objects-needed` (empty stdin) must exit 0; stderr:\n{}",
        stderr_of(&out)
    );
}

// ── 8c. `sync --from` env fallback ────────────────────────────────────────────

/// Clause 8: with `SNAPDIR_STORE` set, `sync --to …` does NOT fail with
/// "--from required" — `--from` falls back to `$SNAPDIR_STORE`.
#[test]
fn sync_from_falls_back_to_env() {
    let fx = Fixture::new();
    let (_from, from_uri) = file_store();
    let (_to, to_uri) = file_store();

    let out = fx
        .cmd()
        .env("SNAPDIR_STORE", &from_uri)
        .args(["sync", "--to", &to_uri])
        .output()
        .expect("run sync with SNAPDIR_STORE");

    // Whatever the eventual outcome, it must NOT be the missing-`--from` parse
    // error: the env fallback supplied the source.
    let err = stderr_of(&out).to_lowercase();
    assert!(
        !(err.contains("--from") && (err.contains("required") || err.contains("provided"))),
        "`SNAPDIR_STORE` must satisfy `sync --from`; it must not demand `--from`; stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 8 (converse): with NEITHER `--from` nor `SNAPDIR_STORE`, `sync --to …`
/// DOES fail demanding `--from` (exit 2, the required-arg error).
#[test]
fn sync_from_required_without_env() {
    let fx = Fixture::new();
    let (_to, to_uri) = file_store();

    // Build the command WITHOUT inheriting an ambient SNAPDIR_STORE.
    let out = fx
        .cmd()
        .env_remove("SNAPDIR_STORE")
        .args(["sync", "--to", &to_uri])
        .output()
        .expect("run sync without store");

    assert_eq!(
        out.status.code(),
        Some(2),
        "`sync --to` with no `--from`/`SNAPDIR_STORE` must be a parse error (exit 2); stderr:\n{}",
        stderr_of(&out)
    );
    assert!(
        stderr_of(&out).to_lowercase().contains("--from"),
        "the rejection must name the required `--from`; stderr:\n{}",
        stderr_of(&out)
    );
}

// ── 8d. `--jobs` is a transfer flag: on push, not on id/manifest ──────────────

/// Clause 8: `--jobs` IS accepted on `push` (a transfer command, `TransferArgs`).
#[test]
fn push_accepts_jobs_flag() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();
    let (_store, store_uri) = file_store();

    let out = run(&fx, &["push", "--store", &store_uri, "--jobs", "2", &dir]);
    assert!(
        !is_unexpected_arg(&stderr_of(&out), "--jobs"),
        "`push --jobs` must be a known flag (TransferArgs); stderr:\n{}",
        stderr_of(&out)
    );
    // A plain `file://` push with two jobs should in fact succeed end-to-end.
    assert!(
        out.status.success(),
        "`push --store file://… --jobs 2 <dir>` must succeed; stderr:\n{}",
        stderr_of(&out)
    );
}

/// Clause 8 (converse): `--jobs` is REJECTED on `manifest` — a pure-walk command
/// carries no transfer concurrency knob (exit 2, unexpected argument).
#[test]
fn manifest_rejects_jobs_flag() {
    let fx = Fixture::new();
    let dir = fx.tree_path().to_str().unwrap().to_owned();

    let out = run(&fx, &["manifest", "--jobs", "2", &dir]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`manifest --jobs` must be rejected (exit 2); stderr:\n{}",
        stderr_of(&out)
    );
    assert!(
        is_unexpected_arg(&stderr_of(&out), "--jobs"),
        "`manifest --jobs` rejection must name the unexpected `--jobs`; stderr:\n{}",
        stderr_of(&out)
    );
}
