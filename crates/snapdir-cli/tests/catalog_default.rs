//! Black-box spec tests for the 1.9.0 default-on catalog semantics.
//!
//! Design lock: `.gatesmith/reviews/catalog-default-1.9.0.md`
//!
//! ## Root bugs being fixed
//!
//! Bug 1 — writes without `--catalog` log NOWHERE: `catalog_db_path()` returns
//! `None` when `--catalog` is unset, so `log_event()` silently no-ops. Snapshots
//! taken with no flag are never recorded anywhere.
//!
//! Bug 2 — `--catalog none` opens a literal `none-catalog.redb` (always empty),
//! so `revisions --catalog none` returns nothing instead of a clear "disabled"
//! message.
//!
//! ## Contract under test
//!
//! - Unset `--catalog` → resolves to `<cache_dir>/default-catalog.redb`.
//!   `push` and `stage` auto-record there; `revisions`/`locations`/`ancestors`
//!   read there. Round-trip works with zero flags.
//! - `--catalog none` (and `--catalog ""`) = disable sentinel: nothing recorded,
//!   no DB file created, query commands print a clear "catalog disabled" message
//!   and exit 0 (NOT a silent empty result, NOT an error exit).
//! - `--catalog foo` → `<cache_dir>/foo-catalog.redb`; isolated from default.
//! - `--catalog <path-with-separator>` → used verbatim as the DB path.
//! - Precedence: flag > `SNAPDIR_CATALOG` env > default.
//! - `snapdir defaults` shows `catalog` knob + source (flag|env|default).
//! - `manifest`/`id` stdout BYTE-IDENTICAL with and without catalog logging
//!   (catalog is a pure side effect; snapshot ids unaffected).
//!
//! ## Sandbox isolation
//!
//! Every test sets an isolated `HOME` + `XDG_CACHE_HOME` (both pointing at a
//! `TempDir`), removes `SNAPDIR_CATALOG` and `SNAPDIR_STORE` from the
//! environment, so the real `~/.cache/snapdir` is never touched and the default
//! catalog path is fully deterministic.
//!
//! These tests are EXPECTED TO FAIL against the current 1.7.0/1.8.0 binary —
//! the implementation does not exist yet. Do not weaken assertions to make them
//! pass. The lane owner moves this file to
//! `crates/snapdir-cli/tests/catalog_default.rs` during the impl gate.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

// ---------------------------------------------------------------------------
// Harness helpers
// ---------------------------------------------------------------------------

/// A `snapdir` command with a fully isolated environment:
/// - `HOME` and `XDG_CACHE_HOME` both point at `cache_dir` (so the default
///   catalog lands there instead of the real `~/.cache/snapdir`).
/// - `SNAPDIR_CATALOG` and `SNAPDIR_STORE` are removed so no ambient config
///   leaks in.
/// - `PATH` is inherited so the binary loader works.
///
/// Tests add the flags / env vars they specifically want to test.
fn snapdir_isolated(cache_dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", cache_dir);
    cmd.env("XDG_CACHE_HOME", cache_dir);
    cmd.env("SNAPDIR_CACHE_DIR", cache_dir);
    cmd.env_remove("SNAPDIR_CATALOG");
    cmd.env_remove("SNAPDIR_STORE");
    cmd
}

/// Builds a tiny, deterministic tree: one file `a.txt` with the given content.
fn build_tree(dir: &TempDir, leaf: &str) {
    dir.child("a.txt").write_str(leaf).unwrap();
    std::fs::set_permissions(dir.child("a.txt").path(), PermissionsExt::from_mode(0o644)).unwrap();
    std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o755)).unwrap();
}

