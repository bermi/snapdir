//! Integration tests for catalog logging on `manifest` and `stage`, mirroring
//! the oracle's `_snapdir_log_event` sites (`snapdir` L212 for `manifest`, L826
//! for `stage`; `push` is covered by `catalog_commands.rs`).
//!
//! The original Bash implementation logged to the catalog at three places:
//!   - `snapdir manifest <dir>` → `_snapdir_log_event "manifest" "$id"
//!     "$snapdir_dir_abs_path"` (location = the manifested dir's absolute path).
//!   - `snapdir push`          → already covered (gate cli-catalog-commands).
//!   - `snapdir stage <dir>`   → `_snapdir_log_event "stage" "$id" "$base_dir"`.
//!
//! `snapdir id` does NOT log (the oracle's `snapdir_id` at L223 has no
//! `_snapdir_log_event`), which `id_does_not_log_to_catalog` asserts.
//!
//! These tests confirm:
//!   - with a temp `--catalog`, `manifest`/`stage` record a revision at the
//!     directory's absolute path, surfaced by `locations` + `revisions
//!     --location=<abs dir>` (and `ancestors` chains a second revision).
//!   - `manifest`/`stage` stdout is byte-UNCHANGED by logging (catalog vs none).
//!   - the no-catalog case exits 0 and still prints the manifest/id.
//!
//! Everything lives under `assert_fs` temp dirs removed on drop, so the tests
//! never touch the user's real cache or catalog.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` command with the cache pinned under a temp dir so tests
/// never touch the user's real `$HOME/.cache/snapdir`. The catalog is passed
/// explicitly per call (via `--catalog`) so the no-catalog cases stay clean.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    // Ensure no ambient catalog leaks in from the environment.
    cmd.env_remove("SNAPDIR_CATALOG");
    cmd
}

/// Builds a tiny tree with explicit, deterministic permissions.
fn build_tree(dir: &Path, leaf: &str) {
    std::fs::write(dir.join("a.txt"), leaf).unwrap();
    std::fs::set_permissions(dir.join("a.txt"), PermissionsExt::from_mode(0o644)).unwrap();
    std::fs::set_permissions(dir, PermissionsExt::from_mode(0o755)).unwrap();
}

/// Runs `snapdir <args>` (cache pinned), asserts success, returns trimmed
/// stdout. Raw (untrimmed) stdout is available via `output_ok`.
fn stdout_ok(cache: &Path, args: &[&str]) -> String {
    let out = output_ok(cache, args);
    String::from_utf8(out).unwrap().trim_end().to_owned()
}

/// Runs `snapdir <args>` (cache pinned), asserts success, returns raw stdout
/// bytes (so byte-identity assertions are exact, including the trailing
/// newline).
fn output_ok(cache: &Path, args: &[&str]) -> Vec<u8> {
    let out = snapdir(cache).args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    out.stdout
}

/// Parses one JSON line's flat string fields (good enough for these shapes).
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

#[test]
fn catalog_logging_manifest_records_location_and_revision() {
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");
    let src = TempDir::new().unwrap();
    build_tree(src.path(), "hello");
    let src_str = src.path().to_string_lossy().into_owned();
    let catalog_str = catalog.path().to_string_lossy().into_owned();

    // `manifest` emits the manifest AND logs it at the dir's absolute path.
    stdout_ok(
        cache.path(),
        &["manifest", "--catalog", &catalog_str, &src_str],
    );

    // The logged id equals `snapdir id` of the same tree.
    let id = stdout_ok(cache.path(), &["id", &src_str]);
    assert_eq!(id.len(), 64, "id must be a 64-hex snapshot id: {id:?}");

    // `locations` lists the manifested directory's absolute path with that id.
    let locations = stdout_ok(cache.path(), &["locations", "--catalog", &catalog_str]);
    let loc_lines: Vec<&str> = locations.lines().collect();
    assert_eq!(loc_lines.len(), 1, "exactly one location: {locations:?}");
    assert_eq!(
        json_field(loc_lines[0], "location"),
        Some(src_str.as_str()),
        "location is the manifested dir's absolute path: {:?}",
        loc_lines[0]
    );
    assert_eq!(json_field(loc_lines[0], "id"), Some(id.as_str()));

    // `revisions --location=<abs dir>` shows the id (root: previous_id null).
    let revisions = stdout_ok(
        cache.path(),
        &[
            "revisions",
            "--catalog",
            &catalog_str,
            "--location",
            &src_str,
        ],
    );
    let rev_lines: Vec<&str> = revisions.lines().collect();
    assert_eq!(rev_lines.len(), 1, "exactly one revision: {revisions:?}");
    assert_eq!(json_field(rev_lines[0], "id"), Some(id.as_str()));
    assert_eq!(
        json_field(rev_lines[0], "previous_id"),
        Some("null"),
        "first revision's previous_id is null: {:?}",
        rev_lines[0]
    );
}

