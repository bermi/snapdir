//! Integration tests for the catalog query subcommands `locations`,
//! `ancestors`, and `revisions`, using `assert_cmd` + `assert_fs`.
//!
//! These exercise the *wired* catalog commands end-to-end: a real `push` to a
//! temp `file://` store (with a temp `--catalog` redb db) records the snapshot
//! in the catalog, and the three queries then emit the frozen CLI-compat JSON
//! lines (`snapdir_catalog::{locations,ancestors,revisions}_json_line`):
//!
//! - one `push` → `revisions --location=<store>` prints a single line whose `id`
//!   equals `snapdir id` and whose `previous_id` is `null` (the root revision);
//!   `locations` lists that store with the same id.
//! - a second `push` of a changed tree to the same store → `revisions` lists
//!   both ids (newest first, `created_at DESC`) and `ancestors --id=<new_id>`
//!   walks back to the previous id.
//! - an unknown location → empty output, exit 0.
//!
//! The JSON line shape (`id`/`previous_id`/`location`/`created_at` keys) is
//! pinned directly by the field assertions below.
//!
//! Everything lives under `assert_fs` temp dirs removed on drop, so the tests
//! are hermetic and never touch the user's real cache or catalog.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` command with the cache + catalog pinned under temp dirs so
/// tests never touch the user's real `$HOME/.cache/snapdir` or catalog.
fn snapdir(cache: &Path, catalog: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env("SNAPDIR_CATALOG", catalog);
    cmd
}

/// Builds a tiny tree with explicit, deterministic permissions.
fn build_tree(dir: &TempDir, leaf: &str) {
    dir.child("a.txt").write_str(leaf).unwrap();
    std::fs::set_permissions(dir.child("a.txt").path(), PermissionsExt::from_mode(0o644)).unwrap();
    std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o755)).unwrap();
}

/// Runs `snapdir <args>` (cache + catalog pinned), asserts success, returns
/// trimmed stdout.
fn stdout_ok(cache: &Path, catalog: &Path, args: &[&str]) -> String {
    let out = snapdir(cache, catalog)
        .args(args)
        .output()
        .expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// A `file://<dir>` store URI for the given store directory.
fn file_store(dir: &Path) -> String {
    format!("file://{}", dir.display())
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
        // null / non-string value: read until , or }.
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim())
    }
}

#[test]
fn catalog_commands_push_records_location_and_revision() {
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");
    let store_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src, "hello");
    let src_str = src.path().to_string_lossy().into_owned();
    let store = file_store(store_dir.path());

    // `push` returns the snapshot id and records it in the catalog at the store.
    let id = stdout_ok(
        cache.path(),
        catalog.path(),
        &["--store", &store, "push", &src_str],
    );
    assert_eq!(id.len(), 64, "push must print a 64-hex id: {id:?}");

    // `id` of the same tree matches the pushed id.
    let bare_id = stdout_ok(cache.path(), catalog.path(), &["id", &src_str]);
    assert_eq!(id, bare_id, "pushed id must equal `snapdir id`");

    // `revisions --location=<store>` lists that one revision (root: previous_id
    // null) with the pushed id.
    let revisions = stdout_ok(
        cache.path(),
        catalog.path(),
        &["revisions", "--location", &store],
    );
    let lines: Vec<&str> = revisions.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one revision: {revisions:?}");
    assert_eq!(json_field(lines[0], "id"), Some(id.as_str()));
    assert_eq!(
        json_field(lines[0], "previous_id"),
        Some("null"),
        "first revision's previous_id is null: {:?}",
        lines[0]
    );
    assert!(
        json_field(lines[0], "created_at").is_some(),
        "revision has a created_at: {:?}",
        lines[0]
    );

    // `locations` lists the store with that id.
    let locations = stdout_ok(cache.path(), catalog.path(), &["locations"]);
    let loc_lines: Vec<&str> = locations.lines().collect();
    assert_eq!(loc_lines.len(), 1, "exactly one location: {locations:?}");
    assert_eq!(json_field(loc_lines[0], "location"), Some(store.as_str()));
    assert_eq!(json_field(loc_lines[0], "id"), Some(id.as_str()));
}

#[test]
fn catalog_commands_second_push_lists_both_and_ancestors_walks_back() {
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");
    let store_dir = TempDir::new().unwrap();
    let store = file_store(store_dir.path());

    // First push.
    let src1 = TempDir::new().unwrap();
    build_tree(&src1, "first revision");
    let id1 = stdout_ok(
        cache.path(),
        catalog.path(),
        &["--store", &store, "push", &src1.path().to_string_lossy()],
    );

    // Second push of a changed tree to the SAME store.
    let src2 = TempDir::new().unwrap();
    build_tree(&src2, "second revision (changed)");
    let id2 = stdout_ok(
        cache.path(),
        catalog.path(),
        &["--store", &store, "push", &src2.path().to_string_lossy()],
    );
    assert_ne!(id1, id2, "the two trees must have distinct ids");

    // `revisions` lists both, newest first (created_at DESC).
    let revisions = stdout_ok(
        cache.path(),
        catalog.path(),
        &["revisions", "--location", &store],
    );
    let lines: Vec<&str> = revisions.lines().collect();
    assert_eq!(lines.len(), 2, "two revisions: {revisions:?}");
    assert_eq!(
        json_field(lines[0], "id"),
        Some(id2.as_str()),
        "newest revision first"
    );
    assert_eq!(json_field(lines[1], "id"), Some(id1.as_str()));
    // The newest revision's previous_id chains back to id1.
    assert_eq!(json_field(lines[0], "previous_id"), Some(id1.as_str()));

    // `ancestors --id=<new_id>` walks back to the previous id, reporting it in
    // the `id` field.
    let ancestors = stdout_ok(cache.path(), catalog.path(), &["ancestors", "--id", &id2]);
    let anc_lines: Vec<&str> = ancestors.lines().collect();
    assert_eq!(anc_lines.len(), 1, "one ancestor: {ancestors:?}");
    assert_eq!(
        json_field(anc_lines[0], "id"),
        Some(id1.as_str()),
        "ancestor id is the previous id"
    );
    assert_eq!(json_field(anc_lines[0], "location"), Some(store.as_str()));
}

#[test]
fn catalog_commands_unknown_location_is_empty_and_exits_zero() {
    let cache = TempDir::new().unwrap();
    let catalog = cache.child("catalog.redb");

    // No pushes yet: every query is empty and exits 0.
    let revisions = stdout_ok(
        cache.path(),
        catalog.path(),
        &["revisions", "--location", "file:///nope/does-not-exist"],
    );
    assert_eq!(revisions, "", "unknown location → empty revisions");

    let locations = stdout_ok(cache.path(), catalog.path(), &["locations"]);
    assert_eq!(locations, "", "empty catalog → empty locations");

    let ancestors = stdout_ok(
        cache.path(),
        catalog.path(),
        &["ancestors", "--id", &"0".repeat(64)],
    );
    assert_eq!(ancestors, "", "unknown id → empty ancestors");
}