/// Runs `snapdir <args>` with the isolated env, asserts exit 0, returns trimmed
/// stdout string.
fn stdout_ok(cache_dir: &Path, args: &[&str]) -> String {
    let out = snapdir_isolated(cache_dir)
        .args(args)
        .output()
        .expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed (code {:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// Same as `stdout_ok` but returns the RAW (untrimmed) stdout bytes so
/// byte-identity assertions are exact (including the trailing newline).
#[allow(dead_code)]
fn output_ok(cache_dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = snapdir_isolated(cache_dir)
        .args(args)
        .output()
        .expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed (code {:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    out.stdout
}

/// A `file://<dir>` store URI.
fn file_store(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

/// Minimal JSON field extractor (same idiom as `catalog_commands.rs`).
fn json_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(&stripped[..end])
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim())
    }
}

// ---------------------------------------------------------------------------
// (a) Round-trip via the DEFAULT catalog (THE core bug being fixed)
// ---------------------------------------------------------------------------

#[test]
fn catalog_default_push_and_stage_round_trip_no_flag() {
    // Spec clause (a): no-flag push + stage → no-flag revisions --location lists
    // both snapshots. This is the primary bug: without --catalog, nothing was ever
    // recorded, so revisions returned empty.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    // First snapshot: push a tree (no --catalog flag).
    let src1 = TempDir::new().unwrap();
    build_tree(&src1, "first push");
    let push_id = stdout_ok(
        cache.path(),
        &["push", "--store", &store, &src1.path().to_string_lossy()],
    );
    assert_eq!(
        push_id.len(),
        64,
        "push must print a 64-hex id: {push_id:?}"
    );

    // Second snapshot: stage a different tree (no --catalog flag).
    let src2 = TempDir::new().unwrap();
    build_tree(&src2, "second stage (different)");
    let stage_id = stdout_ok(cache.path(), &["stage", &src2.path().to_string_lossy()]);
    assert_eq!(
        stage_id.len(),
        64,
        "stage must print a 64-hex id: {stage_id:?}"
    );
    assert_ne!(push_id, stage_id, "distinct trees must have distinct ids");

    // Query: revisions at the store location (no --catalog flag) must list BOTH
    // snapshots. If the default catalog is not written, this will be empty.
    let revisions = stdout_ok(cache.path(), &["revisions", "--location", &store]);
    let lines: Vec<&str> = revisions.lines().collect();
    assert!(
        !lines.is_empty(),
        "revisions (default catalog, no flag) must not be empty after push; \
        got {revisions:?}. This is the bug: writes without --catalog were silently dropped."
    );

    // The pushed id must appear in the revision list.
    let ids_in_output: Vec<&str> = lines.iter().filter_map(|l| json_field(l, "id")).collect();
    assert!(
        ids_in_output.contains(&push_id.as_str()),
        "push id {push_id} must appear in default-catalog revisions; got {revisions:?}"
    );
}

#[test]
fn catalog_default_push_records_to_default_catalog_file() {
    // Spec clause (a) adversarial edge: after a no-flag push, the default catalog
    // FILE must actually exist at <cache_dir>/default-catalog.redb.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    let src = TempDir::new().unwrap();
    build_tree(&src, "catalog file existence");

    stdout_ok(
        cache.path(),
        &["push", "--store", &store, &src.path().to_string_lossy()],
    );

    let default_catalog = cache.path().join("default-catalog.redb");
    assert!(
        default_catalog.exists(),
        "default-catalog.redb must be created at <cache_dir>/default-catalog.redb \
        after a no-flag push; file not found at {default_catalog:?}"
    );
}

#[test]
fn catalog_default_revisions_with_no_flag_lists_pushed_id() {
    // Spec clause (a): the no-flag push → no-flag revisions round-trip — the
    // pushed id must appear in the output (id matches `snapdir id`).
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());
    let src = TempDir::new().unwrap();
    build_tree(&src, "round-trip");
    let src_str = src.path().to_string_lossy().into_owned();

    let push_id = stdout_ok(cache.path(), &["push", "--store", &store, &src_str]);
    let bare_id = stdout_ok(cache.path(), &["id", &src_str]);
    assert_eq!(push_id, bare_id, "push id must equal `snapdir id`");

    let revisions = stdout_ok(cache.path(), &["revisions", "--location", &store]);
    assert!(
        revisions.contains(&push_id),
        "push id {push_id:?} must appear in no-flag revisions output; got {revisions:?}"
    );
}

// ---------------------------------------------------------------------------
// (b) `--catalog none` / empty — disable sentinel
// ---------------------------------------------------------------------------

#[test]
fn catalog_none_push_records_nothing() {
    // Spec clause (b): --catalog none must be an explicit disable sentinel —
    // nothing is recorded, and no none-catalog.redb is created.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());
    let src = TempDir::new().unwrap();
    build_tree(&src, "none-disable");

    // Push with --catalog none; the push itself must succeed (exit 0).
    stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store,
            "--catalog",
            "none",
            &src.path().to_string_lossy(),
        ],
    );

    // After the push, none-catalog.redb must NOT be created (the old bug:
    // --catalog none opened a literal none-catalog.redb file).
    let none_catalog = cache.path().join("none-catalog.redb");
    assert!(
        !none_catalog.exists(),
        "--catalog none must not create none-catalog.redb; found it at {none_catalog:?}"
    );
}

