//! Integration tests for PATH-argument normalization.
//!
//! The directory PATH argument must be normalized so that the four surface
//! forms `foo`, `./foo`, `foo/`, and `./foo/` all produce the IDENTICAL
//! manifest and snapshot id. The fix lives entirely in the CLI's `resolve_root`
//! (lexical normalization of the resolved absolute root); the frozen
//! `snapdir-core` walk/merkle contract is untouched.
//!
//! These drive the compiled `snapdir` binary, running each form from the PARENT
//! directory (via `current_dir`) so the relative form is meaningful, and assert
//! four-way byte-equality of `id` and `manifest` output, spec-conformance of the
//! relative manifest, four-way equality through a `file://` push round-trip, and
//! a pinned blake3 id for the canonical `foo` form (invariant guard).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the compiled `snapdir` binary under test.
///
/// The bin target lives in the `snapdir` crate (`crates/snapdir`), so
/// `CARGO_BIN_EXE_snapdir` is not set for snapdir-cli tests; `assert_cmd`'s
/// lookup falls back to the shared target dir. Under `cargo test --workspace`
/// the binary is always built first; for a standalone
/// `cargo test -p snapdir-cli`, run `cargo build -p snapdir` once before.
fn snapdir_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// Creates a unique temp directory and returns its path.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-cli-pathnorm-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Builds a `<parent>/foo/` subtree with nested files+dirs and fixed perms so
/// the manifest is fully deterministic:
///
/// ```text
/// foo/a.txt        ("hello", 0o644)
/// foo/bar.txt      ("",      0o644)
/// foo/sub/         (0o755)
/// foo/sub/b.txt    ("world!!", 0o644)
/// ```
fn build_foo_tree(parent: &Path) -> PathBuf {
    let foo_dir = parent.join("foo");
    fs::create_dir(&foo_dir).unwrap();
    fs::write(foo_dir.join("a.txt"), b"hello").unwrap();
    fs::write(foo_dir.join("bar.txt"), b"").unwrap();
    fs::create_dir(foo_dir.join("sub")).unwrap();
    fs::write(foo_dir.join("sub").join("b.txt"), b"world!!").unwrap();
    for (rel, mode) in [
        ("a.txt", 0o644),
        ("bar.txt", 0o644),
        ("sub/b.txt", 0o644),
        ("sub", 0o755),
    ] {
        fs::set_permissions(foo_dir.join(rel), fs::Permissions::from_mode(mode)).unwrap();
    }
    fs::set_permissions(&foo_dir, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).unwrap();
    foo_dir
}

/// The four equivalent surface forms of the same `foo` subdir.
const FORMS: [&str; 4] = ["foo", "./foo", "foo/", "./foo/"];

