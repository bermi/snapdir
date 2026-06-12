//! Black-box spec suite for the `snapdir diff` subcommand (phase 28, gate
//! `diff-command-spec-tests`).
//!
//! AUTHORED FROM THE SPEC ONLY — the `diff` / `run_diff` implementation does NOT
//! exist yet, so this suite is EXPECTED to fail until the impl lands. It is staged
//! in `.gatesmith/pending-tests/` so the workspace keeps compiling; the cli impl
//! teammate moves it to `crates/snapdir-cli/tests/diff.rs` and wires it.
//!
//! SPEC under test
//! ===============
//! `snapdir diff --from <ref> [--from <ref> …] --to <ref> [--to <ref> …]` compares
//! two SIDES, each a SET of one-or-more manifests, and reports file-level
//! differences. It reads MANIFESTS ONLY — it NEVER constructs an object store or
//! downloads a blob.
//!
//! - Each `<ref>` selects manifests from a manifest-store prefix (enumerated via
//!   the landed `list_manifest_ids`) and/or an explicit `--id`/id. MULTIPLE refs
//!   per side are UNIONED into a `path -> (path_type, permissions, checksum, size)`
//!   map.
//! - Classification by path: in TO-not-FROM = `A` (added); in FROM-not-TO = `D`
//!   (deleted); in BOTH with differing checksum = `M` (modified — also surface
//!   mode-only / size-only deltas); EQUAL = hidden unless `--all`.
//! - Output: porcelain `X\t./path` (X in `A|D|M`), sorted by path; plus `--json`
//!   (array of `{status, path, …}`). `--all` includes unchanged.
//! - `--exit-code`: any difference -> exit 1 (git `diff --exit-code` semantics);
//!   DEFAULT exit 0 regardless of differences.
//! - Intra-side path collision (the SAME path with DIFFERING content across two
//!   refs unioned on ONE side): default = ERROR with an actionable message;
//!   `--on-conflict last-wins` overrides (last ref wins).
//!
//! CRITICAL CONTRACT pinned here: `diff` reads manifests only, NEVER objects. We
//! run `diff` against a manifest store whose `.objects` pool is bogus / absent /
//! empty and assert it STILL succeeds with correct output (see
//! `diff_reads_manifests_only_ignores_bogus_objects_pool` and friends).
//!
//! These are end-to-end CLI tests driving the REAL `snapdir` binary over `file://`
//! manifest stores with NO credentials, mirroring the existing CLI harness
//! conventions (`objects_store.rs`, `store_roundtrip.rs`, `store_env.rs`).
//!
//! Note on arg/ref grammar: the SPEC does not fix the exact `<ref>` token shape.
//! These tests assume a `<ref>` is a `file://` manifest-store URL, and that a
//! store holding exactly ONE manifest contributes exactly that manifest's entries
//! to its side (the store is enumerated via `list_manifest_ids`). Where a single
//! specific manifest must be pinned we ALSO pass `--id <id>`; the impl teammate may
//! adjust the exact ref token but MUST preserve the BEHAVIORS pinned here.

// The crate enables `clippy::pedantic` workspace-wide; suppress test-only
// stylistic lints (mirroring the `#[allow(...)]` in sibling suites) so the staged
// suite compiles under `-D warnings` WITHOUT touching any assertion.
#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::doc_markdown,
    clippy::manual_split_once,
    clippy::needless_splitn
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Path to the compiled `snapdir` binary under test.
fn snapdir_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// A unique temp directory; created and returned.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-diff-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A `file://<dir>` store URI for `dir`.
fn file_url(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

/// Runs the `snapdir` binary with the cache pinned under `cache` and the store
/// env vars REMOVED so the developer's env can't mask a bug.
fn run_raw(args: &[&str], cache: &Path, extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(snapdir_bin());
    cmd.args(args)
        .env("SNAPDIR_CACHE_DIR", cache)
        .env_remove("SNAPDIR_STORE")
        .env_remove("SNAPDIR_OBJECTS_STORE");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("run snapdir")
}

/// Runs `snapdir <args>`, asserts success (exit 0), returns trimmed stdout.
fn run_ok(args: &[&str], cache: &Path, extra_env: &[(&str, &str)]) -> String {
    let out = run_raw(args, cache, extra_env);
    assert!(
        out.status.success(),
        "snapdir {args:?} exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout)
        .expect("stdout is UTF-8")
        .trim_end()
        .to_owned()
}

/// stderr of an `Output`, lossy, NOT lowercased.
fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Extracts one flat JSON string field's value from a fragment (mirrors the
/// `json_field` helper in `catalog_commands.rs` so this suite stays dep-free — no
/// `serde_json` in the cli test crate). Returns the value of the FIRST `"key":"…"`
/// found in `fragment`.
fn json_str_field<'a>(fragment: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":");
    let start = fragment.find(&needle)? + needle.len();
    let rest = fragment[start..].trim_start();
    let stripped = rest.strip_prefix('"')?;
    let end = stripped.find('"')?;
    Some(&stripped[..end])
}

/// Splits a JSON array body into its top-level object fragments by brace depth,
/// tolerant of strings (so `{` inside a path string doesn't split). Sufficient for
/// the flat `[{"status":"A","path":"./x"}, …]` shape the SPEC defines.
fn json_array_objects(json: &str) -> Vec<String> {
    let trimmed = json.trim();
    assert!(
        trimmed.starts_with('[') && trimmed.ends_with(']'),
        "--json must be a JSON array; got:\n{json}"
    );
    let inner = &trimmed[1..trimmed.len() - 1];
    let mut objs = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut start = None;
    for (i, ch) in inner.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let s = start.take().unwrap();
                    objs.push(inner[s..=i].to_owned());
                }
            }
            _ => {}
        }
    }
    assert_eq!(depth, 0, "unbalanced braces in --json output:\n{json}");
    objs
}

/// Builds a tree with deterministic perms so it manifests to a stable id.
/// `leaves` is `(relative_path, contents, mode)`.
fn build_tree(dir: &Path, leaves: &[(&str, &[u8], u32)]) {
    for (rel, bytes, mode) in leaves {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(*mode)).unwrap();
    }
    set_dir_perms_recursive(dir);
}

fn set_dir_perms_recursive(dir: &Path) {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        if entry.file_type().unwrap().is_dir() {
            set_dir_perms_recursive(&entry.path());
        }
    }
}

/// Pushes `leaves` (built into a fresh source dir) into a FRESH `file://` manifest
/// store, returning `(store_dir, store_url, snapshot_id)`. Each store ends up
/// holding exactly ONE manifest, so referencing the store contributes exactly that
/// capture's entries to a diff side.
fn capture(tag: &str, cache: &Path, leaves: &[(&str, &[u8], u32)]) -> (PathBuf, String, String) {
    let src = temp_dir(&format!("{tag}-src"));
    let store = temp_dir(&format!("{tag}-store"));
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();
    let store_url = file_url(&store);
    let id = run_ok(&["push", "--store", &store_url, &src_str], cache, &[]);
    assert_eq!(id.len(), 64, "snapshot id is 64 hex chars");
    fs::remove_dir_all(&src).ok();
    (store, store_url, id)
}