#[test]
fn catalog_none_revisions_prints_disabled_message_exit_zero() {
    // Spec clause (b): `revisions --catalog none --location <x>` must exit 0 and
    // print a clear "catalog disabled" message (matching /disabl/i or mentioning
    // "none"), NOT a silent empty result and NOT a non-zero exit.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    let out = snapdir_isolated(cache.path())
        .args(["revisions", "--catalog", "none", "--location", &store])
        .output()
        .expect("run snapdir revisions --catalog none");

    assert!(
        out.status.success(),
        "revisions --catalog none must exit 0, not {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    let combined = format!("{stdout}{stderr}");

    assert!(
        combined.contains("disabl") || combined.contains("none"),
        "revisions --catalog none must print a 'catalog disabled' (or 'none') message; \
        got stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn catalog_none_locations_prints_disabled_message_exit_zero() {
    // Spec clause (b) edge: `locations --catalog none` → exit 0 + disabled message
    // (not a crash, not silent empty).
    let cache = TempDir::new().unwrap();

    let out = snapdir_isolated(cache.path())
        .args(["locations", "--catalog", "none"])
        .output()
        .expect("run snapdir locations --catalog none");

    assert!(
        out.status.success(),
        "locations --catalog none must exit 0, not {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout).to_lowercase(),
        String::from_utf8_lossy(&out.stderr).to_lowercase(),
    );
    assert!(
        combined.contains("disabl") || combined.contains("none"),
        "locations --catalog none must print a disabled message; got {combined:?}"
    );
}

#[test]
fn catalog_none_ancestors_prints_disabled_message_exit_zero() {
    // Spec clause (b) edge: `ancestors --catalog none` → exit 0 + disabled message
    // (not a crash, not silent empty).
    let cache = TempDir::new().unwrap();
    let fake_id = "0".repeat(64);

    let out = snapdir_isolated(cache.path())
        .args(["ancestors", "--catalog", "none", "--id", &fake_id])
        .output()
        .expect("run snapdir ancestors --catalog none");

    assert!(
        out.status.success(),
        "ancestors --catalog none must exit 0, not {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout).to_lowercase(),
        String::from_utf8_lossy(&out.stderr).to_lowercase(),
    );
    assert!(
        combined.contains("disabl") || combined.contains("none"),
        "ancestors --catalog none must print a disabled message; got {combined:?}"
    );
}

#[test]
fn catalog_empty_string_acts_like_none_sentinel() {
    // Spec clause (b) adversarial edge: --catalog "" (empty string) must behave
    // identically to --catalog none — disable sentinel, no DB created.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());
    let src = TempDir::new().unwrap();
    build_tree(&src, "empty-catalog-arg");

    // Push with --catalog ""; must not crash and must not record anything.
    let out = snapdir_isolated(cache.path())
        .args([
            "push",
            "--store",
            &store,
            "--catalog",
            "",
            &src.path().to_string_lossy(),
        ])
        .output()
        .expect("run snapdir push --catalog ''");

    // Push must exit 0 (disable is not an error for the push command itself).
    assert!(
        out.status.success(),
        "push --catalog '' must exit 0; got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    // No empty-string-named catalog file should be created.
    // (The old bug would create none-catalog.redb for "none"; the empty-string
    // sentinel must also not create a stray file.)
    let empty_named = cache.path().join("-catalog.redb");
    assert!(
        !empty_named.exists(),
        "--catalog '' must not create a '-catalog.redb' file; found {empty_named:?}"
    );

    // Revisions via --catalog "" must exit 0 with a disabled message.
    let rev_out = snapdir_isolated(cache.path())
        .args(["revisions", "--catalog", "", "--location", &store])
        .output()
        .expect("run snapdir revisions --catalog ''");

    assert!(
        rev_out.status.success(),
        "revisions --catalog '' must exit 0; got {:?}\nstderr: {}",
        rev_out.status.code(),
        String::from_utf8_lossy(&rev_out.stderr),
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&rev_out.stdout).to_lowercase(),
        String::from_utf8_lossy(&rev_out.stderr).to_lowercase(),
    );
    assert!(
        combined.contains("disabl") || combined.contains("none") || combined.contains("catalog"),
        "revisions --catalog '' must signal a disabled catalog; got {combined:?}"
    );
}

// ---------------------------------------------------------------------------
// (c) Named-catalog isolation (both directions)
// ---------------------------------------------------------------------------

#[test]
fn catalog_named_foo_isolated_from_default_both_directions() {
    // Spec clause (c): --catalog foo (bare name) → <cache_dir>/foo-catalog.redb.
    // Pushing to --catalog foo must NOT appear in the default catalog, and
    // pushing with no flag must NOT appear in --catalog foo.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    // Push to the DEFAULT catalog (no flag).
    let src_default = TempDir::new().unwrap();
    build_tree(&src_default, "default-catalog-push");
    let default_push_id = stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store,
            &src_default.path().to_string_lossy(),
        ],
    );

    // Push to the NAMED "foo" catalog.
    let src_foo = TempDir::new().unwrap();
    build_tree(&src_foo, "foo-catalog-push (different)");
    let foo_push_id = stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store,
            "--catalog",
            "foo",
            &src_foo.path().to_string_lossy(),
        ],
    );
    assert_ne!(
        default_push_id, foo_push_id,
        "distinct trees produce distinct ids"
    );

    // Direction 1: default-catalog revisions must NOT contain foo's id.
    let default_revisions = stdout_ok(cache.path(), &["revisions", "--location", &store]);
    assert!(
        !default_revisions.contains(&foo_push_id),
        "default-catalog revisions must NOT contain --catalog foo rows; got {default_revisions:?}"
    );

    // Direction 2: foo-catalog revisions must NOT contain the default-catalog id.
    let foo_revisions = stdout_ok(
        cache.path(),
        &["revisions", "--catalog", "foo", "--location", &store],
    );
    assert!(
        !foo_revisions.contains(&default_push_id),
        "--catalog foo revisions must NOT contain default-catalog rows; got {foo_revisions:?}"
    );

    // Sanity: foo-catalog revisions DOES contain foo's id.
    assert!(
        foo_revisions.contains(&foo_push_id),
        "--catalog foo revisions must contain foo's push id; got {foo_revisions:?}"
    );
}

