//! Black-box spec suite for the 1.8.0 HELP-TEXT + ERROR-DISCOVERABILITY contract
//! (phase 30, gate `dx-helptext-spec-tests`).
//!
//! AUTHORED FROM THE SPEC ONLY. This suite pins the TWO findings the independent
//! loop-closer (`dx-fix-verify`) caught as still NOT-RESOLVED — both were accepted
//! in the findings report (cluster 4 / §5) but the earlier `dx-errors-spec-tests`
//! never pinned them, so they were never implemented. It is staged in
//! `.gatesmith/pending-tests/` so the workspace keeps compiling; the impl teammate
//! moves it to `crates/snapdir-cli/tests/dx_helptext.rs` and wires it. BOTH tests
//! are EXPECTED TO FAIL against the current binary — that is the point. They must
//! not be weakened to pass.
//!
//! SPEC under test (two clauses, each test comments the clause it pins)
//! ===================================================================
//!  (1) verify --help MUST NOT FALSELY SAY "staged" — today `snapdir verify
//!      --help` summarizes the verb as "Verify the integrity of a *staged*
//!      snapshot", but `verify` actually requires `--store`/`--id` and checks the
//!      STORE. That is false reassurance (it reads like it inspects the local
//!      staging area). CONTRACT: `verify --help` stdout must NOT contain the word
//!      "staged" (case-insensitive), AND must accurately indicate it verifies a
//!      snapshot in a/the STORE (mention "store").
//!  (2) file:// SCHEME MUST BE DISCOVERABLE ON AN INVALID-PROTOCOL ERROR — today a
//!      store value that is not a recognized `scheme://...` URI (e.g. a bare path
//!      with no scheme) errors `invalid store protocol: '<x>'` listing NO valid
//!      schemes, so a user has no clue what to type. CONTRACT: that error (stderr)
//!      must LIST the valid scheme(s) and include `file://`.
//!
//! These drive the REAL `snapdir` binary; every test is hermetic (per-test temp
//! cache + temp tree, env store vars REMOVED so the developer's env cannot mask a
//! bug). Substance is pinned with case-insensitive line/stdout-contains so the impl
//! keeps wording latitude — only the load-bearing tokens are pinned.

// The crate enables `clippy::pedantic` workspace-wide; suppress test-only
// stylistic lints (mirroring the `#![allow(...)]` in sibling suites like
// `dx_errors.rs`) so the staged suite compiles under `-D warnings` WITHOUT
// touching any assertion or behavior.
#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::doc_markdown
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ===========================================================================
// Harness (mirrors crates/snapdir-cli/tests/dx_errors.rs)
// ===========================================================================

/// Path to the compiled `snapdir` binary under test.
fn snapdir_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// A unique temp directory; created and returned. `tag` only aids debugging.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-dxhelp-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Runs `snapdir <args>` with the cache pinned under `cache` and the store/objects
/// env vars REMOVED (so the developer's env cannot mask a bug). Returns the raw
/// `Output`.
fn run_raw(args: &[&str], cache: &Path) -> Output {
    Command::new(snapdir_bin())
        .args(args)
        .env("SNAPDIR_CACHE_DIR", cache)
        .env_remove("SNAPDIR_STORE")
        .env_remove("SNAPDIR_OBJECTS_STORE")
        .output()
        .expect("run snapdir")
}