#[test]
fn catalog_logging_stage_records_location_and_revision() {
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");
    let src = TempDir::new().unwrap();
    build_tree(src.path(), "hello-stage");
    let src_str = src.path().to_string_lossy().into_owned();
    let catalog_str = catalog.path().to_string_lossy().into_owned();

    // `stage` caches the tree AND logs it at the staged base dir.
    let staged_id = stdout_ok(
        cache.path(),
        &["stage", "--catalog", &catalog_str, &src_str],
    );
    assert_eq!(
        staged_id.len(),
        64,
        "stage prints a 64-hex id: {staged_id:?}"
    );

    // `id` of the same tree matches the staged id.
    let id = stdout_ok(cache.path(), &["id", &src_str]);
    assert_eq!(id, staged_id, "staged id must equal `snapdir id`");

    // `locations` lists the staged base dir with that id.
    let locations = stdout_ok(cache.path(), &["locations", "--catalog", &catalog_str]);
    let loc_lines: Vec<&str> = locations.lines().collect();
    assert_eq!(loc_lines.len(), 1, "exactly one location: {locations:?}");
    assert_eq!(
        json_field(loc_lines[0], "location"),
        Some(src_str.as_str()),
        "location is the staged base dir: {:?}",
        loc_lines[0]
    );

    // `revisions --location=<base dir>` shows the staged id.
    let revisions = stdout_ok(
        cache.path(),
        &[
            "revisions",
            "--catalog",
            &catalog_str,
            "--location",
            &src_str,
        ],
    );
    let rev_lines: Vec<&str> = revisions.lines().collect();
    assert_eq!(rev_lines.len(), 1, "exactly one revision: {revisions:?}");
    assert_eq!(json_field(rev_lines[0], "id"), Some(staged_id.as_str()));
}

#[test]
fn catalog_logging_second_manifest_chains_revisions_and_ancestors() {
    // Two manifests of the SAME directory with changed content land two
    // revisions at that one location; `ancestors` walks the newer back to the
    // older, proving `manifest` logging exercises the catalog's save/chain path.
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");
    let src = TempDir::new().unwrap();
    let src_str = src.path().to_string_lossy().into_owned();
    let catalog_str = catalog.path().to_string_lossy().into_owned();

    build_tree(src.path(), "first");
    stdout_ok(
        cache.path(),
        &["manifest", "--catalog", &catalog_str, &src_str],
    );
    let id1 = stdout_ok(cache.path(), &["id", &src_str]);

    build_tree(src.path(), "second (changed)");
    stdout_ok(
        cache.path(),
        &["manifest", "--catalog", &catalog_str, &src_str],
    );
    let id2 = stdout_ok(cache.path(), &["id", &src_str]);
    assert_ne!(id1, id2, "changed content yields a distinct id");

    // Two revisions, newest first; the newest chains back to id1.
    let revisions = stdout_ok(
        cache.path(),
        &[
            "revisions",
            "--catalog",
            &catalog_str,
            "--location",
            &src_str,
        ],
    );
    let rev_lines: Vec<&str> = revisions.lines().collect();
    assert_eq!(rev_lines.len(), 2, "two revisions: {revisions:?}");
    assert_eq!(json_field(rev_lines[0], "id"), Some(id2.as_str()));
    assert_eq!(json_field(rev_lines[1], "id"), Some(id1.as_str()));
    assert_eq!(json_field(rev_lines[0], "previous_id"), Some(id1.as_str()));

    // `ancestors --id=<id2>` walks back to id1.
    let ancestors = stdout_ok(
        cache.path(),
        &["ancestors", "--catalog", &catalog_str, "--id", &id2],
    );
    let anc_lines: Vec<&str> = ancestors.lines().collect();
    assert_eq!(anc_lines.len(), 1, "one ancestor: {ancestors:?}");
    assert_eq!(json_field(anc_lines[0], "id"), Some(id1.as_str()));
}