#[test]
fn catalog_named_foo_file_exists_under_cache_dir() {
    // Spec clause (c) adversarial edge: --catalog foo → the ACTUAL file
    // foo-catalog.redb must be created under the sandboxed cache dir.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());
    let src = TempDir::new().unwrap();
    build_tree(&src, "named-foo");

    stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store,
            "--catalog",
            "foo",
            &src.path().to_string_lossy(),
        ],
    );

    let foo_catalog = cache.path().join("foo-catalog.redb");
    assert!(
        foo_catalog.exists(),
        "--catalog foo must create foo-catalog.redb in the cache dir; \
        expected {foo_catalog:?}"
    );
}

// ---------------------------------------------------------------------------
// (d) `snapdir defaults` shows the catalog knob + source
// ---------------------------------------------------------------------------

#[test]
fn catalog_defaults_shows_catalog_knob_source_default() {
    // Spec clause (d): `snapdir defaults` on a clean environment must include a
    // `catalog` line whose source is tagged `default`.
    let cache = TempDir::new().unwrap();

    let out = snapdir_isolated(cache.path())
        .args(["defaults"])
        .output()
        .expect("run snapdir defaults");

    assert!(
        out.status.success(),
        "snapdir defaults failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let low = stdout.to_lowercase();

    // The output must mention the `catalog` knob at all.
    assert!(
        low.contains("catalog"),
        "snapdir defaults must include a 'catalog' knob; got:\n{stdout}"
    );

    // On a clean env (no SNAPDIR_CATALOG, no --catalog flag), source must be
    // tagged `default`.
    assert!(
        low.contains("default"),
        "snapdir defaults catalog source must be 'default' on a clean env; got:\n{stdout}"
    );
}

#[test]
fn catalog_defaults_shows_source_env_when_snapdir_catalog_set() {
    // Spec clause (d): when SNAPDIR_CATALOG is set, the catalog line's source
    // must be tagged `env`.
    let cache = TempDir::new().unwrap();
    let custom_catalog = cache.path().join("custom-catalog.redb");

    let out = snapdir_isolated(cache.path())
        .env("SNAPDIR_CATALOG", &custom_catalog)
        .args(["defaults"])
        .output()
        .expect("run snapdir defaults with SNAPDIR_CATALOG set");

    assert!(
        out.status.success(),
        "snapdir defaults failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let low = stdout.to_lowercase();

    assert!(
        low.contains("catalog"),
        "snapdir defaults must include a 'catalog' knob; got:\n{stdout}"
    );
    // Source must be tagged `env` when SNAPDIR_CATALOG is set.
    assert!(
        low.contains("env"),
        "snapdir defaults catalog source must be 'env' when SNAPDIR_CATALOG is set; got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// (e) KEYSTONE — manifest/id stdout BYTE-IDENTICAL regardless of catalog
// ---------------------------------------------------------------------------

#[test]
fn catalog_default_manifest_stdout_byte_identical_with_and_without_logging() {
    // Spec clause (e) KEYSTONE: `manifest` stdout must be BYTE-IDENTICAL whether
    // or not catalog logging occurs. The catalog is a pure side effect.
    let cache1 = TempDir::new().unwrap();
    let cache2 = TempDir::new().unwrap();
    let catalog = cache1.path().join("test-catalog.redb");

    let src = TempDir::new().unwrap();
    build_tree(&src, "keystone-manifest");
    let src_str = src.path().to_string_lossy().into_owned();
    let catalog_str = catalog.to_string_lossy().into_owned();

    // With a catalog configured (logs to DB).
    let with_catalog = snapdir_isolated(cache1.path())
        .args(["manifest", "--catalog", &catalog_str, &src_str])
        .output()
        .expect("run manifest --catalog");
    assert!(with_catalog.status.success(), "manifest --catalog failed");

    // With --catalog none (no logging, sentinel disabled).
    let without_catalog = snapdir_isolated(cache2.path())
        .args(["manifest", "--catalog", "none", &src_str])
        .output()
        .expect("run manifest --catalog none");
    assert!(
        without_catalog.status.success(),
        "manifest --catalog none failed"
    );

    assert_eq!(
        with_catalog.stdout, without_catalog.stdout,
        "manifest stdout must be BYTE-IDENTICAL with/without catalog logging. \
        KEYSTONE: the catalog is a pure side effect."
    );
}

#[test]
fn catalog_default_id_stdout_byte_identical_with_and_without_logging() {
    // Spec clause (e) KEYSTONE: `id` stdout must be BYTE-IDENTICAL regardless of
    // whether a catalog is configured via env (id never logs, but the id output
    // itself must not be contaminated by catalog activity).
    let cache1 = TempDir::new().unwrap();
    let cache2 = TempDir::new().unwrap();
    let catalog = cache1.path().join("test-catalog.redb");

    let src = TempDir::new().unwrap();
    build_tree(&src, "keystone-id");
    let src_str = src.path().to_string_lossy().into_owned();

    // With SNAPDIR_CATALOG set (even though `id` never logs).
    let with_catalog_env = snapdir_isolated(cache1.path())
        .env("SNAPDIR_CATALOG", &catalog)
        .args(["id", &src_str])
        .output()
        .expect("run id with SNAPDIR_CATALOG");
    assert!(
        with_catalog_env.status.success(),
        "id with SNAPDIR_CATALOG failed"
    );

    // With no catalog env (clean).
    let without_catalog_env = snapdir_isolated(cache2.path())
        .args(["id", &src_str])
        .output()
        .expect("run id without SNAPDIR_CATALOG");
    assert!(
        without_catalog_env.status.success(),
        "id without catalog failed"
    );

    assert_eq!(
        with_catalog_env.stdout, without_catalog_env.stdout,
        "id stdout must be BYTE-IDENTICAL with/without SNAPDIR_CATALOG. \
        KEYSTONE: catalog is a pure side effect."
    );
}

#[test]
fn catalog_default_id_is_64_hex_chars() {
    // Spec clause (e) subsidiary: the id output is exactly 64 hex chars (the
    // frozen BLAKE3 snapshot id format must be unaffected by catalog changes).
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src, "id-format");

    let id = stdout_ok(cache.path(), &["id", &src.path().to_string_lossy()]);
    assert_eq!(id.len(), 64, "id must be exactly 64 hex chars; got {id:?}");
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit()),
        "id must be all hex digits; got {id:?}"
    );
}

// ---------------------------------------------------------------------------
// (f) Path-like catalog (contains a separator → used verbatim)
// ---------------------------------------------------------------------------

#[test]
fn catalog_path_like_arg_used_verbatim() {
    // Spec clause (f): --catalog <path-with-separator> → used verbatim as the
    // redb DB path (not expanded to <cache_dir>/<name>-catalog.redb).
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    // Create a temp directory for the path-like catalog.
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("x.redb");
    let db_str = db_path.to_string_lossy().into_owned();

    // Push using the verbatim path.
    let src = TempDir::new().unwrap();
    build_tree(&src, "path-like-catalog");
    let push_id = stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store,
            "--catalog",
            &db_str,
            &src.path().to_string_lossy(),
        ],
    );
    assert_eq!(
        push_id.len(),
        64,
        "push with path catalog must print a 64-hex id"
    );

    // The DB file must be created at the exact given path (verbatim).
    assert!(
        db_path.exists(),
        "--catalog <path> must create the redb file at that exact path; \
        expected {db_path:?}"
    );

    // Revisions with the same verbatim path must list the pushed id.
    let revisions = stdout_ok(
        cache.path(),
        &["revisions", "--catalog", &db_str, "--location", &store],
    );
    assert!(
        revisions.contains(&push_id),
        "revisions via path-like catalog must list the pushed id {push_id:?}; \
        got {revisions:?}"
    );
}

