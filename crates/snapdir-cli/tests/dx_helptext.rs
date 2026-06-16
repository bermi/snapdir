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
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
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

// ===========================================================================
// REVIEW-GATE ADDITIONS (phase 30, `dx-helptext-review`) — impl-revealed cases.
//
// The fix landed across two commits: cli `f933ef8` (verify about-line) and stores
// `4039237` (router `RouteError::InvalidProtocol` message-only enrichment). With
// `src/` now visible, the router fix is in ONE place
// (`snapdir-stores::router::RouteError::InvalidProtocol`'s `#[error(...)]`), so it
// fires identically at EVERY store-URI call site. These cases lock that down at
// more sites than the spec pinned, strengthen the verify-help accuracy clause to
// the actual required flags, and pin the bare-path-vs-`scheme://` routing split so
// the two error messages can never be conflated. ALL must PASS against the current
// binary — a failure here is a REAL BUG (the impl gate reopens), never a weakening.
// ===========================================================================

/// Strengthens clause (1): the corrected `verify --help` does not merely mention
/// "store" — it must name BOTH required flags `--store` AND `--id` (the flags
/// `verify` actually needs), and still must NOT say "staged". The landed about-line
/// is `Verify the integrity of a snapshot in a store (requires --store/--id)` and
/// the `--store`/`--id` option blocks reinforce both — so this PASSES today; it
/// guards against a future regression that drops a required flag from the help or
/// reintroduces the false "staged" wording.
#[test]
fn verify_help_names_both_required_flags_and_no_staged() {
    let cache = temp_dir("verify-help-flags");
    let out = run_raw(&["verify", "--help"], &cache);
    assert!(
        out.status.success(),
        "`verify --help` must exit 0; stderr: {}",
        stderr_of(&out)
    );
    let help = stdout_of(&out);
    let help_lc = help.to_lowercase();

    // No false "staged" claim (the load-bearing clause-1 invariant), re-pinned here
    // so this stronger test stands alone.
    assert!(
        !help_lc.contains("staged"),
        "`verify --help` must NOT describe the snapshot as \"staged\"; full help:\n{help}"
    );
    // Names BOTH required flags so the help is actionable, not just \"store\".
    assert!(
        help.contains("--store"),
        "`verify --help` must name the required `--store` flag; full help:\n{help}"
    );
    assert!(
        help.contains("--id"),
        "`verify --help` must name the required `--id` flag; full help:\n{help}"
    );
}

