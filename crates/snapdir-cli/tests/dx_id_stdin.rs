//! Black-box repro for gate `dx-id-stdin-verify` (phase 30).
//!
//! `snapdir id --help` promises: "Print the manifest ID of a directory or a
//! manifest piped via stdin" and "[PATH] … omit to read a manifest from stdin".
//! The CURRENT binary does NOT honor that: when invoked with no PATH it routes
//! through `resolve_root(None)` which falls back to `current_dir()`, so it walks
//! the CWD and IGNORES stdin entirely. Consequences:
//!
//!   * `snapdir id` reading "from stdin" is non-deterministic for a FIXED stdin
//!     input — its result is the id of whatever the CWD happens to be.
//!   * `snapdir manifest <dir> | snapdir id` never round-trips to
//!     `snapdir id <dir>` (it hashes the CWD, not the piped manifest).
//!
//! The snapshot-id spec (`snapdir-core::merkle::snapshot_id`) is
//! `manifest | grep -v '^#' | b3sum --no-names` over the manifest text plus the
//! `echo` trailing newline. So the INVARIANT the stdin path must satisfy is:
//!
//!   id(stdin = `snapdir manifest <dir>`)  ==  `snapdir id <dir>`
//!
//! and it must depend ONLY on the stdin bytes (not on the CWD).
//!
//! These tests are authored to FAIL against the current binary and PASS once the
//! `id` command's stdin-read path is implemented (lane: cli stdin-read). Do not
//! weaken them to pass.
//!
//! Conventions mirror `tests/dx_args.rs`: drive the built binary with
//! `assert_cmd`, pin the cache under a tempdir, build a hermetic fixture tree.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` with the cache pinned so tests never touch the real cache.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd
}

/// Builds a known tiny tree with explicit perms so `id`/`manifest` reproduce.
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