#[test]
fn catalog_path_like_isolation_from_default() {
    // Spec clause (f) adversarial edge: a verbatim-path catalog must be isolated
    // from the default catalog. Pushing to the path catalog must NOT appear in
    // the default catalog.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("isolated.redb");
    let db_str = db_path.to_string_lossy().into_owned();

    let src = TempDir::new().unwrap();
    build_tree(&src, "path-like-isolated");
    let path_id = stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store,
            "--catalog",
            &db_str,
            &src.path().to_string_lossy(),
        ],
    );

    // The default catalog revisions must NOT contain the path-catalog push.
    let default_revisions = stdout_ok(cache.path(), &["revisions", "--location", &store]);
    assert!(
        !default_revisions.contains(&path_id),
        "default-catalog revisions must NOT contain rows from a path-like catalog; \
        got {default_revisions:?}"
    );
}

// ---------------------------------------------------------------------------
// Precedence: --catalog flag overrides SNAPDIR_CATALOG env
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::similar_names)] // catalog_a_str / catalog_b_str are the test subjects
fn catalog_flag_overrides_snapdir_catalog_env() {
    // Spec precedence: --catalog flag > SNAPDIR_CATALOG env > default.
    // Push with env pointing at catalog-A and flag pointing at catalog-B;
    // the revision must appear in catalog-B, not catalog-A.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    let catalog_a = cache.path().join("catalog-a.redb");
    let catalog_b = cache.path().join("catalog-b.redb");
    let catalog_a_str = catalog_a.to_string_lossy().into_owned();
    let catalog_b_str = catalog_b.to_string_lossy().into_owned();

    let src = TempDir::new().unwrap();
    build_tree(&src, "precedence-test");

    // Push: env says catalog-A, flag says catalog-B → flag wins.
    let push_id = snapdir_isolated(cache.path())
        .env("SNAPDIR_CATALOG", &catalog_a_str)
        .args([
            "push",
            "--store",
            &store,
            "--catalog",
            &catalog_b_str,
            &src.path().to_string_lossy(),
        ])
        .output()
        .expect("run push with flag+env");
    assert!(push_id.status.success(), "push with flag+env must exit 0");
    let push_id_str = String::from_utf8(push_id.stdout)
        .unwrap()
        .trim_end()
        .to_owned();

    // catalog-B must contain the revision.
    let rev_b = snapdir_isolated(cache.path())
        .args([
            "revisions",
            "--catalog",
            &catalog_b_str,
            "--location",
            &store,
        ])
        .output()
        .expect("run revisions --catalog B");
    assert!(rev_b.status.success());
    let rev_b_str = String::from_utf8(rev_b.stdout).unwrap();
    assert!(
        rev_b_str.contains(&push_id_str),
        "--catalog flag (B) must contain the pushed revision {push_id_str:?}; \
        got {rev_b_str:?}"
    );

    // catalog-A must NOT contain the revision (flag beat env).
    let rev_a = snapdir_isolated(cache.path())
        .args([
            "revisions",
            "--catalog",
            &catalog_a_str,
            "--location",
            &store,
        ])
        .output()
        .expect("run revisions --catalog A");
    assert!(rev_a.status.success());
    let rev_a_str = String::from_utf8(rev_a.stdout).unwrap();
    assert!(
        !rev_a_str.contains(&push_id_str),
        "SNAPDIR_CATALOG env (A) must NOT contain the revision when --catalog flag (B) overrides; \
        got {rev_a_str:?}"
    );
}