#[test]
fn manifest_stdout_is_unchanged_by_catalog_logging() {
    // Logging is a side effect: the manifest stdout bytes must be identical
    // whether or not a catalog is configured.
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");
    let src = TempDir::new().unwrap();
    build_tree(src.path(), "byte-identical");
    let src_str = src.path().to_string_lossy().into_owned();
    let catalog_str = catalog.path().to_string_lossy().into_owned();

    let with_catalog = output_ok(
        cache.path(),
        &["manifest", "--catalog", &catalog_str, &src_str],
    );
    let without_catalog = output_ok(cache.path(), &["manifest", &src_str]);
    assert_eq!(
        with_catalog, without_catalog,
        "manifest stdout must be byte-identical with and without a catalog"
    );
}

#[test]
fn stage_stdout_is_unchanged_by_catalog_logging() {
    // The staged id printed to stdout is identical with/without a catalog.
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");
    let src = TempDir::new().unwrap();
    build_tree(src.path(), "stage-byte-identical");
    let src_str = src.path().to_string_lossy().into_owned();
    let catalog_str = catalog.path().to_string_lossy().into_owned();

    let with_catalog = output_ok(
        cache.path(),
        &["stage", "--catalog", &catalog_str, &src_str],
    );
    // Fresh cache for the no-catalog leg so neither run is affected by the
    // other's cached objects (the printed id is content-addressed regardless).
    let cache2 = TempDir::new().unwrap();
    let without_catalog = output_ok(cache2.path(), &["stage", &src_str]);
    assert_eq!(
        with_catalog, without_catalog,
        "stage stdout must be byte-identical with and without a catalog"
    );
}

#[test]
fn catalog_logging_manifest_without_catalog_exits_zero() {
    // No `--catalog` / `SNAPDIR_CATALOG`: logging is a silent no-op, the command
    // exits 0 and still prints the manifest (no error, no catalog file created).
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(src.path(), "no-catalog");
    let src_str = src.path().to_string_lossy().into_owned();

    let manifest = stdout_ok(cache.path(), &["manifest", &src_str]);
    assert!(
        manifest.contains("a.txt"),
        "manifest still printed without a catalog: {manifest:?}"
    );

    // And `id` works the same with no catalog.
    let id = stdout_ok(cache.path(), &["id", &src_str]);
    assert_eq!(id.len(), 64);
}

#[test]
fn catalog_logging_id_does_not_log() {
    // The oracle's `snapdir_id` (L223) has no `_snapdir_log_event`, so
    // `snapdir id <dir>` must NOT record anything in the catalog — only
    // `manifest`/`stage`/`push` do. With a catalog configured, `id` leaves
    // `locations` empty.
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");
    let src = TempDir::new().unwrap();
    build_tree(src.path(), "id-no-log");
    let src_str = src.path().to_string_lossy().into_owned();
    let catalog_str = catalog.path().to_string_lossy().into_owned();

    // `id` has no `--catalog` flag; offer the catalog via the env var instead so
    // the assertion is that `id` ignores an *available* catalog (it never logs).
    let id_out = snapdir(cache.path())
        .env("SNAPDIR_CATALOG", &catalog_str)
        .args(["id", &src_str])
        .output()
        .expect("run snapdir id");
    assert!(
        id_out.status.success(),
        "snapdir id failed: {}",
        String::from_utf8_lossy(&id_out.stderr)
    );
    let id = String::from_utf8(id_out.stdout)
        .unwrap()
        .trim_end()
        .to_owned();
    assert_eq!(id.len(), 64);

    let locations = stdout_ok(cache.path(), &["locations", "--catalog", &catalog_str]);
    assert_eq!(
        locations, "",
        "`id` must not log to the catalog (oracle's snapdir_id does not): {locations:?}"
    );
}