/// Run `snapdir id <args>` from `cwd`, feeding `stdin_bytes` on stdin; returns
/// trimmed stdout. Asserts exit success.
fn id_with_stdin(cache: &Path, cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> String {
    let mut child = snapdir(cache)
        .arg("id")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn snapdir id");
    child.stdin.take().unwrap().write_all(stdin_bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "`snapdir id` (stdin) must succeed; stderr would be on null"
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Compute the deterministic `snapdir manifest <tree>` text and `snapdir id
/// <tree>` for the fixture, returning `(manifest_bytes, id_dir)`.
fn manifest_and_id(cache: &Path, tree: &Path) -> (Vec<u8>, String) {
    let man = snapdir(cache).arg("manifest").arg(tree).output().unwrap();
    assert!(man.status.success(), "manifest <tree> must succeed");
    let id_dir = snapdir(cache).arg("id").arg(tree).output().unwrap();
    assert!(id_dir.status.success(), "id <tree> must succeed");
    let id = String::from_utf8(id_dir.stdout).unwrap().trim().to_owned();
    (man.stdout, id)
}

/// INVARIANT 1 — fixed-input determinism: feeding the SAME manifest bytes on
/// stdin must yield the SAME id regardless of the process CWD. The current
/// binary fails this because it walks the CWD and ignores stdin.
#[test]
fn id_from_stdin_depends_only_on_stdin_not_cwd() {
    let cache = TempDir::new().unwrap();
    let tree = TempDir::new().unwrap();
    build_tree(&tree);

    // Two unrelated, NON-EMPTY working directories with different contents.
    let cwd_a = TempDir::new().unwrap();
    cwd_a.child("alpha.txt").write_str("A").unwrap();
    let cwd_b = TempDir::new().unwrap();
    cwd_b.child("beta.txt").write_str("BBBB").unwrap();

    let (manifest_bytes, _id_dir) = manifest_and_id(cache.path(), tree.path());

    // Distinct cold caches per run to rule out any cache leakage.
    let c1 = TempDir::new().unwrap();
    let c2 = TempDir::new().unwrap();
    let from_a = id_with_stdin(c1.path(), cwd_a.path(), &[], &manifest_bytes);
    let from_b = id_with_stdin(c2.path(), cwd_b.path(), &[], &manifest_bytes);

    assert_eq!(
        from_a, from_b,
        "`snapdir id` on FIXED stdin must be CWD-independent, \
         but got {from_a} (cwd_a) vs {from_b} (cwd_b) — stdin is being ignored"
    );
}

/// INVARIANT 2 — round-trip: `snapdir manifest <tree> | snapdir id` must equal
/// `snapdir id <tree>` (the snapshot-id spec hashes the #-stripped manifest
/// text). The current binary fails this because it hashes the CWD.
#[test]
fn manifest_piped_to_id_round_trips_to_id_dir() {
    let cache = TempDir::new().unwrap();
    let tree = TempDir::new().unwrap();
    build_tree(&tree);

    // A CWD that is deliberately NOT the fixture tree, to expose the cwd-walk bug.
    let other_cwd = TempDir::new().unwrap();
    other_cwd.child("noise.txt").write_str("noise").unwrap();

    let (manifest_bytes, id_dir) = manifest_and_id(cache.path(), tree.path());

    let c = TempDir::new().unwrap();
    let piped = id_with_stdin(c.path(), other_cwd.path(), &[], &manifest_bytes);

    assert_eq!(
        piped, id_dir,
        "`manifest <tree> | id` ({piped}) must round-trip to `id <tree>` ({id_dir})"
    );
}

// ---------------------------------------------------------------------------
// Impl-revealed cases (gate `dx-id-stdin-review`, phase 30).
//
// The landed handler (`Command::Id`, cli `dc6b389`) has exactly three branches:
//   1. `path.is_none() && !stdin.is_terminal()` -> read stdin to a String,
//      `Manifest::parse` (strips empty + `#` lines), then frozen `snapshot_id`.
//   2. `path.is_none() && stdin.is_terminal()`  -> loud `bail!` (no cwd walk).
//   3. else                                     -> walk the given PATH.
//
// These tests pin every observable consequence of that. They are written to
// PASS against the current binary; a failure is a REAL BUG, not a test to relax.
// ---------------------------------------------------------------------------

/// Run `snapdir id <args>` from `cwd`, feeding `stdin` from the given
/// `Stdio` source; returns the full `Output` (status + stdout + stderr) so the
/// error-path tests can assert on the exit code AND the message. Unlike
/// `id_with_stdin`, this does NOT assert success.
fn id_run_raw(
    cache: &Path,
    cwd: &Path,
    args: &[&str],
    stdin: Stdio,
    stdin_bytes: Option<&[u8]>,
) -> std::process::Output {
    let mut child = snapdir(cache)
        .arg("id")
        .args(args)
        .current_dir(cwd)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn snapdir id");
    if let Some(bytes) = stdin_bytes {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    child.wait_with_output().unwrap()
}

/// KEYSTONE (strengthened round-trip): `manifest <dir> | id` == `id <dir>`,
/// byte-identical, with the pipe driven from an UNRELATED cwd. This makes the
/// cwd-independence of the round-trip EXPLICIT: the producing `manifest <dir>`
/// and the consuming `id` share the same fixed bytes, but the `id` process runs
/// in a cwd that is neither the fixture tree nor empty — yet still reproduces
/// `id <dir>` exactly. (A regression to the cwd-walk bug would hash the cwd.)
#[test]
fn round_trip_keystone_is_cwd_independent_and_byte_identical() {
    let cache = TempDir::new().unwrap();
    let tree = TempDir::new().unwrap();
    build_tree(&tree);

    // An unrelated cwd with its own distinct, non-empty contents.
    let unrelated_cwd = TempDir::new().unwrap();
    unrelated_cwd
        .child("decoy/keystone.txt")
        .write_str("totally different bytes")
        .unwrap();

    let (manifest_bytes, id_dir) = manifest_and_id(cache.path(), tree.path());

    // Fresh cold cache for the consuming `id`, run from the unrelated cwd.
    let c = TempDir::new().unwrap();
    let piped = id_with_stdin(c.path(), unrelated_cwd.path(), &[], &manifest_bytes);

    assert_eq!(
        piped, id_dir,
        "`manifest <dir> | id` from an unrelated cwd must be byte-identical to \
         `id <dir>`: got {piped} vs {id_dir}"
    );
    // And the keystone id is non-empty hex (sanity: not a blank line).
    assert_eq!(
        id_dir.len(),
        64,
        "snapshot id must be 64 hex chars: {id_dir}"
    );
    assert!(
        id_dir.bytes().all(|b| b.is_ascii_hexdigit()),
        "snapshot id must be hex: {id_dir}"
    );
}

/// TTY / no-input: a bare `snapdir id` with NO PATH and stdin NOT a pipe must
/// fail loudly (nonzero + a helpful message) rather than silently walking the
/// cwd. We simulate "no manifest on stdin" with `/dev/null`.
///
/// IMPL NOTE (pinned, surprising): the handler keys off `is_terminal()`, NOT
/// off emptiness. `/dev/null` is NOT a terminal, so it takes branch 1 and
/// parses the empty string -> the EMPTY-MANIFEST id (`snapshot_id` of the empty
/// manifest = b3sum of a single "\n"). It is therefore a clean, deterministic
/// result and crucially NOT a cwd-derived walk. We assert exactly that: success
/// with the empty-manifest id, and that the id does NOT equal the id of the
/// (non-empty) cwd.
#[test]
fn id_no_path_with_dev_null_is_empty_manifest_id_not_cwd_walk() {
    let cache = TempDir::new().unwrap();

    // A non-empty cwd whose own `id` we can compute to prove we did NOT walk it.
    let cwd = TempDir::new().unwrap();
    cwd.child("walked.txt")
        .write_str("if you see this id, you walked the cwd")
        .unwrap();
    let cwd_id = {
        let out = snapdir(cache.path())
            .arg("id")
            .arg(cwd.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };

    // The empty-manifest id: parse("") -> empty manifest -> snapshot_id.
    let empty_cache = TempDir::new().unwrap();
    let empty_id = id_with_stdin(empty_cache.path(), cwd.path(), &[], b"");

    let c = TempDir::new().unwrap();
    let out = id_run_raw(
        c.path(),
        cwd.path(),
        &[],
        Stdio::null(), // /dev/null: not a TTY, not a pipe-with-bytes
        None,
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let got = stdout.trim().to_owned();

    assert!(
        out.status.success(),
        "`id` with /dev/null stdin currently takes the empty-manifest branch \
         and succeeds; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        got, empty_id,
        "`id < /dev/null` must yield the deterministic EMPTY-manifest id, got {got}"
    );
    assert_ne!(
        got, cwd_id,
        "`id < /dev/null` must NOT walk the cwd (cwd id was {cwd_id})"
    );
}

/// Empty stdin (an explicitly closed/empty PIPE, distinct from `/dev/null`):
/// deterministic result. Pinned actual behavior: parse("") -> empty manifest
/// id, exit 0 — NOT a cwd walk. Identical to the `/dev/null` case, proving the
/// handler depends only on the (empty) byte stream.
#[test]
fn id_empty_piped_stdin_is_deterministic_empty_manifest_id() {
    let cache = TempDir::new().unwrap();

    // Non-empty cwd, again to rule out a silent walk.
    let cwd = TempDir::new().unwrap();
    cwd.child("noise.txt")
        .write_str("noise noise noise")
        .unwrap();
    let cwd_id = {
        let out = snapdir(cache.path())
            .arg("id")
            .arg(cwd.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };

    let c1 = TempDir::new().unwrap();
    let c2 = TempDir::new().unwrap();
    // Two independent runs of empty piped input must agree (determinism).
    let first = id_with_stdin(c1.path(), cwd.path(), &[], b"");
    let second = id_with_stdin(c2.path(), cwd.path(), &[], b"");

    assert_eq!(
        first, second,
        "empty piped stdin must be deterministic: {first} vs {second}"
    );
    assert_ne!(
        first, cwd_id,
        "empty piped stdin must NOT produce the cwd id ({cwd_id})"
    );
}

/// Trailing newline: the frozen `snapshot_id` contract parses the manifest with
/// `str::lines()` (which ignores a trailing newline) and then appends exactly
/// one `\n` before hashing. So a manifest piped WITH vs WITHOUT a single
/// trailing newline must yield the SAME id, and that id must equal `id <dir>`.
/// This is what lets `manifest <dir> | id` round-trip even though `manifest`
/// emits a trailing newline.
#[test]
fn id_trailing_newline_is_normalized_to_snapshot_id_contract() {
    let cache = TempDir::new().unwrap();
    let tree = TempDir::new().unwrap();
    build_tree(&tree);

    let (manifest_bytes, id_dir) = manifest_and_id(cache.path(), tree.path());

    // `manifest <dir>` ends in exactly one '\n'; strip it to get the
    // no-trailing-newline form (without disturbing internal line breaks).
    let mut no_nl = manifest_bytes.clone();
    assert_eq!(
        no_nl.last(),
        Some(&b'\n'),
        "`manifest <dir>` output is expected to end with a single newline"
    );
    no_nl.pop();
    assert_ne!(
        no_nl.last(),
        Some(&b'\n'),
        "fixture must not end with a blank line; stripping one '\\n' must leave a content line"
    );

    let with_nl = manifest_bytes; // as emitted, trailing '\n' present

    let c1 = TempDir::new().unwrap();
    let c2 = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let id_with = id_with_stdin(c1.path(), cwd.path(), &[], &with_nl);
    let id_without = id_with_stdin(c2.path(), cwd.path(), &[], &no_nl);

    assert_eq!(
        id_with, id_without,
        "trailing newline must be normalized: with-nl {id_with} vs without-nl {id_without}"
    );
    assert_eq!(
        id_with, id_dir,
        "the piped-manifest id must equal `id <dir>` per the frozen snapshot_id contract"
    );
}

/// `#`-comment lines: the snapshot id is computed over the comment-STRIPPED
/// manifest (`Manifest::parse` drops `^#` lines, mirroring `id <dir>`). So
/// adding or removing `#`-comment lines on stdin must NOT change the id, and it
/// must still equal `id <dir>`.
#[test]
fn id_comment_lines_are_stripped_and_do_not_affect_the_id() {
    let cache = TempDir::new().unwrap();
    let tree = TempDir::new().unwrap();
    build_tree(&tree);

    let (manifest_bytes, id_dir) = manifest_and_id(cache.path(), tree.path());

    // Build a comment-laden variant: a leading comment, then the real manifest,
    // then a trailing comment. None of these may perturb the id.
    let mut commented = Vec::new();
    commented.extend_from_slice(b"# snapdir manifest header comment\n");
    commented.extend_from_slice(b"# generated-by: adversary review fixture\n");
    commented.extend_from_slice(&manifest_bytes);
    commented.extend_from_slice(b"# trailing comment after the entries\n");

    let cwd = TempDir::new().unwrap();
    let c1 = TempDir::new().unwrap();
    let c2 = TempDir::new().unwrap();
    let id_plain = id_with_stdin(c1.path(), cwd.path(), &[], &manifest_bytes);
    let id_commented = id_with_stdin(c2.path(), cwd.path(), &[], &commented);

    assert_eq!(
        id_plain, id_commented,
        "adding/removing `#`-comment lines must NOT change the id: \
         plain {id_plain} vs commented {id_commented}"
    );
    assert_eq!(
        id_commented, id_dir,
        "the comment-stripped piped id must equal `id <dir>` ({id_dir})"
    );
}

/// Malformed manifest: garbage on stdin must produce a CLEAN nonzero error
/// (a parse-error message on stderr), NOT a panic and NOT a cwd-derived id.
/// Pinned: the handler wraps the parse failure with `parsing manifest from
/// stdin` context and bails (exit 1, nothing on stdout).
#[test]
fn id_malformed_stdin_errors_cleanly_without_cwd_fallback() {
    let cache = TempDir::new().unwrap();

    // Non-empty cwd so a silent walk, if it happened, would print a real id.
    let cwd = TempDir::new().unwrap();
    cwd.child("present.txt").write_str("present").unwrap();
    let cwd_id = {
        let out = snapdir(cache.path())
            .arg("id")
            .arg(cwd.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };

    let c = TempDir::new().unwrap();
    let out = id_run_raw(
        c.path(),
        cwd.path(),
        &[],
        Stdio::piped(),
        Some(b"this is not a manifest @@@\nrandom !!! garbage\n"),
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "malformed stdin must produce a nonzero exit; stdout={stdout:?} stderr={stderr}"
    );
    // No panic / abort: a clean error exits with a code (not a SIGABRT/SIGSEGV).
    assert!(
        out.status.code().is_some(),
        "malformed stdin must exit cleanly (a code), not via a signal/panic; stderr={stderr}"
    );
    assert!(
        stderr.contains("manifest") || stderr.contains("stdin") || stderr.contains("parse"),
        "malformed stdin must emit a parse-error message; got stderr: {stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "malformed stdin must NOT print an id on stdout; got: {stdout:?}"
    );
    assert!(
        !stdout.contains(&cwd_id),
        "malformed stdin must NOT fall back to the cwd id ({cwd_id})"
    );
}