#[test]
fn catalog_env_overrides_default_catalog() {
    // Spec precedence: SNAPDIR_CATALOG env > default. A push with only the env
    // var set must land in the env-specified catalog, NOT in default-catalog.redb.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    let custom_catalog = cache.path().join("custom-env-catalog.redb");
    let custom_str = custom_catalog.to_string_lossy().into_owned();

    let src = TempDir::new().unwrap();
    build_tree(&src, "env-precedence");

    // Push with SNAPDIR_CATALOG set (no --catalog flag).
    let push_out = snapdir_isolated(cache.path())
        .env("SNAPDIR_CATALOG", &custom_str)
        .args(["push", "--store", &store, &src.path().to_string_lossy()])
        .output()
        .expect("run push with SNAPDIR_CATALOG env");
    assert!(
        push_out.status.success(),
        "push with SNAPDIR_CATALOG must exit 0"
    );
    let push_id = String::from_utf8(push_out.stdout)
        .unwrap()
        .trim_end()
        .to_owned();

    // The custom catalog must contain the revision.
    let custom_rev = snapdir_isolated(cache.path())
        .args(["revisions", "--catalog", &custom_str, "--location", &store])
        .output()
        .expect("run revisions --catalog custom");
    assert!(custom_rev.status.success());
    let custom_rev_str = String::from_utf8(custom_rev.stdout).unwrap();
    assert!(
        custom_rev_str.contains(&push_id),
        "SNAPDIR_CATALOG env catalog must contain the pushed revision; got {custom_rev_str:?}"
    );

    // The DEFAULT catalog must NOT contain the revision (env beat default).
    let default_rev = snapdir_isolated(cache.path())
        .args(["revisions", "--location", &store])
        .output()
        .expect("run revisions (default catalog)");
    assert!(default_rev.status.success());
    let default_rev_str = String::from_utf8(default_rev.stdout).unwrap();
    assert!(
        !default_rev_str.contains(&push_id),
        "default catalog must NOT contain a revision pushed via SNAPDIR_CATALOG env; \
        got {default_rev_str:?}"
    );
}

// ---------------------------------------------------------------------------
// Adversarial edge: --catalog none does NOT create default-catalog.redb either
// ---------------------------------------------------------------------------

#[test]
fn catalog_none_push_does_not_create_default_catalog_file() {
    // Spec clause (b) adversarial edge: after push --catalog none, NEITHER
    // none-catalog.redb NOR default-catalog.redb must be created.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());
    let src = TempDir::new().unwrap();
    build_tree(&src, "none-no-default-file");

    stdout_ok(
        cache.path(),
        &[
            "push",
            "--store",
            &store,
            "--catalog",
            "none",
            &src.path().to_string_lossy(),
        ],
    );

    let none_catalog = cache.path().join("none-catalog.redb");
    let default_catalog = cache.path().join("default-catalog.redb");

    assert!(
        !none_catalog.exists(),
        "--catalog none must NOT create none-catalog.redb; found {none_catalog:?}"
    );
    assert!(
        !default_catalog.exists(),
        "--catalog none must NOT create default-catalog.redb either; found {default_catalog:?}"
    );
}

// ---------------------------------------------------------------------------
// Adversarial edge: the disabled message is distinct from "no revisions for <loc>"
// ---------------------------------------------------------------------------