/// stdout of an `Output`, lossy.
fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// stderr of an `Output`, lossy.
fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Builds a tree with deterministic perms so it manifests to a stable id.
fn build_tree(dir: &Path, leaves: &[(&str, &[u8])]) {
    for (rel, bytes) in leaves {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
}

// ===========================================================================
// (1) verify --help MUST NOT FALSELY SAY "staged" — and must mention the STORE.
//
// Current behavior (confirmed by running `snapdir verify --help`): the very first
// summary line is exactly
//     Verify the integrity of a staged snapshot
// and the word "store" appears ONLY inside the `--store <URI>` option block, not in
// the verb description. So:
//   * the `!contains("staged")` assertion is EXPECTED TO FAIL (the word is present);
//   * the `contains("store")` assertion happens to pass today only because of the
//     `--store` option block — that is fine, the load-bearing failure is the false
//     "staged" claim. Both are pinned so the fixed wording must describe a STORE
//     verification accurately.
// ===========================================================================

/// Clause 1: `verify --help` stdout must NOT contain the word "staged"
/// (case-insensitive) — `verify` checks a snapshot in the STORE (it requires
/// `--store`/`--id`), so calling it a "staged snapshot" is false reassurance — AND
/// the help must accurately indicate it verifies a snapshot in a/the STORE (mention
/// "store"). Pins both: no "staged", and mentions store.
#[test]
fn verify_help_does_not_claim_staged_and_mentions_store() {
    let cache = temp_dir("verify-help");
    let out = run_raw(&["verify", "--help"], &cache);
    assert!(
        out.status.success(),
        "`verify --help` must exit 0; stderr: {}",
        stderr_of(&out)
    );
    let help = stdout_of(&out);
    let help_lc = help.to_lowercase();

    // (a) The word "staged" must NOT appear anywhere in `verify --help`: `verify`
    // operates on a snapshot in the STORE, not on the local staging area, so
    // "staged snapshot" is a false description. (Current binary's summary line is
    // "Verify the integrity of a staged snapshot" -> EXPECTED TO FAIL.)
    assert!(
        !help_lc.contains("staged"),
        "`verify --help` must NOT describe the snapshot as \"staged\" \
         (verify checks the STORE, requiring --store/--id; \"staged\" is false \
         reassurance). Full help:\n{help}"
    );

    // (b) The help must accurately indicate it verifies a snapshot in a/the STORE.
    assert!(
        help_lc.contains("store"),
        "`verify --help` must indicate it verifies a snapshot in the STORE \
         (mention \"store\"). Full help:\n{help}"
    );
}

// ===========================================================================
// (2) file:// SCHEME MUST BE DISCOVERABLE ON AN INVALID-PROTOCOL ERROR.
//
// Current behavior (confirmed by running the binary): a `--store` value that is not
// a recognized `scheme://...` URI — e.g. a bare absolute path with no scheme — is
// rejected with exactly
//     resolving --store protocol: invalid store protocol: '/no/scheme/here'
// which lists NO valid schemes, so the user has no idea what to type. The primary
// contract is that THIS ERROR must name the valid scheme(s) and include `file://`.
//
// We construct an invocation that reaches the store-protocol resolver: a transfer
// command (`push`) with a `--store` value that has no recognized scheme. A bare
// absolute path (leading slash, no `://`) is what triggers the
// `invalid store protocol` branch (a `scheme://` value instead routes to a backend
// spawn, a different error), so we pin on the bare-path form.
// ===========================================================================

/// Clause 2 (primary): a `push` whose `--store` is an unrecognized protocol must
/// FAIL with the `invalid store protocol` error, and that error (stderr) must LIST
/// the valid scheme(s) and include `file://` so the user learns what to type.
/// (Current binary prints `invalid store protocol: '<x>'` with NO schemes listed
/// -> EXPECTED TO FAIL.)
#[test]
fn invalid_store_protocol_error_lists_file_scheme() {
    let cache = temp_dir("badproto-push-cache");
    let src = temp_dir("badproto-push-src");
    build_tree(&src, &[("a.txt", b"hello")]);
    let src_str = src.to_string_lossy().into_owned();

    // A bare absolute path with no scheme reaches the protocol resolver and is
    // rejected as an invalid store protocol (a `scheme://` value would instead try
    // to spawn a backend binary — a different code path).
    let out = run_raw(&["push", "--store", "/no/scheme/here", &src_str], &cache);
    assert!(
        !out.status.success(),
        "push with an unrecognized --store protocol must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    let err_lc = err.to_lowercase();

    // Sanity: we really hit the invalid-protocol branch (not some other failure).
    assert!(
        err_lc.contains("invalid store protocol") || err_lc.contains("protocol"),
        "expected the invalid-store-protocol error; got: {err}"
    );

    // The load-bearing contract: the error must name `file://` so the user knows a
    // valid scheme to type. (Pinned case-insensitively on the literal `file://`.)
    assert!(
        err_lc.contains("file://"),
        "the invalid-store-protocol error must LIST the valid scheme(s) and \
         include `file://` so the user can discover what to type; got: {err}"
    );
}

/// Clause 2 (mirror, sync `--to`): the same discoverability must hold on the
/// transfer-write side. `sync --to <unrecognized>` reaches the SAME protocol
/// resolver (error prefixed `resolving --to store protocol:`), so its
/// `invalid store protocol` error must ALSO list `file://`. This rules out the fix
/// being wired into only one call site.
#[test]
fn invalid_store_protocol_error_on_sync_to_lists_file_scheme() {
    let cache = temp_dir("badproto-sync-cache");

    // A real, valid id shape (never pushed) and a valid file:// --from; the ONLY
    // reason to fail here must be the unrecognized --to protocol.
    let id = "0".repeat(64);
    let from = temp_dir("badproto-sync-from");
    let from_url = format!("file://{}", from.display());

    let out = run_raw(
        &[
            "sync",
            "--id",
            &id,
            "--from",
            &from_url,
            "--to",
            "/no/scheme/dest",
        ],
        &cache,
    );
    assert!(
        !out.status.success(),
        "sync with an unrecognized --to protocol must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    let err_lc = err.to_lowercase();

    assert!(
        err_lc.contains("invalid store protocol") || err_lc.contains("protocol"),
        "expected the invalid-store-protocol error on the --to side; got: {err}"
    );
    assert!(
        err_lc.contains("file://"),
        "the invalid-store-protocol error (sync --to) must also list `file://`; \
         got: {err}"
    );
}