/// Runs `snapdir <args>` from `cwd`, asserting success and returning stdout
/// verbatim (no trimming — byte-equality matters).
fn run_from(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(snapdir_bin());
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("run snapdir");
    assert!(
        output.status.success(),
        "snapdir {args:?} (cwd={}) exited with {:?}\nstderr: {}",
        cwd.display(),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// (1) `id foo` == `id ./foo` == `id foo/` == `id ./foo/` — all four byte-equal,
/// each invoked from the parent directory.
#[test]
fn path_normalize_id_four_forms_are_byte_equal() {
    let parent = temp_dir("id");
    build_foo_tree(&parent);

    let mut ids = Vec::new();
    for form in FORMS {
        let id = run_from(&parent, &["id", form], &[]);
        let id = id.trim_end().to_owned();
        assert_eq!(
            id.len(),
            64,
            "snapshot id should be 64 hex chars (form {form:?})"
        );
        ids.push(id);
    }
    for form_id in &ids[1..] {
        assert_eq!(&ids[0], form_id, "all four id forms must match: {ids:?}");
    }
    fs::remove_dir_all(&parent).ok();
}

/// (2) The `manifest` output is byte-identical across the four forms AND
/// spec-conformant: root line is `D ... ./`, no absolute-path leakage, every
/// relative entry starts with `./` (no `.bar` artifact).
#[test]
fn path_normalize_manifest_four_forms_byte_equal_and_spec_conformant() {
    let parent = temp_dir("manifest");
    build_foo_tree(&parent);
    let parent_str = parent.to_string_lossy().into_owned();

    let mut outputs = Vec::new();
    for form in FORMS {
        outputs.push(run_from(&parent, &["manifest", form], &[]));
    }
    for (i, out) in outputs.iter().enumerate().skip(1) {
        assert_eq!(
            outputs[0], *out,
            "manifest must be byte-identical across forms (diff at {:?})",
            FORMS[i]
        );
    }

    // Spec-conformance of the relative manifest (any form; they're equal).
    let manifest = &outputs[0];
    let lines: Vec<&str> = manifest.lines().collect();
    assert!(!lines.is_empty(), "manifest must not be empty");

    // Root line: `D ... ./`.
    let (head, root_path) = lines[0].rsplit_once(' ').unwrap();
    assert!(
        head.starts_with("D "),
        "root line must be a directory: {:?}",
        lines[0]
    );
    assert_eq!(
        root_path, "./",
        "root entry path must be `./`: {:?}",
        lines[0]
    );

    for line in &lines {
        let (_head, path) = line.rsplit_once(' ').unwrap();
        // No absolute-path leakage (e.g. the temp parent prefix).
        assert!(
            !path.starts_with('/'),
            "relative manifest must not leak absolute paths: {line:?}"
        );
        assert!(
            !path.starts_with(&parent_str),
            "relative manifest must not leak the parent prefix: {line:?}"
        );
        // Every relative entry starts with `./` — no `.bar` trailing-slash artifact.
        assert!(
            path.starts_with("./"),
            "every relative entry must start with `./`: {line:?}"
        );
    }
    // Specifically, the trailing-slash bug produced `./.bar.txt` → `.bar.txt`;
    // assert the proper form is present.
    assert!(
        manifest.contains(" ./bar.txt\n") || manifest.ends_with(" ./bar.txt"),
        "bar.txt must render as `./bar.txt`, not a `.bar.txt` artifact:\n{manifest}"
    );

    fs::remove_dir_all(&parent).ok();
}

/// `--absolute` mode is also consistent across the four forms: identical
/// normalized absolute prefix, no `/./` artifact.
#[test]
fn path_normalize_absolute_four_forms_byte_equal() {
    let parent = temp_dir("abs");
    build_foo_tree(&parent);

    let mut outputs = Vec::new();
    for form in FORMS {
        outputs.push(run_from(&parent, &["manifest", "--absolute", form], &[]));
    }
    for (i, out) in outputs.iter().enumerate().skip(1) {
        assert_eq!(
            outputs[0], *out,
            "--absolute manifest must be byte-identical across forms (diff at {:?})",
            FORMS[i]
        );
    }
    // No `/./` artifact in the absolute paths.
    assert!(
        !outputs[0].contains("/./"),
        "--absolute manifest must not contain a `/./` artifact:\n{}",
        outputs[0]
    );
    fs::remove_dir_all(&parent).ok();
}

/// (3) The four-way equality holds through a `file://` push → re-id round-trip:
/// push each form to a fresh store, then re-`id` the source via each form;
/// all printed ids (push + re-id) are identical.
#[test]
fn path_normalize_push_reid_four_forms_byte_equal() {
    let parent = temp_dir("push-parent");
    build_foo_tree(&parent);
    let cache = temp_dir("push-cache");
    let cache_str = cache.to_string_lossy().into_owned();

    let mut ids = Vec::new();
    for form in FORMS {
        // A fresh store per form keeps the forms independent.
        let store = temp_dir("push-store");
        let store_url = format!("file://{}", store.display());

        let pushed = run_from(
            &parent,
            &["push", "--store", &store_url, form],
            &[("SNAPDIR_CACHE_DIR", &cache_str)],
        );
        let pushed = pushed.trim_end().to_owned();
        assert_eq!(
            pushed.len(),
            64,
            "push must print a 64-hex id (form {form:?})"
        );

        let reid = run_from(&parent, &["id", form], &[("SNAPDIR_CACHE_DIR", &cache_str)]);
        let reid = reid.trim_end().to_owned();
        assert_eq!(pushed, reid, "push id must equal re-id (form {form:?})");

        ids.push(pushed);
        fs::remove_dir_all(&store).ok();
    }
    for form_id in &ids[1..] {
        assert_eq!(
            &ids[0], form_id,
            "all four push/re-id forms must match: {ids:?}"
        );
    }
    fs::remove_dir_all(&parent).ok();
    fs::remove_dir_all(&cache).ok();
}

/// (4) Pinned blake3 snapshot id for the canonical `foo` form — guards the
/// invariant that a plain (no trailing slash, no `./`) path's output is
/// unchanged by the normalization. The value was computed once from this binary
/// over the fixture tree (`build_foo_tree`).
#[test]
fn path_normalize_canonical_foo_id_pinned() {
    let parent = temp_dir("pinned");
    build_foo_tree(&parent);

    let id = run_from(&parent, &["id", "foo"], &[]);
    let id = id.trim_end();
    assert_eq!(
        id, PINNED_FOO_ID,
        "canonical `foo` snapshot id must be byte-stable (invariant guard)"
    );
    fs::remove_dir_all(&parent).ok();
}

/// blake3 snapshot id of the `build_foo_tree` fixture, canonical `foo` form.
/// Computed once from the current binary; pinned to guard the no-op invariant.
const PINNED_FOO_ID: &str = "ff83fc0387da6480304710b52094f1d137736908661b01cb4e838116dc37d231";