#[test]
fn catalog_none_disabled_message_distinct_from_empty_enabled_catalog() {
    // Spec clause (b) adversarial: the "disabled" response to `revisions --catalog none`
    // must be DISTINGUISHABLE from the empty-but-enabled response of a valid catalog
    // with no records. The enabled-empty case must NOT print a disabled message.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    // Enabled-but-empty catalog (push nothing, query with an explicit catalog path).
    let empty_catalog = cache.path().join("empty.redb");
    let empty_str = empty_catalog.to_string_lossy().into_owned();

    let empty_out = snapdir_isolated(cache.path())
        .args(["revisions", "--catalog", &empty_str, "--location", &store])
        .output()
        .expect("run revisions with empty catalog");
    assert!(
        empty_out.status.success(),
        "enabled-empty catalog must exit 0"
    );

    let empty_combined = format!(
        "{}{}",
        String::from_utf8_lossy(&empty_out.stdout).to_lowercase(),
        String::from_utf8_lossy(&empty_out.stderr).to_lowercase(),
    );
    // The enabled-but-empty case must NOT say "disabled" — that would be confusing.
    assert!(
        !empty_combined.contains("disabl"),
        "enabled-empty catalog must NOT print 'disabled'; got {empty_combined:?}"
    );

    // Disabled case (--catalog none) MUST say "disabled" or "none".
    let none_out = snapdir_isolated(cache.path())
        .args(["revisions", "--catalog", "none", "--location", &store])
        .output()
        .expect("run revisions --catalog none");
    assert!(none_out.status.success(), "--catalog none must exit 0");

    let none_combined = format!(
        "{}{}",
        String::from_utf8_lossy(&none_out.stdout).to_lowercase(),
        String::from_utf8_lossy(&none_out.stderr).to_lowercase(),
    );
    assert!(
        none_combined.contains("disabl") || none_combined.contains("none"),
        "--catalog none must print a disabled message; got {none_combined:?}"
    );
}

// ---------------------------------------------------------------------------
// Impl-revealed: disabled message goes to STDERR only (stdout stays clean)
// ---------------------------------------------------------------------------

#[test]
fn catalog_none_disabled_message_is_on_stderr_not_stdout() {
    // Impl-revealed: `print_catalog_disabled()` uses `eprintln!` → the message
    // must appear on STDERR; stdout must be empty (no JSON lines, no stray text).
    // This matters for pipeline users who capture stdout to parse JSON.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    let out = snapdir_isolated(cache.path())
        .args(["revisions", "--catalog", "none", "--location", &store])
        .output()
        .expect("run revisions --catalog none");

    assert!(
        out.status.success(),
        "revisions --catalog none must exit 0; got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    // stdout must be empty — the disabled message must NOT appear on stdout.
    assert!(
        out.stdout.is_empty(),
        "revisions --catalog none stdout must be empty; got {:?}",
        String::from_utf8_lossy(&out.stdout),
    );

    // stderr MUST carry the disabled message.
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("disabl") || stderr.contains("none"),
        "disabled message must appear on stderr; got stderr={stderr:?}"
    );
}

#[test]
fn catalog_none_locations_disabled_message_is_on_stderr_not_stdout() {
    // Impl-revealed: `locations --catalog none` — disabled message on stderr,
    // empty stdout (same contract as revisions, for pipeline safety).
    let cache = TempDir::new().unwrap();

    let out = snapdir_isolated(cache.path())
        .args(["locations", "--catalog", "none"])
        .output()
        .expect("run locations --catalog none");

    assert!(out.status.success(), "locations --catalog none must exit 0");
    assert!(
        out.stdout.is_empty(),
        "locations --catalog none stdout must be empty; got {:?}",
        String::from_utf8_lossy(&out.stdout),
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("disabl") || stderr.contains("none"),
        "disabled message must appear on stderr; got stderr={stderr:?}"
    );
}