/// Clause-2 extra call site (`sync --from`): the invalid-protocol error must name
/// `file://` on the transfer-READ side too. The router fix is one shared message,
/// so this pins that the `--from` resolver (error prefixed `resolving --from store
/// protocol:`) surfaces it. PASSES today; proves the fix is not wired to only the
/// write side.
#[test]
fn invalid_store_protocol_error_on_sync_from_lists_file_scheme() {
    let cache = temp_dir("badproto-from-cache");

    let id = "0".repeat(64);
    let to = temp_dir("badproto-from-to");
    let to_url = format!("file://{}", to.display());

    // Valid --to (file://), bare-path --from -> the ONLY failure is the --from
    // protocol.
    let out = run_raw(
        &[
            "sync",
            "--id",
            &id,
            "--from",
            "/no/scheme/src",
            "--to",
            &to_url,
        ],
        &cache,
    );
    assert!(
        !out.status.success(),
        "sync with an unrecognized --from protocol must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    let err_lc = err.to_lowercase();
    assert!(
        err_lc.contains("invalid store protocol") || err_lc.contains("protocol"),
        "expected the invalid-store-protocol error on the --from side; got: {err}"
    );
    assert!(
        err_lc.contains("file://"),
        "the invalid-store-protocol error (sync --from) must also list `file://`; got: {err}"
    );
}

/// Clause-2 extra call site (`push --objects-store`): the shared-pool objects-store
/// URI flows through the SAME protocol resolver, so an unrecognized `--objects-store`
/// must also fail with the `file://`-naming error (prefixed `resolving
/// --objects-store protocol:`). PASSES today; pins the fourth store-URI site.
#[test]
fn invalid_objects_store_protocol_error_lists_file_scheme() {
    let cache = temp_dir("badproto-obj-cache");
    let src = temp_dir("badproto-obj-src");
    build_tree(&src, &[("a.txt", b"hello")]);
    let src_str = src.to_string_lossy().into_owned();

    // Valid --store (file://) so the only unrecognized protocol is --objects-store.
    let store = temp_dir("badproto-obj-store");
    let store_url = format!("file://{}", store.display());

    let out = run_raw(
        &[
            "push",
            "--store",
            &store_url,
            "--objects-store",
            "/no/scheme/obj",
            &src_str,
        ],
        &cache,
    );
    assert!(
        !out.status.success(),
        "push with an unrecognized --objects-store protocol must fail; stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    let err_lc = err.to_lowercase();
    assert!(
        err_lc.contains("invalid store protocol") || err_lc.contains("protocol"),
        "expected the invalid-store-protocol error on the --objects-store side; got: {err}"
    );
    assert!(
        err_lc.contains("file://"),
        "the invalid-store-protocol error (--objects-store) must also list `file://`; got: {err}"
    );
}

/// Routing split — the two error messages must NOT collide. The router validates
/// the scheme against `^[a-z0-9]*$` (see `snapdir-stores::router::store_protocol`):
///   * a BARE, scheme-less path (`/no/scheme/here`) fails that validation -> the
///     `InvalidProtocol` error that NAMES `file://`;
///   * a well-formed UNKNOWN `scheme://` (`notaproto://x`) PASSES validation and
///     routes to `Adapter::External` -> the shim tries to spawn
///     `snapdir-notaproto-store`, a DIFFERENT error (`failed to spawn store
///     binary 'snapdir-notaproto-store'`) that mentions NEITHER "invalid store
///     protocol" NOR `file://`.
/// Pins BOTH halves so a future change can't route a bare path into the spawn path
/// (losing the `file://` hint) or fold an unknown scheme into the invalid-protocol
/// message (a misleading "use file://" for a perfectly well-formed `s4://`-style URI).
#[test]
fn bare_path_vs_unknown_scheme_route_to_distinct_errors() {
    // Half 1: bare scheme-less path -> invalid-protocol error naming file://.
    let bare_cache = temp_dir("split-bare-cache");
    let bare_src = temp_dir("split-bare-src");
    build_tree(&bare_src, &[("a.txt", b"hi")]);
    let bare_src_str = bare_src.to_string_lossy().into_owned();
    let bare = run_raw(
        &["push", "--store", "/no/scheme/here", &bare_src_str],
        &bare_cache,
    );
    assert!(
        !bare.status.success(),
        "bare-path --store must fail; stderr: {}",
        stderr_of(&bare)
    );
    let bare_err = stderr_of(&bare);
    let bare_err_lc = bare_err.to_lowercase();
    assert!(
        bare_err_lc.contains("invalid store protocol"),
        "a bare scheme-less path must hit the invalid-store-protocol branch; got: {bare_err}"
    );
    assert!(
        bare_err_lc.contains("file://"),
        "the bare-path invalid-protocol error must name `file://`; got: {bare_err}"
    );

    // Half 2: well-formed unknown scheme -> external-helper spawn path, a DIFFERENT
    // error. `PATH` is irrelevant here: even if some `snapdir-notaproto-store`
    // existed, the message would not be the invalid-protocol one — the point is the
    // route differs. We assert the message is NOT the invalid-protocol message and
    // does NOT carry the `file://` hint (so the two never collide), and that the
    // adapter name appears.
    let ext_cache = temp_dir("split-ext-cache");
    let ext_src = temp_dir("split-ext-src");
    build_tree(&ext_src, &[("a.txt", b"hi")]);
    let ext_src_str = ext_src.to_string_lossy().into_owned();
    let ext = run_raw(
        &["push", "--store", "notaproto://x", &ext_src_str],
        &ext_cache,
    );
    assert!(
        !ext.status.success(),
        "unknown scheme push must still fail (no such helper); stderr: {}",
        stderr_of(&ext)
    );
    let ext_err = stderr_of(&ext);
    let ext_err_lc = ext_err.to_lowercase();
    assert!(
        !ext_err_lc.contains("invalid store protocol"),
        "a well-formed unknown `scheme://` must NOT hit the invalid-store-protocol \
         branch (it is a valid scheme that routes to an external helper); got: {ext_err}"
    );
    assert!(
        !ext_err_lc.contains("file://"),
        "the unknown-scheme route must NOT carry the `file://` invalid-protocol hint \
         (it would wrongly suggest a local store for a well-formed remote URI); got: {ext_err}"
    );
    assert!(
        ext_err_lc.contains("notaproto"),
        "the unknown-scheme error should reference the requested adapter \
         (`snapdir-notaproto-store`); got: {ext_err}"
    );
}