/// Pushes `leaves` into an EXISTING manifest store (a second capture into the same
/// store), returning the snapshot id. Used to build multi-manifest stores.
fn capture_into(tag: &str, cache: &Path, store_url: &str, leaves: &[(&str, &[u8], u32)]) -> String {
    let src = temp_dir(&format!("{tag}-src"));
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();
    let id = run_ok(&["push", "--store", store_url, &src_str], cache, &[]);
    fs::remove_dir_all(&src).ok();
    id
}

/// Recursively deletes the `.objects/` pool of a `file://` store and replaces it
/// with GARBAGE, to prove `diff` never reads it. After this, any code path that
/// tries to open the object store or fetch a blob would fail.
fn sabotage_objects_pool(store: &Path) {
    let objects = store.join(".objects");
    if objects.exists() {
        fs::remove_dir_all(&objects).ok();
    }
    // Re-create `.objects` as a regular FILE containing junk: an object store that
    // tried to treat this as its sharded pool root would error out.
    fs::write(&objects, b"NOT-AN-OBJECT-POOL\x00\xff garbage").unwrap();
}

/// Parses porcelain `diff` stdout into a sorted `Vec<(status, path)>`.
/// Each line is exactly `X\t<path>` with X in {A,D,M}.
fn parse_porcelain(stdout: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut it = line.splitn(2, '\t');
            let status = it.next().unwrap_or("").to_owned();
            let path = it
                .next()
                .unwrap_or_else(|| panic!("porcelain line {line:?} must be 'X\\t<path>'"))
                .to_owned();
            assert!(
                status == "A" || status == "D" || status == "M",
                "status letter must be one of A|D|M, got {status:?} in {line:?}"
            );
            (status, path)
        })
        .collect();
    out.sort();
    out
}

/// Asserts the porcelain stdout is EXACTLY `expected` (status, path) pairs, and
/// that the RAW output is already sorted by path (sort-stability contract).
fn assert_porcelain_eq(stdout: &str, expected: &[(&str, &str)]) {
    // Raw order must already be sorted by path (the SPEC says "sorted by path").
    let raw_paths: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.splitn(2, '\t').nth(1).expect("tab-separated path"))
        .collect();
    let mut sorted_paths = raw_paths.clone();
    sorted_paths.sort_unstable();
    assert_eq!(
        raw_paths, sorted_paths,
        "porcelain output MUST be sorted by path; got {raw_paths:?}"
    );

    let got = parse_porcelain(stdout);
    let mut want: Vec<(String, String)> = expected
        .iter()
        .map(|(s, p)| ((*s).to_owned(), (*p).to_owned()))
        .collect();
    want.sort();
    assert_eq!(
        got, want,
        "porcelain diff mismatch.\n got: {got:?}\nwant: {want:?}\nraw stdout:\n{stdout}"
    );
}

// ===========================================================================
// (1) A/D/M CLASSIFICATION — the core porcelain contract.
// ===========================================================================

/// SPEC classification: a path in TO-not-FROM is `A`, in FROM-not-TO is `D`, in
/// BOTH with a differing checksum is `M`; equal paths are HIDDEN by default. Pins
/// all three letters at once with the exact `X\t./path` shape, sorted by path.
#[test]
fn classifies_added_deleted_modified_and_hides_equal() {
    let cache = temp_dir("adm-cache");

    // FROM has: keep.txt (unchanged), gone.txt (deleted), changed.txt (v1).
    let (_from_store, from_url, _from_id) = capture(
        "adm-from",
        &cache,
        &[
            ("keep.txt", b"same", 0o644),
            ("gone.txt", b"removed in TO", 0o644),
            ("changed.txt", b"version one", 0o644),
        ],
    );
    // TO has: keep.txt (unchanged), changed.txt (v2), new.txt (added).
    let (_to_store, to_url, _to_id) = capture(
        "adm-to",
        &cache,
        &[
            ("keep.txt", b"same", 0o644),
            ("changed.txt", b"version TWO is longer", 0o644),
            ("new.txt", b"brand new", 0o644),
        ],
    );

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);

    // keep.txt + the two directory entries (`./`) are EQUAL -> hidden by default.
    assert_porcelain_eq(
        &stdout,
        &[
            ("M", "./changed.txt"),
            ("D", "./gone.txt"),
            ("A", "./new.txt"),
        ],
    );

    fs::remove_dir_all(&cache).ok();
}