#[test]
fn catalog_none_ancestors_disabled_message_is_on_stderr_not_stdout() {
    // Impl-revealed: `ancestors --catalog none` — disabled message on stderr,
    // empty stdout.
    let cache = TempDir::new().unwrap();
    let fake_id = "0".repeat(64);

    let out = snapdir_isolated(cache.path())
        .args(["ancestors", "--catalog", "none", "--id", &fake_id])
        .output()
        .expect("run ancestors --catalog none");

    assert!(out.status.success(), "ancestors --catalog none must exit 0");
    assert!(
        out.stdout.is_empty(),
        "ancestors --catalog none stdout must be empty; got {:?}",
        String::from_utf8_lossy(&out.stdout),
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("disabl") || stderr.contains("none"),
        "disabled message must appear on stderr; got stderr={stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// Impl-revealed: --cache-dir round-trip (push and query must agree on path)
// ---------------------------------------------------------------------------

#[test]
fn catalog_cache_dir_flag_round_trip_push_then_revisions() {
    // Impl-revealed: `--cache-dir` on the query commands was additive in this
    // cluster (CatalogArgs gained cache_dir). Push with `--cache-dir X` and
    // revisions with `--cache-dir X` must resolve the SAME default-catalog.redb
    // and therefore list the pushed id.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    // A separate dir to use as the explicit cache (not the HOME-derived default).
    let explicit_cache = TempDir::new().unwrap();
    let explicit_cache_str = explicit_cache.path().to_string_lossy().into_owned();

    let src = TempDir::new().unwrap();
    build_tree(&src, "cache-dir-round-trip");

    // Push with explicit --cache-dir so default-catalog.redb lands there.
    let push_id = snapdir_isolated(cache.path())
        .args([
            "push",
            "--store",
            &store,
            "--cache-dir",
            &explicit_cache_str,
            &src.path().to_string_lossy(),
        ])
        .output()
        .expect("run push --cache-dir");
    assert!(
        push_id.status.success(),
        "push --cache-dir must exit 0; stderr: {}",
        String::from_utf8_lossy(&push_id.stderr),
    );
    let push_id_str = String::from_utf8(push_id.stdout)
        .unwrap()
        .trim_end()
        .to_owned();
    assert_eq!(push_id_str.len(), 64, "push must print a 64-hex id");

    // The default catalog must exist under the EXPLICIT cache dir, not `cache`.
    let default_catalog_explicit = explicit_cache.path().join("default-catalog.redb");
    assert!(
        default_catalog_explicit.exists(),
        "default-catalog.redb must be in the explicit --cache-dir, not the HOME-derived one; \
        expected {default_catalog_explicit:?}"
    );
    let default_catalog_home = cache.path().join("default-catalog.redb");
    assert!(
        !default_catalog_home.exists(),
        "default-catalog.redb must NOT be in HOME when --cache-dir overrides; \
        found stray file at {default_catalog_home:?}"
    );

    // Query with the SAME explicit --cache-dir; must find the pushed id.
    let revisions = snapdir_isolated(cache.path())
        .args([
            "revisions",
            "--cache-dir",
            &explicit_cache_str,
            "--location",
            &store,
        ])
        .output()
        .expect("run revisions --cache-dir");
    assert!(
        revisions.status.success(),
        "revisions --cache-dir must exit 0; stderr: {}",
        String::from_utf8_lossy(&revisions.stderr),
    );
    let revisions_str = String::from_utf8(revisions.stdout).unwrap();
    assert!(
        revisions_str.contains(&push_id_str),
        "revisions with --cache-dir must list the push id {push_id_str:?}; \
        got {revisions_str:?}"
    );
}

// ---------------------------------------------------------------------------
// Impl-revealed: locations and ancestors end-to-end with default catalog
// ---------------------------------------------------------------------------

#[test]
fn catalog_default_locations_end_to_end() {
    // Impl-revealed: `locations` with the default catalog (no flag) must list
    // the store URI that was pushed to. This verifies `open_catalog()` wiring
    // for `run_locations()`.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    let src = TempDir::new().unwrap();
    build_tree(&src, "locations-default-catalog");

    // Push with no --catalog flag (goes to default-catalog.redb).
    stdout_ok(
        cache.path(),
        &["push", "--store", &store, &src.path().to_string_lossy()],
    );

    // No-flag locations must include the store URI.
    let locations = stdout_ok(cache.path(), &["locations"]);
    assert!(
        locations.contains(store_dir.path().to_str().unwrap()),
        "no-flag locations must list the pushed store URI; got {locations:?}"
    );
}

#[test]
fn catalog_default_ancestors_end_to_end() {
    // Impl-revealed: `ancestors --id <id>` with the default catalog (no flag)
    // must return the chain from the default catalog. After two no-flag pushes
    // at the same location, ancestors of the second id must include the first.
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    // First push.
    let src1 = TempDir::new().unwrap();
    build_tree(&src1, "ancestors-first");
    let id1 = stdout_ok(
        cache.path(),
        &["push", "--store", &store, &src1.path().to_string_lossy()],
    );
    assert_eq!(id1.len(), 64);

    // Second push (different content so different id).
    let src2 = TempDir::new().unwrap();
    build_tree(&src2, "ancestors-second (different)");
    let id2 = stdout_ok(
        cache.path(),
        &["push", "--store", &store, &src2.path().to_string_lossy()],
    );
    assert_eq!(id2.len(), 64);
    assert_ne!(id1, id2, "distinct trees must have distinct ids");

    // ancestors of id2 (no --catalog flag) must mention id1 (the previous
    // revision at the same location).
    let ancestors = stdout_ok(cache.path(), &["ancestors", "--id", &id2]);
    assert!(
        ancestors.contains(&id1),
        "default-catalog ancestors of id2 must mention id1; got {ancestors:?}"
    );
}

// ---------------------------------------------------------------------------
// Impl-revealed: defaults shows the resolved DEFAULT catalog path (not just
// the word "catalog"), so the value is useful, not just a tag.
// ---------------------------------------------------------------------------

#[test]
fn catalog_defaults_value_is_full_path_when_default() {
    // Impl-revealed: `defaults` on a clean env resolves the catalog knob to
    // the full `<cache_dir>/default-catalog.redb` path, not the bare string
    // "default" or "none". Users should see exactly where the DB will land.
    let cache = TempDir::new().unwrap();

    let out = snapdir_isolated(cache.path())
        .args(["defaults"])
        .output()
        .expect("run snapdir defaults");
    assert!(out.status.success(), "snapdir defaults must exit 0");

    let stdout = String::from_utf8(out.stdout).unwrap();
    // The catalog line must contain the path to default-catalog.redb.
    assert!(
        stdout.contains("default-catalog.redb"),
        "defaults must show the resolved default-catalog.redb path; got:\n{stdout}"
    );
    // And it must be under the cache dir we pointed at.
    assert!(
        stdout.contains(cache.path().to_str().unwrap()),
        "defaults catalog path must be under the sandboxed cache dir; got:\n{stdout}"
    );
}