/// SPEC `--all`: with `--all`, unchanged paths are ALSO emitted (there must be at
/// least one unchanged entry, `keep.txt`, shown alongside the changed ones), while
/// without `--all` it is hidden. Pins the inclusion of equal paths under `--all`.
#[test]
fn all_flag_includes_unchanged_paths() {
    let cache = temp_dir("all-cache");

    let (_from_store, from_url, _from_id) = capture(
        "all-from",
        &cache,
        &[("keep.txt", b"identical", 0o644), ("drop.txt", b"x", 0o644)],
    );
    let (_to_store, to_url, _to_id) = capture(
        "all-to",
        &cache,
        &[("keep.txt", b"identical", 0o644), ("add.txt", b"y", 0o644)],
    );

    // Without --all: keep.txt is hidden.
    let plain = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    assert!(
        !plain.contains("keep.txt"),
        "unchanged keep.txt must be HIDDEN without --all; got:\n{plain}"
    );
    assert!(plain.contains("drop.txt") && plain.contains("add.txt"));

    // With --all: keep.txt appears too. The SPEC fixes A|D|M letters for changed
    // entries; an unchanged entry is surfaced under some non-A/D/M marker, so we
    // assert by path presence + that the changed letters are still correct.
    let all = run_ok(
        &["diff", "--from", &from_url, "--to", &to_url, "--all"],
        &cache,
        &[],
    );
    assert!(
        all.contains("keep.txt"),
        "--all must INCLUDE the unchanged keep.txt; got:\n{all}"
    );
    assert!(
        all.lines()
            .any(|l| l.ends_with("./drop.txt") && l.starts_with('D')),
        "drop.txt must still be D under --all; got:\n{all}"
    );
    assert!(
        all.lines()
            .any(|l| l.ends_with("./add.txt") && l.starts_with('A')),
        "add.txt must still be A under --all; got:\n{all}"
    );

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (2) THE MANIFESTS-ONLY CONTRACT — diff NEVER reads objects.  *PINNED HARD*
// ===========================================================================

/// SPEC critical contract: `diff` reads MANIFESTS ONLY and NEVER constructs an
/// object store or fetches a blob. PROOF: after capturing two trees we SABOTAGE
/// each store's `.objects/` pool (replace it with garbage), then run `diff` with a
/// FRESH cache (so nothing is served from a local object cache). If `diff` ever
/// tried to open the object store or fetch any blob it would FAIL here; it must
/// instead SUCCEED and produce the correct A/D/M output.
#[test]
fn diff_reads_manifests_only_ignores_bogus_objects_pool() {
    let cache = temp_dir("mo-cache");

    let (from_store, from_url, _fid) = capture(
        "mo-from",
        &cache,
        &[
            ("keep.txt", b"same", 0o644),
            ("old.txt", b"old only", 0o644),
        ],
    );
    let (to_store, to_url, _tid) = capture(
        "mo-to",
        &cache,
        &[
            ("keep.txt", b"same", 0o644),
            ("new.txt", b"new only", 0o644),
        ],
    );

    // Destroy BOTH object pools — diff must not touch them.
    sabotage_objects_pool(&from_store);
    sabotage_objects_pool(&to_store);

    // A pristine cache: no object can be served locally either.
    let fresh_cache = temp_dir("mo-freshcache");
    let out = run_raw(
        &["diff", "--from", &from_url, "--to", &to_url],
        &fresh_cache,
        &[],
    );
    assert!(
        out.status.success(),
        "diff MUST succeed against a manifest store with a bogus/absent .objects \
         pool (it reads manifests only); exited {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert_porcelain_eq(&stdout, &[("D", "./old.txt"), ("A", "./new.txt")]);

    fs::remove_dir_all(&cache).ok();
    fs::remove_dir_all(&fresh_cache).ok();
}

/// SPEC manifests-only (absent pool variant): a manifest store with NO `.objects/`
/// directory at all (only `.manifests/`) still diffs correctly. Belt-and-braces
/// against an impl that lazily creates/opens an object store only when present.
#[test]
fn diff_works_with_absent_objects_directory() {
    let cache = temp_dir("ao-cache");

    let (from_store, from_url, _fid) = capture("ao-from", &cache, &[("a.txt", b"one", 0o644)]);
    let (to_store, to_url, _tid) = capture("ao-to", &cache, &[("a.txt", b"two-changed", 0o644)]);

    // Remove `.objects/` entirely from both stores (leave `.manifests/`).
    for s in [&from_store, &to_store] {
        let objects = s.join(".objects");
        if objects.exists() {
            fs::remove_dir_all(&objects).ok();
        }
        assert!(
            s.join(".manifests").exists(),
            "the manifest dir must remain so diff has something to read"
        );
    }

    let fresh_cache = temp_dir("ao-freshcache");
    let stdout = run_ok(
        &["diff", "--from", &from_url, "--to", &to_url],
        &fresh_cache,
        &[],
    );
    assert_porcelain_eq(&stdout, &[("M", "./a.txt")]);

    fs::remove_dir_all(&cache).ok();
    fs::remove_dir_all(&fresh_cache).ok();
}

// ===========================================================================
// (3) --exit-code — git diff --exit-code semantics (BOTH branches).
// ===========================================================================

/// SPEC `--exit-code` (differences present): with `--exit-code`, ANY difference
/// makes `diff` exit 1 (git semantics) — while STILL printing the porcelain to
/// stdout.
#[test]
fn exit_code_one_when_differences_present() {
    let cache = temp_dir("ec1-cache");

    let (_fs, from_url, _fid) = capture("ec1-from", &cache, &[("x.txt", b"a", 0o644)]);
    let (_ts, to_url, _tid) = capture("ec1-to", &cache, &[("x.txt", b"b-changed", 0o644)]);

    let out = run_raw(
        &["diff", "--exit-code", "--from", &from_url, "--to", &to_url],
        &cache,
        &[],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "--exit-code with a difference must exit 1; stderr: {}",
        stderr_of(&out)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert_porcelain_eq(&stdout, &[("M", "./x.txt")]);

    fs::remove_dir_all(&cache).ok();
}

/// SPEC `--exit-code` (no differences): with `--exit-code` and ZERO differences,
/// `diff` exits 0 with no output.
#[test]
fn exit_code_zero_when_no_differences() {
    let cache = temp_dir("ec0-cache");

    // Identical trees in two different stores.
    let leaves: &[(&str, &[u8], u32)] = &[("x.txt", b"same", 0o644), ("y.txt", b"same2", 0o644)];
    let (_fs, from_url, fid) = capture("ec0-from", &cache, leaves);
    let (_ts, to_url, tid) = capture("ec0-to", &cache, leaves);
    assert_eq!(fid, tid, "identical trees must share the snapshot id");

    let out = run_raw(
        &["diff", "--exit-code", "--from", &from_url, "--to", &to_url],
        &cache,
        &[],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "--exit-code with NO differences must exit 0; stderr: {}",
        stderr_of(&out)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        stdout.trim().is_empty(),
        "no differences -> no porcelain output; got:\n{stdout}"
    );

    fs::remove_dir_all(&cache).ok();
}

/// SPEC default exit (no `--exit-code`): WITHOUT `--exit-code`, `diff` exits 0
/// REGARDLESS of differences (the difference is reported on stdout, not the code).
#[test]
fn default_exit_zero_even_with_differences() {
    let cache = temp_dir("ed-cache");

    let (_fs, from_url, _fid) = capture("ed-from", &cache, &[("x.txt", b"a", 0o644)]);
    let (_ts, to_url, _tid) = capture("ed-to", &cache, &[("x.txt", b"changed", 0o644)]);

    let out = run_raw(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "default (no --exit-code) must exit 0 even WITH differences; stderr: {}",
        stderr_of(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("./x.txt"),
        "the difference must still be reported on stdout"
    );

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (4) EMPTY / IDENTICAL / DEGENERATE.
// ===========================================================================

/// SPEC empty-vs-empty: when BOTH sides resolve to nothing (empty manifest stores,
/// no manifests), `diff` produces NO output and exits 0.
#[test]
fn empty_vs_empty_no_output_exit_zero() {
    let cache = temp_dir("ee-cache");

    // Two empty manifest stores (created, but never pushed to).
    let from_store = temp_dir("ee-from");
    let to_store = temp_dir("ee-to");
    let from_url = file_url(&from_store);
    let to_url = file_url(&to_store);

    let out = run_raw(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "empty-vs-empty must exit 0; stderr: {}",
        stderr_of(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "empty-vs-empty must produce no output"
    );

    // Even with --exit-code: no differences -> still exit 0.
    let out2 = run_raw(
        &["diff", "--exit-code", "--from", &from_url, "--to", &to_url],
        &cache,
        &[],
    );
    assert_eq!(
        out2.status.code(),
        Some(0),
        "empty-vs-empty with --exit-code is still 0 (no differences)"
    );

    fs::remove_dir_all(&cache).ok();
    fs::remove_dir_all(&from_store).ok();
    fs::remove_dir_all(&to_store).ok();
}

/// SPEC identical: identical manifests on both sides produce NO output (unless
/// `--all`), exit 0 — and WITH `--all` every shared path is surfaced.
#[test]
fn identical_manifests_no_output_unless_all() {
    let cache = temp_dir("id-cache");

    let leaves: &[(&str, &[u8], u32)] =
        &[("a.txt", b"alpha", 0o644), ("dir/b.txt", b"bravo", 0o644)];
    let (_fs, from_url, fid) = capture("id-from", &cache, leaves);
    let (_ts, to_url, tid) = capture("id-to", &cache, leaves);
    assert_eq!(fid, tid);

    let plain = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    assert!(
        plain.trim().is_empty(),
        "identical manifests must produce no output without --all; got:\n{plain}"
    );

    let all = run_ok(
        &["diff", "--from", &from_url, "--to", &to_url, "--all"],
        &cache,
        &[],
    );
    assert!(
        all.contains("./a.txt") && all.contains("./dir/b.txt"),
        "--all must surface the shared (unchanged) paths even when identical; got:\n{all}"
    );
    // No change letters: nothing is A/D/M here.
    for line in all.lines().filter(|l| !l.is_empty()) {
        let status = line.splitn(2, '\t').next().unwrap_or("");
        assert!(
            status != "A" && status != "D" && status != "M",
            "identical-side --all must not mark any path A/D/M; got {line:?}"
        );
    }

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (5) MODE-ONLY and SIZE-ONLY changes — classified `M` per SPEC.
// ===========================================================================

/// SPEC mode-only: a pure permissions change (SAME checksum/content, DIFFERENT
/// mode) is classified `M` (the SPEC says "surface mode-only ... deltas").
#[test]
fn mode_only_change_is_modified() {
    let cache = temp_dir("mode-cache");

    // Same content "exec me", different permissions (0644 vs 0755).
    let (_fs, from_url, fid) = capture("mode-from", &cache, &[("s.sh", b"exec me", 0o644)]);
    let (_ts, to_url, tid) = capture("mode-to", &cache, &[("s.sh", b"exec me", 0o755)]);

    // The CONTENT is identical (same blob checksum) but the manifest id differs
    // because permissions are part of the merkle. So this is a real mode-only M.
    assert_ne!(
        fid, tid,
        "a permission change must change the snapshot id (perms are in the merkle)"
    );

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    assert_porcelain_eq(&stdout, &[("M", "./s.sh")]);

    fs::remove_dir_all(&cache).ok();
}

/// SPEC size-only/content: a content change that also changes the size is
/// classified `M` (differing checksum). Pins that a size+content delta surfaces as
/// a single `M` for that path, not an A+D pair.
#[test]
fn content_size_change_is_single_modified() {
    let cache = temp_dir("size-cache");

    let (_fs, from_url, _fid) = capture("size-from", &cache, &[("f.txt", b"tiny", 0o644)]);
    let (_ts, to_url, _tid) = capture(
        "size-to",
        &cache,
        &[("f.txt", b"a substantially longer body of text", 0o644)],
    );

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    // Exactly ONE M for the path — NOT an A and a D.
    assert_porcelain_eq(&stdout, &[("M", "./f.txt")]);
    let m_count = stdout.lines().filter(|l| l.starts_with('M')).count();
    assert_eq!(m_count, 1, "a changed path is ONE M line, not A+D");

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (6) --json — stable, parseable shape.
// ===========================================================================

/// SPEC `--json`: emits a JSON ARRAY of objects, each with at least `status` and
/// `path`, where `status` is one of `A|D|M` and `path` matches the porcelain path.
/// Pins that the JSON is parseable, an array, and agrees with the porcelain set.
#[test]
fn json_output_is_array_of_status_path_objects() {
    let cache = temp_dir("json-cache");

    let (_fs, from_url, _fid) = capture(
        "json-from",
        &cache,
        &[
            ("keep.txt", b"same", 0o644),
            ("gone.txt", b"x", 0o644),
            ("chg.txt", b"v1", 0o644),
        ],
    );
    let (_ts, to_url, _tid) = capture(
        "json-to",
        &cache,
        &[
            ("keep.txt", b"same", 0o644),
            ("add.txt", b"y", 0o644),
            ("chg.txt", b"v2-longer", 0o644),
        ],
    );

    let json = run_ok(
        &["diff", "--json", "--from", &from_url, "--to", &to_url],
        &cache,
        &[],
    );

    // Must be a JSON array of objects; default (no --all) -> 3 changed entries.
    let objs = json_array_objects(&json);
    let mut pairs: Vec<(String, String)> = objs
        .iter()
        .map(|obj| {
            let status = json_str_field(obj, "status")
                .unwrap_or_else(|| panic!("each entry must have a string `status`; got {obj}"))
                .to_owned();
            let path = json_str_field(obj, "path")
                .unwrap_or_else(|| panic!("each entry must have a string `path`; got {obj}"))
                .to_owned();
            assert!(
                status == "A" || status == "D" || status == "M",
                "json status must be A|D|M, got {status:?}"
            );
            (status, path)
        })
        .collect();
    pairs.sort();

    let mut want = vec![
        ("A".to_owned(), "./add.txt".to_owned()),
        ("D".to_owned(), "./gone.txt".to_owned()),
        ("M".to_owned(), "./chg.txt".to_owned()),
    ];
    want.sort();
    assert_eq!(
        pairs, want,
        "json entries must match the change set; got:\n{json}"
    );

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (7) MULTI-REF UNION on a side.
// ===========================================================================

/// SPEC multi-ref union (disjoint): two `--from` refs with DISJOINT paths are
/// UNIONED into the FROM side, so a TO that lacks both shows two `D`s, one per
/// unioned path. Pins that multiple refs per side compose into one map.
#[test]
fn multi_ref_from_union_disjoint_paths() {
    let cache = temp_dir("mru-cache");

    // Two FROM stores with disjoint files.
    let (_f1, from1_url, _f1id) = capture("mru-from1", &cache, &[("only1.txt", b"one", 0o644)]);
    let (_f2, from2_url, _f2id) = capture("mru-from2", &cache, &[("only2.txt", b"two", 0o644)]);
    // TO has neither file (a different file entirely).
    let (_ts, to_url, _tid) = capture("mru-to", &cache, &[("other.txt", b"o", 0o644)]);

    let stdout = run_ok(
        &[
            "diff", "--from", &from1_url, "--from", &from2_url, "--to", &to_url,
        ],
        &cache,
        &[],
    );
    // Both only1/only2 deleted; other.txt added.
    assert_porcelain_eq(
        &stdout,
        &[
            ("A", "./other.txt"),
            ("D", "./only1.txt"),
            ("D", "./only2.txt"),
        ],
    );

    fs::remove_dir_all(&cache).ok();
}

/// SPEC multi-ref union (same path, SAME content = no conflict): two refs on one
/// side carrying the SAME path with IDENTICAL content union WITHOUT error, and the
/// entry behaves as a single entry against the other side.
#[test]
fn multi_ref_same_path_same_content_no_conflict() {
    let cache = temp_dir("mrs-cache");

    // Two FROM stores BOTH containing dup.txt with the SAME content.
    let (_f1, from1_url, _f1id) = capture(
        "mrs-from1",
        &cache,
        &[("dup.txt", b"identical", 0o644), ("a.txt", b"a", 0o644)],
    );
    let (_f2, from2_url, _f2id) = capture(
        "mrs-from2",
        &cache,
        &[("dup.txt", b"identical", 0o644), ("b.txt", b"b", 0o644)],
    );
    // TO has dup.txt with the SAME content (so it stays unchanged/hidden).
    let (_ts, to_url, _tid) = capture("mrs-to", &cache, &[("dup.txt", b"identical", 0o644)]);

    let out = run_raw(
        &[
            "diff", "--from", &from1_url, "--from", &from2_url, "--to", &to_url,
        ],
        &cache,
        &[],
    );
    assert!(
        out.status.success(),
        "same-path/same-content union must NOT be a conflict; stderr: {}",
        stderr_of(&out)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    // dup.txt equal on both sides -> hidden; a.txt and b.txt only in FROM -> D.
    assert_porcelain_eq(&stdout, &[("D", "./a.txt"), ("D", "./b.txt")]);
    assert!(
        !stdout.contains("dup.txt"),
        "an unchanged unioned path must stay hidden; got:\n{stdout}"
    );

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (8) INTRA-SIDE COLLISION POLICY — default ERROR, --on-conflict last-wins.
// ===========================================================================

/// SPEC collision (default = ERROR): the SAME path with DIFFERING content across
/// two refs UNIONED on ONE side is an ERROR by default, with an actionable message
/// that names the colliding path. NO porcelain is produced; exit is non-zero.
#[test]
fn intra_side_collision_errors_by_default() {
    let cache = temp_dir("col-cache");

    // Two FROM stores BOTH containing clash.txt but with DIFFERENT content.
    let (_f1, from1_url, _f1id) = capture(
        "col-from1",
        &cache,
        &[("clash.txt", b"left version", 0o644)],
    );
    let (_f2, from2_url, _f2id) = capture(
        "col-from2",
        &cache,
        &[("clash.txt", b"RIGHT version", 0o644)],
    );
    let (_ts, to_url, _tid) = capture("col-to", &cache, &[("z.txt", b"z", 0o644)]);

    let out = run_raw(
        &[
            "diff", "--from", &from1_url, "--from", &from2_url, "--to", &to_url,
        ],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "an intra-side collision (same path, differing content) must be a hard \
         error by default; got success.\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = stderr_of(&out);
    let lc = stderr.to_lowercase();
    assert!(
        lc.contains("conflict") || lc.contains("collision") || lc.contains("clash"),
        "the error must explain the collision actionably; got: {stderr}"
    );
    assert!(
        stderr.contains("clash.txt") || stderr.contains("./clash.txt"),
        "the error must name the colliding path; got: {stderr}"
    );
    // It must NOT have silently emitted porcelain as if all was well.
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("clash.txt"),
        "a collision must not be silently resolved into normal porcelain output"
    );

    fs::remove_dir_all(&cache).ok();
}

/// SPEC collision (`--on-conflict last-wins`): with `--on-conflict last-wins`, the
/// LAST ref on the side wins, so the colliding path resolves to the last ref's
/// content. Pins that the override SUCCEEDS and selects the last ref's entry.
#[test]
fn intra_side_collision_last_wins_selects_last_ref() {
    let cache = temp_dir("lw-cache");

    // FROM = [from1: clash=LEFT] ++ [from2: clash=RIGHT-WINS]; last (from2) wins.
    let (_f1, from1_url, _f1id) =
        capture("lw-from1", &cache, &[("clash.txt", b"LEFT loses", 0o644)]);
    let (_f2, from2_url, _f2id) =
        capture("lw-from2", &cache, &[("clash.txt", b"RIGHT-WINS", 0o644)]);
    // TO carries clash.txt == the WINNING (last) FROM content, so if last-wins is
    // honored the path is EQUAL and hidden; if first-wins (wrong) it would be M.
    let (_ts, to_url, _tid) = capture("lw-to", &cache, &[("clash.txt", b"RIGHT-WINS", 0o644)]);

    let stdout = run_ok(
        &[
            "diff",
            "--on-conflict",
            "last-wins",
            "--from",
            &from1_url,
            "--from",
            &from2_url,
            "--to",
            &to_url,
        ],
        &cache,
        &[],
    );
    // last-wins -> FROM clash == "RIGHT-WINS" == TO clash -> EQUAL -> hidden.
    assert!(
        stdout.trim().is_empty(),
        "last-wins must select the LAST ref's content (RIGHT-WINS), matching TO, \
         so clash.txt is equal and hidden; got:\n{stdout}"
    );

    // Cross-check: if TO instead held the LOSING (first) content, last-wins yields M.
    let (_ts2, to2_url, _tid2) = capture("lw-to2", &cache, &[("clash.txt", b"LEFT loses", 0o644)]);
    let stdout2 = run_ok(
        &[
            "diff",
            "--on-conflict",
            "last-wins",
            "--from",
            &from1_url,
            "--from",
            &from2_url,
            "--to",
            &to2_url,
        ],
        &cache,
        &[],
    );
    assert_porcelain_eq(&stdout2, &[("M", "./clash.txt")]);

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (9) UNICODE / SPACE / ./-PREFIXED PATHS — stable sort + round-trip.
// ===========================================================================

/// SPEC unicode/space sort stability: paths containing unicode and spaces sort
/// STABLY in porcelain (already byte-sorted) and round-trip exactly (the `./`
/// prefix and the literal bytes are preserved). The set is chosen so a naive vs
/// byte sort would differ if mishandled.
#[test]
fn unicode_and_space_paths_sort_stably_and_round_trip() {
    let cache = temp_dir("uni-cache");

    // All ADDED (TO only), so each appears once with status A, exercising sort.
    let leaves: &[(&str, &[u8], u32)] = &[
        ("zeta.txt", b"z", 0o644),
        ("a file with spaces.txt", b"s", 0o644),
        ("café.txt", b"c", 0o644),
        ("naïve dir/inner.txt", b"i", 0o644),
        ("Apple.txt", b"A", 0o644), // uppercase sorts before lowercase by byte
    ];
    let (_fs, from_url, _fid) = capture("uni-from", &cache, &[("placeholder.txt", b"p", 0o644)]);
    let (_ts, to_url, _tid) = capture("uni-to", &cache, leaves);

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);

    // Every expected path must appear, ./-prefixed and byte-exact.
    for (rel, _, _) in leaves {
        let want = format!("./{rel}");
        assert!(
            stdout.lines().any(|l| l == format!("A\t{want}")),
            "expected an `A\\t{want}` line (byte-exact, ./-prefixed); got:\n{stdout}"
        );
    }
    // placeholder.txt (FROM only) is a D.
    assert!(stdout.lines().any(|l| l == "D\t./placeholder.txt"));

    // Porcelain must already be sorted by path (byte order) — assert_porcelain_eq
    // checks raw==sorted, so reuse it for the full set.
    let mut expected: Vec<(&str, &str)> = vec![("D", "./placeholder.txt")];
    let owned: Vec<String> = leaves.iter().map(|(r, _, _)| format!("./{r}")).collect();
    for p in &owned {
        expected.push(("A", p.as_str()));
    }
    assert_porcelain_eq(&stdout, &expected);

    // And the same set in --json must preserve the exact path bytes too.
    let json = run_ok(
        &["diff", "--json", "--from", &from_url, "--to", &to_url],
        &cache,
        &[],
    );
    let paths: Vec<String> = json_array_objects(&json)
        .iter()
        .map(|o| {
            json_str_field(o, "path")
                .unwrap_or_else(|| panic!("each json entry needs a `path`; got {o}"))
                .to_owned()
        })
        .collect();
    for (rel, _, _) in leaves {
        assert!(
            paths.contains(&format!("./{rel}")),
            "json must carry the exact unicode/space path ./{rel}; got: {paths:?}"
        );
    }

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (10) DIRECTION SANITY — swapping --from/--to flips A <-> D.
// ===========================================================================

/// SPEC classification direction: A means "in TO not FROM", D means "in FROM not
/// TO". Swapping the two sides must FLIP A<->D for the same trees (and keep M as M).
#[test]
fn swapping_from_to_flips_added_and_deleted() {
    let cache = temp_dir("dir-cache");

    let (_fs, left_url, _lid) = capture(
        "dir-left",
        &cache,
        &[
            ("keep.txt", b"k", 0o644),
            ("leftonly.txt", b"L", 0o644),
            ("chg.txt", b"v1", 0o644),
        ],
    );
    let (_ts, right_url, _rid) = capture(
        "dir-right",
        &cache,
        &[
            ("keep.txt", b"k", 0o644),
            ("rightonly.txt", b"R", 0o644),
            ("chg.txt", b"v2", 0o644),
        ],
    );

    // FROM=left, TO=right.
    let fwd = run_ok(
        &["diff", "--from", &left_url, "--to", &right_url],
        &cache,
        &[],
    );
    assert_porcelain_eq(
        &fwd,
        &[
            ("M", "./chg.txt"),
            ("D", "./leftonly.txt"),
            ("A", "./rightonly.txt"),
        ],
    );

    // FROM=right, TO=left -> A and D must flip; M stays M.
    let rev = run_ok(
        &["diff", "--from", &right_url, "--to", &left_url],
        &cache,
        &[],
    );
    assert_porcelain_eq(
        &rev,
        &[
            ("M", "./chg.txt"),
            ("A", "./leftonly.txt"),
            ("D", "./rightonly.txt"),
        ],
    );

    fs::remove_dir_all(&cache).ok();
}

// ===========================================================================
// (11) --id PINS a specific manifest within a multi-manifest store.
// ===========================================================================

/// SPEC ref selection via `--id`: a store may hold MULTIPLE manifests; `--id`
/// pins ONE specific manifest as the side (rather than unioning the whole store).
/// Two captures land in one store; pinning each id in turn yields the correct,
/// DIFFERENT diff — proving `--id` selects a single manifest, not the union.
#[test]
fn from_id_pins_single_manifest_in_multi_manifest_store() {
    let cache = temp_dir("pin-cache");

    // Build a FROM store holding TWO different manifests.
    let from_store = temp_dir("pin-from");
    let from_url = file_url(&from_store);
    let id_v1 = capture_into(
        "pin-v1",
        &cache,
        &from_url,
        &[("f.txt", b"version one", 0o644)],
    );
    let id_v2 = capture_into(
        "pin-v2",
        &cache,
        &from_url,
        &[("f.txt", b"version two!!", 0o644)],
    );
    assert_ne!(id_v1, id_v2, "the two captures must be distinct manifests");

    // TO == version two.
    let (_ts, to_url, to_id) = capture("pin-to", &cache, &[("f.txt", b"version two!!", 0o644)]);
    assert_eq!(to_id, id_v2);

    // Pin --from --id v1 -> f.txt differs from TO (v2) -> M.
    let d1 = run_ok(
        &["diff", "--from", &from_url, "--id", &id_v1, "--to", &to_url],
        &cache,
        &[],
    );
    assert_porcelain_eq(&d1, &[("M", "./f.txt")]);

    // Pin --from --id v2 -> equal to TO -> no output.
    let d2 = run_ok(
        &["diff", "--from", &from_url, "--id", &id_v2, "--to", &to_url],
        &cache,
        &[],
    );
    assert!(
        d2.trim().is_empty(),
        "pinning the matching manifest id must yield no differences; got:\n{d2}"
    );

    fs::remove_dir_all(&cache).ok();
    fs::remove_dir_all(&from_store).ok();
}

// ===========================================================================
// (12) DIRECTORY HANDLING — `diff` is FILE-LEVEL. (review-gate strengthening)
//
// The impl gate fixed two real bugs the spec suite caught: (1) directory
// entries leaked as `M`/collisions because a dir's subtree-merkle changes with
// any descendant, and (2) added/deleted directory entries leaked into the
// porcelain. `src/diff.rs` now compares directories by `(path_type,
// permissions)` only and DROPS any path that is a directory on every side it
// appears in. These tests PIN that file-level intent so a regression in the
// dir-handling fix is caught: every assertion below states the file-level
// behavior the spec mandates, and any failure is a real bug to report.
// ===========================================================================

/// A directory line in porcelain is one whose path ends with `/` (manifests
/// render directory paths with a trailing slash, e.g. `./sub/`). `diff` is
/// file-level, so NO porcelain line may carry a trailing-slash directory path.
fn assert_no_directory_lines(stdout: &str) {
    for line in stdout.lines().filter(|l| !l.is_empty()) {
        let path = line.splitn(2, '\t').nth(1).unwrap_or("");
        assert!(
            !path.ends_with('/'),
            "diff is file-level: NO directory line may appear, got {line:?} in:\n{stdout}"
        );
    }
}

/// SPEC file-level (a): a file MODIFIED inside a directory present on both sides
/// shows the FILE as `M`, and the containing directory does NOT appear — even
/// though the directory's subtree merkle changed with the descendant. Pins
/// bug-fix #1 (dir not surfaced as `M` for a descendant change).
#[test]
fn modified_file_in_dir_shows_file_not_dir() {
    let cache = temp_dir("dirM-cache");

    let (_fs, from_url, _fid) = capture(
        "dirM-from",
        &cache,
        &[
            ("top.txt", b"top-same", 0o644),
            ("sub/inner.txt", b"inner v1", 0o644),
        ],
    );
    let (_ts, to_url, _tid) = capture(
        "dirM-to",
        &cache,
        &[
            ("top.txt", b"top-same", 0o644),
            ("sub/inner.txt", b"inner v2 is longer", 0o644),
        ],
    );

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    // ONLY the file is M; the dir `./sub/` (whose merkle changed) is omitted,
    // top.txt is unchanged/hidden, and the root `./` is omitted too.
    assert_porcelain_eq(&stdout, &[("M", "./sub/inner.txt")]);
    assert_no_directory_lines(&stdout);

    fs::remove_dir_all(&cache).ok();
}

/// SPEC file-level (b): a directory present on BOTH sides whose ONLY change is a
/// descendant file (the dir's own type+perms are unchanged) is NOT itself
/// reported — neither as `M` (its merkle differs) nor under any other letter.
/// Belt-and-braces for bug-fix #1, asserting via the explicit set + no dir line.
#[test]
fn dir_with_only_descendant_change_is_not_reported() {
    let cache = temp_dir("dirDesc-cache");

    // A nested directory whose ONLY delta is a leaf two levels deep.
    let (_fs, from_url, _fid) = capture(
        "dirDesc-from",
        &cache,
        &[("a/b/leaf.txt", b"leaf one", 0o644)],
    );
    let (_ts, to_url, _tid) = capture(
        "dirDesc-to",
        &cache,
        &[("a/b/leaf.txt", b"leaf two changed", 0o644)],
    );

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    // Only the leaf is M; ./, ./a/ and ./a/b/ (all merkle-changed dirs) are gone.
    assert_porcelain_eq(&stdout, &[("M", "./a/b/leaf.txt")]);
    assert_no_directory_lines(&stdout);
    // No ancestor directory entry (matched on the FULL line's path field, not a
    // substring — `./a/b/leaf.txt` legitimately *contains* `./a/`).
    assert!(
        !stdout.lines().any(|l| {
            let p = l.splitn(2, '\t').nth(1).unwrap_or("");
            p == "./" || p == "./a/" || p == "./a/b/"
        }),
        "no ancestor directory line may surface for a descendant-only change; got:\n{stdout}"
    );

    fs::remove_dir_all(&cache).ok();
}

/// SPEC file-level (c): a WHOLE NEW subdirectory of files (TO has a directory
/// tree FROM lacks) surfaces as `A` for each FILE, with NO directory lines.
/// Pins bug-fix #2 (added directory entries must NOT leak into porcelain).
#[test]
fn new_subdir_appears_as_files_only_no_dir_lines() {
    let cache = temp_dir("dirNew-cache");

    let (_fs, from_url, _fid) = capture("dirNew-from", &cache, &[("root.txt", b"r", 0o644)]);
    // TO adds an entire `pkg/` subtree (two files at two depths) plus keeps root.
    let (_ts, to_url, _tid) = capture(
        "dirNew-to",
        &cache,
        &[
            ("root.txt", b"r", 0o644),
            ("pkg/one.txt", b"1", 0o644),
            ("pkg/nested/two.txt", b"2", 0o644),
        ],
    );

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    // Each NEW FILE is A; the new dirs `./pkg/` and `./pkg/nested/` are omitted.
    assert_porcelain_eq(
        &stdout,
        &[("A", "./pkg/nested/two.txt"), ("A", "./pkg/one.txt")],
    );
    assert_no_directory_lines(&stdout);

    fs::remove_dir_all(&cache).ok();
}

/// SPEC file-level (c, mirror): a whole subdirectory REMOVED (FROM has it, TO
/// lacks it) surfaces as `D` for each FILE, no dir lines. Confirms bug-fix #2
/// for the deletion direction (deleted dir entries must not leak as `D`).
#[test]
fn removed_subdir_appears_as_files_only_no_dir_lines() {
    let cache = temp_dir("dirRm-cache");

    let (_fs, from_url, _fid) = capture(
        "dirRm-from",
        &cache,
        &[
            ("root.txt", b"r", 0o644),
            ("pkg/one.txt", b"1", 0o644),
            ("pkg/nested/two.txt", b"2", 0o644),
        ],
    );
    let (_ts, to_url, _tid) = capture("dirRm-to", &cache, &[("root.txt", b"r", 0o644)]);

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    assert_porcelain_eq(
        &stdout,
        &[("D", "./pkg/nested/two.txt"), ("D", "./pkg/one.txt")],
    );
    assert_no_directory_lines(&stdout);

    fs::remove_dir_all(&cache).ok();
}

/// SPEC file-level (d): a directory whose PERMISSIONS change while NO file under
/// it changes. Per the impl, `diff` is FILE-level — directories are dropped from
/// file-level output, so a pure directory-mode change yields NO output. We PIN
/// the impl's documented file-level intent here: a dir-only perm change is NOT a
/// file-level difference and is omitted. (If the project later decides a dir
/// perm change SHOULD surface, this is the assertion to flip — and it would then
/// be a deliberate spec change, not a silent regression.)
#[test]
fn dir_only_permission_change_is_omitted_file_level() {
    let cache = temp_dir("dirPerm-cache");

    // Same file content + same file perms on both sides; ONLY a subdirectory's
    // mode differs (0o755 vs 0o700). The shared `build_tree`/`capture` helpers
    // force 0o755 on every dir, so we build the two source trees BY HAND here to
    // make the nested dir's perms the sole difference.
    let from_src = temp_dir("dirPerm-from-src");
    let to_src = temp_dir("dirPerm-to-src");
    for src in [&from_src, &to_src] {
        fs::create_dir_all(src.join("d")).unwrap();
        fs::write(src.join("d/f.txt"), b"same").unwrap();
        fs::set_permissions(src.join("d/f.txt"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(src, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // The ONLY difference: the `d/` directory's mode.
    fs::set_permissions(from_src.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(to_src.join("d"), fs::Permissions::from_mode(0o700)).unwrap();

    let from_store = temp_dir("dirPerm-from-store");
    let to_store = temp_dir("dirPerm-to-store");
    let from_url = file_url(&from_store);
    let to_url = file_url(&to_store);
    let fid = run_ok(
        &["push", "--store", &from_url, &from_src.to_string_lossy()],
        &cache,
        &[],
    );
    let tid = run_ok(
        &["push", "--store", &to_url, &to_src.to_string_lossy()],
        &cache,
        &[],
    );
    // The dir-mode delta DOES change the snapshot id (perms are in the merkle),
    // so the two sides genuinely differ at the manifest level...
    assert_ne!(
        fid, tid,
        "a directory permission change must change the snapshot id (perms are in the merkle)"
    );

    // ...yet `diff`, being FILE-level, reports NOTHING: the only changed entry is
    // a directory, and directories are dropped from file-level output.
    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    assert!(
        stdout.trim().is_empty(),
        "a dir-ONLY permission change is not a file-level difference -> no output; got:\n{stdout}"
    );
    assert_no_directory_lines(&stdout);

    fs::remove_dir_all(&cache).ok();
    fs::remove_dir_all(&from_src).ok();
    fs::remove_dir_all(&to_src).ok();
    fs::remove_dir_all(&from_store).ok();
    fs::remove_dir_all(&to_store).ok();
}

/// SPEC file-level (e): a path that is a FILE on one side and a DIRECTORY on the
/// other (type change). At the manifest level the file is `./x` and the
/// directory is `./x/` (dirs carry a trailing slash), so the replacement
/// surfaces as a DELETED file `./x` plus an ADDED file for the new dir's
/// leaf(s) — and the directory entry `./x/` itself never appears. Pins the
/// file<->dir type-change behavior end to end.
#[test]
fn file_replaced_by_directory_is_file_level_delete_plus_add() {
    let cache = temp_dir("dirType-cache");

    // FROM: `x` is a FILE. TO: `x` is a DIRECTORY containing `x/inner.txt`.
    let (_fs, from_url, _fid) = capture(
        "dirType-from",
        &cache,
        &[("keep.txt", b"k", 0o644), ("x", b"i am a file", 0o644)],
    );
    let (_ts, to_url, _tid) = capture(
        "dirType-to",
        &cache,
        &[
            ("keep.txt", b"k", 0o644),
            ("x/inner.txt", b"now a dir", 0o644),
        ],
    );

    let stdout = run_ok(&["diff", "--from", &from_url, "--to", &to_url], &cache, &[]);
    // `./x` (file) deleted; `./x/inner.txt` (file under the new dir) added; the
    // directory entry `./x/` itself is dropped (file-level).
    assert_porcelain_eq(&stdout, &[("A", "./x/inner.txt"), ("D", "./x")]);
    assert_no_directory_lines(&stdout);

    // The reverse direction (dir replaced by file) must flip A<->D symmetrically.
    let rev = run_ok(&["diff", "--from", &to_url, "--to", &from_url], &cache, &[]);
    assert_porcelain_eq(&rev, &[("A", "./x"), ("D", "./x/inner.txt")]);
    assert_no_directory_lines(&rev);

    fs::remove_dir_all(&cache).ok();
}

/// SPEC file-level + `--all`: even under `--all` (which surfaces UNCHANGED
/// paths), directory entries are STILL dropped — `--all` widens the FILE set,
/// never re-introduces directory rows. Pins that the dir-drop is unconditional,
/// not merely a side effect of hiding unchanged rows.
#[test]
fn all_flag_still_drops_directory_entries() {
    let cache = temp_dir("dirAll-cache");

    let (_fs, from_url, fid) = capture(
        "dirAll-from",
        &cache,
        &[("top.txt", b"t", 0o644), ("sub/inner.txt", b"same", 0o644)],
    );
    let (_ts, to_url, tid) = capture(
        "dirAll-to",
        &cache,
        &[("top.txt", b"t", 0o644), ("sub/inner.txt", b"same", 0o644)],
    );
    assert_eq!(fid, tid, "identical trees share the snapshot id");

    let all = run_ok(
        &["diff", "--from", &from_url, "--to", &to_url, "--all"],
        &cache,
        &[],
    );
    // The files appear (unchanged, non-A/D/M marker); the dirs `./` and `./sub/`
    // must NOT — even under --all.
    assert!(
        all.contains("./top.txt") && all.contains("./sub/inner.txt"),
        "--all must surface the unchanged FILES; got:\n{all}"
    );
    assert_no_directory_lines(&all);
    assert!(
        !all.lines().any(|l| {
            let p = l.splitn(2, '\t').nth(1).unwrap_or("");
            p == "./" || p == "./sub/"
        }),
        "--all must not re-introduce the directory entries ./ or ./sub/; got:\n{all}"
    );

    fs::remove_dir_all(&cache).ok();
}

/// SPEC dir-drop interaction with collision policy: two FROM refs that BOTH carry
/// the same directory subtree but DIFFER only in a descendant file's content must
/// collide on the FILE (default error), NOT on the enclosing directory — proving
/// the dir's merkle/size are excluded from the collision key (bug-fix #1 applied
/// to `union_side`, not just `classify`). The error must name the FILE path.
#[test]
fn intra_side_collision_keys_on_file_not_enclosing_dir() {
    let cache = temp_dir("dirCol-cache");

    // Two FROM refs: identical dir layout, but `sub/inner.txt` differs. Their
    // `./sub/` (and `./`) merkles differ, but those dirs must NOT be the
    // collision — only the file does.
    let (_f1, from1_url, _f1id) = capture(
        "dirCol-from1",
        &cache,
        &[("sub/inner.txt", b"left content", 0o644)],
    );
    let (_f2, from2_url, _f2id) = capture(
        "dirCol-from2",
        &cache,
        &[("sub/inner.txt", b"RIGHT content", 0o644)],
    );
    let (_ts, to_url, _tid) = capture("dirCol-to", &cache, &[("z.txt", b"z", 0o644)]);

    let out = run_raw(
        &[
            "diff", "--from", &from1_url, "--from", &from2_url, "--to", &to_url,
        ],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "differing descendant content across two FROM refs must collide (error); got success.\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("sub/inner.txt"),
        "the collision must name the FILE ./sub/inner.txt, not the dir; got: {stderr}"
    );
    // It must NOT report a directory path as the collision.
    assert!(
        !stderr.contains("\"./sub/\"") && !stderr.contains("\"./\""),
        "the collision must NOT key on the enclosing directory ./sub/ or ./; got: {stderr}"
    );

    // And under last-wins it resolves to the LAST ref's file content (a real M
    // vs a TO that holds neither -> the file is added, not the dir).
    let (_ts2, to2_url, _tid2) = capture(
        "dirCol-to2",
        &cache,
        &[("sub/inner.txt", b"RIGHT content", 0o644)],
    );
    let stdout = run_ok(
        &[
            "diff",
            "--on-conflict",
            "last-wins",
            "--from",
            &from1_url,
            "--from",
            &from2_url,
            "--to",
            &to2_url,
        ],
        &cache,
        &[],
    );
    // last-wins picks from2 (RIGHT) == to2 -> file equal -> hidden; no dir lines.
    assert!(
        stdout.trim().is_empty(),
        "last-wins selects the LAST ref's file content, matching TO -> no diff; got:\n{stdout}"
    );
    assert_no_directory_lines(&stdout);

    fs::remove_dir_all(&cache).ok();
}

/// SPEC dir-drop in `--json`: the directory-drop rule applies identically to the
/// JSON renderer — a descendant-only change yields a JSON array carrying ONLY the
/// file entry, no directory object. Pins parity between porcelain and JSON for
/// the dir-handling fix.
#[test]
fn json_drops_directory_entries_too() {
    let cache = temp_dir("dirJson-cache");

    let (_fs, from_url, _fid) = capture("dirJson-from", &cache, &[("d/leaf.txt", b"v1", 0o644)]);
    let (_ts, to_url, _tid) = capture("dirJson-to", &cache, &[("d/leaf.txt", b"v2 longer", 0o644)]);

    let json = run_ok(
        &["diff", "--json", "--from", &from_url, "--to", &to_url],
        &cache,
        &[],
    );
    let paths: Vec<String> = json_array_objects(&json)
        .iter()
        .map(|o| {
            json_str_field(o, "path")
                .unwrap_or_else(|| panic!("each json entry needs a `path`; got {o}"))
                .to_owned()
        })
        .collect();
    assert_eq!(
        paths,
        vec!["./d/leaf.txt".to_owned()],
        "json must carry ONLY the file entry, no directory object; got:\n{json}"
    );
    for p in &paths {
        assert!(
            !p.ends_with('/'),
            "no json path may be a directory (trailing slash); got {p:?}"
        );
    }

    fs::remove_dir_all(&cache).ok();
}
