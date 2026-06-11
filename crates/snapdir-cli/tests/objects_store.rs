//! Black-box spec suite for the global `--objects-store <URI>` / env
//! `$SNAPDIR_OBJECTS_STORE` CLI flag (phase 28, gate
//! `objects-store-cli-spec-tests`).
//!
//! AUTHORED FROM THE SPEC ONLY — the `--objects-store` CLI wiring does NOT exist
//! yet, so this suite is EXPECTED to fail until the impl lands. It is staged in
//! `.gatesmith/pending-tests/` so the workspace keeps compiling; the cli impl
//! teammate moves it to `crates/snapdir-cli/tests/objects_store.rs` and wires it.
//!
//! SPEC under test
//! ===============
//! A new GLOBAL flag `--objects-store <URI>` / env `$SNAPDIR_OBJECTS_STORE` holds
//! a SHARED object pool, while `--store <URI>` holds the (per-capture) MANIFEST
//! location. When `--objects-store` is set, `push`/`fetch`/`pull` route content
//! OBJECTS to the pool's `.objects/` and MANIFESTS to `--store`'s `.manifests/`
//! (via the already-landed `SplitStore`). When UNSET, behavior is byte-for-byte
//! unchanged (the colocated store as today).
//!
//! These are end-to-end CLI tests driving the REAL `snapdir` binary over
//! `file://` stores with NO credentials, mirroring the existing CLI harness
//! conventions (`store_roundtrip.rs`, `store_env.rs`, `catalog_commands.rs`).
//!
//! Note on arg shape: the SPEC says `--objects-store` is a GLOBAL flag, so it is
//! accepted both BEFORE the subcommand (`snapdir --objects-store … push …`) and
//! after (`snapdir push … --objects-store …`). The existing suites mix both
//! orderings; clap global flags accept either. The impl teammate may adjust the
//! exact call shape, but must preserve the BEHAVIORS pinned here.

// The crate enables `clippy::pedantic` workspace-wide; these test-only stylistic
// lints (mirroring the `#[allow(...)]` in sibling suites like `progress_e2e.rs`)
// are suppressed so the staged suite compiles under `-D warnings` WITHOUT
// touching any assertion or behavior.
#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::manual_let_else
)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use snapdir_core::{Blake3Hasher, Hasher};

/// Path to the compiled `snapdir` binary under test (the bin target lives in the
/// `snapdir` crate; `assert_cmd` resolves it from the shared target dir).
fn snapdir_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// A unique temp directory; created and returned.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-objstore-{tag}-{}-{:?}",
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

/// Runs the `snapdir` binary with the cache pinned under `cache` and
/// `SNAPDIR_OBJECTS_STORE`/`SNAPDIR_STORE` REMOVED (so the developer's env can't
/// mask a bug); tests that exercise env-equivalence re-add them via `extra_env`.
/// Returns the raw `Output`.
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

/// Runs `snapdir <args>`, asserts success, returns trimmed stdout.
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

/// `.objects/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>` — the frozen sharded layout.
fn sharded(prefix: &str, hex: &str) -> String {
    format!(
        "{prefix}/{}/{}/{}/{}",
        &hex[0..3],
        &hex[3..6],
        &hex[6..9],
        &hex[9..]
    )
}

/// Recursively collects every regular FILE under `dir` (the leaf object/manifest
/// blobs), returned as paths relative to `dir`. Returns empty if `dir` is absent.
fn collect_files(dir: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
        let rd = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_dir() {
                walk(base, &p, out);
            } else if ft.is_file() {
                out.insert(p.strip_prefix(base).unwrap().to_path_buf());
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

/// Number of leaf blobs under `<pool>/.objects/`.
fn count_pool_objects(pool: &Path) -> usize {
    collect_files(&pool.join(".objects")).len()
}

/// Builds a known tree with explicit, deterministic permissions so a checked-out
/// copy must restore them to re-manifest to the same id. `leaves` is a slice of
/// `(relative_path, contents)`.
fn build_tree(dir: &Path, leaves: &[(&str, &[u8])]) {
    for (rel, bytes) in leaves {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    // Normalize directory perms top-down so the id is deterministic.
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

/// Asserts the tree at `dest` reproduces every `(rel, bytes)` leaf in `expected`.
fn assert_tree_contents(dest: &Path, expected: &[(&str, &[u8])]) {
    for (rel, bytes) in expected {
        let got = fs::read(dest.join(rel))
            .unwrap_or_else(|e| panic!("read {rel} from dest {}: {e}", dest.display()));
        assert_eq!(&got[..], *bytes, "contents of {rel} must match source");
    }
}

// ===========================================================================
// (a) SHARED POOL, MANY MANIFEST LOCATIONS — one blob copy, both manifests pull
// ===========================================================================

/// SPEC (a): pushing the SAME tree with the SAME `--objects-store` pool but TWO
/// DIFFERENT `--store` manifest locations stores exactly ONE copy of each blob in
/// the pool (no duplication across captures), and BOTH manifests resolve and
/// `pull` to byte-identical trees.
#[test]
fn shared_pool_dedupes_objects_across_two_manifest_locations() {
    let src = temp_dir("a-src");
    let pool = temp_dir("a-pool");
    let cap_a = temp_dir("a-capA");
    let cap_b = temp_dir("a-capB");
    let cache = temp_dir("a-cache");

    let leaves: &[(&str, &[u8])] = &[
        ("a.txt", b"hello"),
        ("sub/b.txt", b"world!!"),
        ("sub/c.txt", b"another blob"),
    ];
    build_tree(&src, leaves);

    let src_str = src.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let cap_a_url = file_url(&cap_a);
    let cap_b_url = file_url(&cap_b);

    // The id the source manifests to, independent of any store.
    let src_id = run_ok(&["id", &src_str], &cache, &[]);
    assert_eq!(src_id.len(), 64, "snapshot id is 64 hex chars");

    // Capture A: objects -> pool, manifest -> cap-A.
    let id_a = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_a_url,
            &src_str,
        ],
        &cache,
        &[],
    );
    assert_eq!(id_a, src_id, "push must print the source snapshot id");

    let pool_after_a = count_pool_objects(&pool);
    assert_eq!(
        pool_after_a, 3,
        "pool must hold exactly one blob per distinct file (3), not duplicates"
    );

    // The manifest landed in cap-A's `.manifests/`, NOT in the pool.
    assert!(
        cap_a.join(sharded(".manifests", &id_a)).is_file(),
        "manifest must land in --store (cap-A) .manifests/"
    );
    assert!(
        !pool.join(sharded(".manifests", &id_a)).exists(),
        "manifest must NOT be written into the shared pool"
    );

    // The objects landed in the POOL's `.objects/`, NOT in cap-A.
    for (_, bytes) in leaves {
        let sum = Blake3Hasher::new().hash_hex(bytes);
        assert!(
            pool.join(sharded(".objects", &sum)).is_file(),
            "object must land in the pool .objects/"
        );
    }
    assert!(
        collect_files(&cap_a.join(".objects")).is_empty(),
        "no objects may be written into the manifest-only --store (cap-A)"
    );

    // Capture B: SAME tree, SAME pool, DIFFERENT manifest store cap-B.
    let id_b = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_b_url,
            &src_str,
        ],
        &cache,
        &[],
    );
    assert_eq!(id_b, src_id, "identical tree must produce the identical id");

    // The pool STILL holds exactly one copy of each blob: zero re-upload.
    assert_eq!(
        count_pool_objects(&pool),
        3,
        "second capture into the shared pool must NOT duplicate any blob"
    );

    // Both manifests exist in their own --store locations.
    assert!(
        cap_b.join(sharded(".manifests", &id_b)).is_file(),
        "cap-B manifest must land in cap-B .manifests/"
    );

    // BOTH manifests pull to byte-identical trees (and re-manifest to src id).
    for (cap_url, dest_tag) in [(&cap_a_url, "a-destA"), (&cap_b_url, "a-destB")] {
        let dest = temp_dir(dest_tag);
        let dest_str = dest.to_string_lossy().into_owned();
        run_ok(
            &[
                "pull",
                "--objects-store",
                &pool_url,
                "--store",
                cap_url,
                "--id",
                &src_id,
                &dest_str,
            ],
            &cache,
            &[],
        );
        assert_tree_contents(&dest, leaves);
        assert_eq!(
            run_ok(&["id", &dest_str], &cache, &[]),
            src_id,
            "pulled tree from {cap_url} must re-manifest to the source id"
        );
        fs::remove_dir_all(&dest).ok();
    }

    for d in [&src, &pool, &cap_a, &cap_b, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

/// SPEC (a)/fetch: `fetch` with `--objects-store` populates the cache from the
/// pool's objects + the `--store`'s manifest, and a later offline `checkout`
/// reproduces the tree.
#[test]
fn fetch_with_objects_store_populates_cache_then_checkout() {
    let src = temp_dir("af-src");
    let pool = temp_dir("af-pool");
    let cap = temp_dir("af-cap");
    let dest = temp_dir("af-dest");
    let cache = temp_dir("af-cache");

    let leaves: &[(&str, &[u8])] = &[("only.txt", b"solo")];
    build_tree(&src, leaves);

    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let cap_url = file_url(&cap);

    let id = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_url,
            &src_str,
        ],
        &cache,
        &[],
    );

    // fetch pulls the manifest (from cap) + objects (from pool) into the cache.
    run_ok(
        &[
            "fetch",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_url,
            "--id",
            &id,
        ],
        &cache,
        &[],
    );
    assert!(
        cache.join(sharded(".manifests", &id)).is_file(),
        "fetch must cache the manifest"
    );

    // checkout works offline from the cache only (no store flags needed).
    run_ok(&["checkout", "--id", &id, &dest_str], &cache, &[]);
    assert_tree_contents(&dest, leaves);
    assert_eq!(run_ok(&["id", &dest_str], &cache, &[]), id);

    for d in [&src, &pool, &cap, &dest, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

// ===========================================================================
// (b) BACKWARD COMPAT — colocated store unchanged when --objects-store UNSET
// ===========================================================================

/// SPEC (b): an existing colocated `push`/`pull` with NO `--objects-store` (just
/// `--store`) still works IDENTICALLY to today — both objects AND manifest land
/// in the SAME store, and a round trip reproduces the tree byte-for-byte.
#[test]
fn backward_compat_colocated_store_unchanged_without_objects_store() {
    let src = temp_dir("b-src");
    let store = temp_dir("b-store");
    let dest = temp_dir("b-dest");
    let cache = temp_dir("b-cache");

    let leaves: &[(&str, &[u8])] = &[("a.txt", b"hello"), ("sub/b.txt", b"world!!")];
    build_tree(&src, leaves);

    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();
    let store_url = file_url(&store);

    let src_id = run_ok(&["id", &src_str], &cache, &[]);

    // Colocated push: NO --objects-store.
    let id = run_ok(&["push", "--store", &store_url, &src_str], &cache, &[]);
    assert_eq!(id, src_id);

    // Objects AND manifest both land in the SAME store (colocated, as today).
    assert!(
        store.join(sharded(".manifests", &id)).is_file(),
        "manifest must land colocated in --store"
    );
    for (_, bytes) in leaves {
        let sum = Blake3Hasher::new().hash_hex(bytes);
        assert!(
            store.join(sharded(".objects", &sum)).is_file(),
            "object must land colocated in the SAME --store .objects/"
        );
    }

    // Round-trip: pull (no --objects-store) reproduces the tree + id.
    run_ok(
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
        &cache,
        &[],
    );
    assert_tree_contents(&dest, leaves);
    assert_eq!(
        run_ok(&["id", &dest_str], &cache, &[]),
        src_id,
        "colocated round trip must re-manifest to the source id"
    );

    for d in [&src, &store, &dest, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

// ===========================================================================
// (c) $SNAPDIR_OBJECTS_STORE ENV EQUIVALENCE
// ===========================================================================

/// SPEC (c): `$SNAPDIR_OBJECTS_STORE` is equivalent to the `--objects-store`
/// flag. A push using the ENV (flag omitted) routes objects to the pool exactly
/// like the flag, and a pull using the ENV restores byte-identically.
#[test]
fn env_objects_store_equivalent_to_flag() {
    let src = temp_dir("c-src");
    let pool = temp_dir("c-pool");
    let cap = temp_dir("c-cap");
    let dest = temp_dir("c-dest");
    let cache = temp_dir("c-cache");

    let leaves: &[(&str, &[u8])] = &[("a.txt", b"hello"), ("d.txt", b"distinct")];
    build_tree(&src, leaves);

    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let cap_url = file_url(&cap);

    let src_id = run_ok(&["id", &src_str], &cache, &[]);

    // Push using $SNAPDIR_OBJECTS_STORE (flag omitted); --store still explicit.
    let id = run_ok(
        &["push", "--store", &cap_url, &src_str],
        &cache,
        &[("SNAPDIR_OBJECTS_STORE", &pool_url)],
    );
    assert_eq!(id, src_id, "env-driven push must print the source id");

    // Objects routed to the POOL, manifest to cap — same as the flag.
    for (_, bytes) in leaves {
        let sum = Blake3Hasher::new().hash_hex(bytes);
        assert!(
            pool.join(sharded(".objects", &sum)).is_file(),
            "env $SNAPDIR_OBJECTS_STORE must route objects to the pool"
        );
    }
    assert!(
        cap.join(sharded(".manifests", &id)).is_file(),
        "manifest must land in --store under env equivalence"
    );
    assert_eq!(
        count_pool_objects(&pool),
        2,
        "exactly two distinct blobs land in the env-named pool"
    );

    // Pull using the env restores byte-identically.
    run_ok(
        &["pull", "--store", &cap_url, "--id", &id, &dest_str],
        &cache,
        &[("SNAPDIR_OBJECTS_STORE", &pool_url)],
    );
    assert_tree_contents(&dest, leaves);
    assert_eq!(run_ok(&["id", &dest_str], &cache, &[]), src_id);

    for d in [&src, &pool, &cap, &dest, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

/// SPEC (c): the explicit `--objects-store` FLAG overrides `$SNAPDIR_OBJECTS_STORE`
/// when both are set (flag > env, mirroring `--store`/`$SNAPDIR_STORE`).
#[test]
fn flag_objects_store_overrides_env() {
    let src = temp_dir("ce-src");
    let env_pool = temp_dir("ce-envpool");
    let flag_pool = temp_dir("ce-flagpool");
    let cap = temp_dir("ce-cap");
    let cache = temp_dir("ce-cache");

    let leaves: &[(&str, &[u8])] = &[("a.txt", b"hello")];
    build_tree(&src, leaves);

    let src_str = src.to_string_lossy().into_owned();
    let env_url = file_url(&env_pool);
    let flag_url = file_url(&flag_pool);
    let cap_url = file_url(&cap);

    // ENV names env_pool, FLAG names flag_pool: the flag must win.
    run_ok(
        &[
            "push",
            "--objects-store",
            &flag_url,
            "--store",
            &cap_url,
            &src_str,
        ],
        &cache,
        &[("SNAPDIR_OBJECTS_STORE", &env_url)],
    );

    let sum = Blake3Hasher::new().hash_hex(b"hello");
    assert!(
        flag_pool.join(sharded(".objects", &sum)).is_file(),
        "explicit --objects-store flag must receive the objects"
    );
    assert!(
        !env_pool.join(sharded(".objects", &sum)).exists(),
        "the $SNAPDIR_OBJECTS_STORE pool must be untouched when the flag is explicit"
    );

    for d in [&src, &env_pool, &flag_pool, &cap, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

// ===========================================================================
// FAILURE MODE: wrong/empty pool on read FAILS; right pool succeeds.
// (Proves objects really come from the pool, not the manifest location.)
// ===========================================================================

/// SPEC failure mode: `pull` against a DIFFERENT/EMPTY `--objects-store` than the
/// one the objects were pushed to FAILS (objects-not-found, non-zero exit, clear
/// error) — while the SAME `pull` against the RIGHT pool SUCCEEDS. This proves
/// objects are sourced from the pool, not the manifest `--store`.
#[test]
fn pull_with_wrong_or_empty_pool_fails_right_pool_succeeds() {
    let src = temp_dir("w-src");
    let pool = temp_dir("w-pool");
    let empty_pool = temp_dir("w-emptypool");
    let cap_b = temp_dir("w-capB");
    let cache = temp_dir("w-cache");

    let leaves: &[(&str, &[u8])] = &[("a.txt", b"hello"), ("b.txt", b"second")];
    build_tree(&src, leaves);

    let src_str = src.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let empty_url = file_url(&empty_pool);
    let cap_b_url = file_url(&cap_b);

    // Capture into cap-B with the real pool.
    let id_b = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_b_url,
            &src_str,
        ],
        &cache,
        &[],
    );

    // Fresh cache so the read MUST hit the pool (not a locally cached object).
    // pull --id <B> --store cap-B with the WRONG/EMPTY pool must FAIL.
    let read_cache = temp_dir("w-readcache");
    let dest_bad = temp_dir("w-destbad");
    let out = run_raw(
        &[
            "pull",
            "--objects-store",
            &empty_url,
            "--store",
            &cap_b_url,
            "--id",
            &id_b,
            &dest_bad.to_string_lossy(),
        ],
        &read_cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "pull with an empty/wrong --objects-store must fail (objects not found)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("not found")
            || stderr.contains("missing")
            || stderr.contains("object")
            || stderr.contains("no such"),
        "error must explain objects could not be found in the pool; got: {stderr}"
    );

    // The SAME pull against the RIGHT pool (still a fresh cache) SUCCEEDS.
    let read_cache2 = temp_dir("w-readcache2");
    let dest_ok = temp_dir("w-destok");
    let dest_ok_str = dest_ok.to_string_lossy().into_owned();
    run_ok(
        &[
            "pull",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_b_url,
            "--id",
            &id_b,
            &dest_ok_str,
        ],
        &read_cache2,
        &[],
    );
    assert_tree_contents(&dest_ok, leaves);

    for d in [
        &src,
        &pool,
        &empty_pool,
        &cap_b,
        &cache,
        &read_cache,
        &read_cache2,
        &dest_bad,
        &dest_ok,
    ] {
        fs::remove_dir_all(d).ok();
    }
}

// ===========================================================================
// FAILURE MODE: external adapter rejected on EITHER side (in-process only).
// ===========================================================================

/// SPEC failure mode: a non-in-process scheme as `--objects-store` is rejected
/// with an actionable error (only in-process file/s3/b2/gcs are allowed — the
/// same constraint as `sync`).
#[test]
fn external_objects_store_scheme_rejected() {
    let src = temp_dir("x-src");
    let cap = temp_dir("x-cap");
    let cache = temp_dir("x-cache");
    build_tree(&src, &[("a.txt", b"hello")]);

    let src_str = src.to_string_lossy().into_owned();
    let cap_url = file_url(&cap);

    let out = run_raw(
        &[
            "push",
            "--objects-store",
            "custom://x",
            "--store",
            &cap_url,
            &src_str,
        ],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "a non-in-process --objects-store scheme must be rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("in-process")
            || stderr.contains("not supported")
            || stderr.contains("file/s3/b2/gcs"),
        "rejection must be actionable (in-process file/s3/b2/gcs only); got: {stderr}"
    );

    for d in [&src, &cap, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

/// SPEC failure mode: the external/in-process constraint applies on the
/// MANIFEST side too — a non-in-process `--store` (with a valid `--objects-store`
/// pool) is likewise rejected with an actionable error.
#[test]
fn external_store_scheme_rejected_with_objects_store_set() {
    let src = temp_dir("xm-src");
    let pool = temp_dir("xm-pool");
    let cache = temp_dir("xm-cache");
    build_tree(&src, &[("a.txt", b"hello")]);

    let src_str = src.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);

    let out = run_raw(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            "custom://x",
            &src_str,
        ],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "a non-in-process --store (manifest side) must be rejected when split"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("in-process")
            || stderr.contains("not supported")
            || stderr.contains("file/s3/b2/gcs"),
        "manifest-side rejection must be actionable; got: {stderr}"
    );

    for d in [&src, &pool, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

// ===========================================================================
// FAILURE MODE: --objects-store set but --store missing -> clear error (no panic)
// ===========================================================================

/// SPEC failure mode: `--objects-store` set but `--store` (and `$SNAPDIR_STORE`)
/// MISSING must produce a clear, actionable error — NOT a panic and NOT a silent
/// success. (Without a manifest location there is nowhere to write the manifest.)
#[test]
fn objects_store_set_but_store_missing_errors_cleanly() {
    let src = temp_dir("m-src");
    let pool = temp_dir("m-pool");
    let cache = temp_dir("m-cache");
    build_tree(&src, &[("a.txt", b"hello")]);

    let src_str = src.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);

    // No --store, and run_raw removes $SNAPDIR_STORE, so the manifest location
    // is genuinely unset.
    let out = run_raw(
        &["push", "--objects-store", &pool_url, &src_str],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "--objects-store without --store must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Must be a clean, actionable error — never a Rust panic.
    assert!(
        !stderr.contains("panicked")
            && !stderr.contains("RUST_BACKTRACE")
            && !stderr.to_lowercase().contains("internal error"),
        "missing --store must be a clean error, not a panic; got: {stderr}"
    );
    let lc = stderr.to_lowercase();
    assert!(
        lc.contains("--store") || lc.contains("store"),
        "error must name the missing --store/manifest location; got: {stderr}"
    );

    for d in [&src, &pool, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

// ===========================================================================
// CATALOG records the MANIFEST-side URI (--store), not the pool.
// ===========================================================================

/// SPEC: if the CLI surfaces locations/catalog, the catalog records the
/// MANIFEST-side URI (`--store`), NOT the `--objects-store` pool. A split push
/// then `locations`/`revisions --location=<--store>` must show the manifest
/// store, and the pool URI must NOT appear as a catalog location.
#[test]
fn catalog_records_manifest_store_not_pool() {
    let src = temp_dir("cat-src");
    let pool = temp_dir("cat-pool");
    let cap = temp_dir("cat-cap");
    let cache = temp_dir("cat-cache");
    let catalog = cache.join("catalog.redb");
    build_tree(&src, &[("a.txt", b"hello")]);

    let src_str = src.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let cap_url = file_url(&cap);
    let catalog_str = catalog.to_string_lossy().into_owned();
    // The catalog is selected via $SNAPDIR_CATALOG (== the global --catalog flag),
    // mirroring `catalog_commands.rs`.
    let cat_env: &[(&str, &str)] = &[("SNAPDIR_CATALOG", &catalog_str)];

    let id = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_url,
            &src_str,
        ],
        &cache,
        cat_env,
    );
    assert_eq!(id.len(), 64);

    // `revisions --location=<--store>` lists the revision under the MANIFEST URI.
    let revisions = run_ok(&["revisions", "--location", &cap_url], &cache, cat_env);
    assert!(
        revisions.contains(&id),
        "catalog must record the revision under the --store (manifest) URI; got: {revisions:?}"
    );

    // `locations` records the manifest store, and NEVER the pool.
    let locations = run_ok(&["locations"], &cache, cat_env);
    assert!(
        locations.contains(&cap_url),
        "catalog locations must include the --store (manifest) URI; got: {locations:?}"
    );
    assert!(
        !locations.contains(&pool_url),
        "catalog must NOT record the --objects-store pool as a location; got: {locations:?}"
    );

    // Sanity: querying by the POOL URI yields no revisions (it isn't a location).
    let pool_revs = run_ok(&["revisions", "--location", &pool_url], &cache, cat_env);
    assert!(
        pool_revs.trim().is_empty(),
        "the pool URI must NOT be a recorded catalog location; got: {pool_revs:?}"
    );

    for d in [&src, &pool, &cap, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

// ===========================================================================
// DELTA COST: a changed source file on the second capture uploads only the
// CHANGED object to the pool, not the whole tree.
// ===========================================================================

/// SPEC: when a single source file changes on the second capture (same shared
/// pool), only the CHANGED object is uploaded to the pool — the unchanged blobs
/// are skipped (delta cost). After the second push the pool holds the original
/// blobs PLUS exactly ONE new blob (the changed file's), not a whole new tree.
#[test]
fn second_capture_uploads_only_changed_object_to_pool() {
    let src = temp_dir("d-src");
    let pool = temp_dir("d-pool");
    let cap1 = temp_dir("d-cap1");
    let cap2 = temp_dir("d-cap2");
    let cache = temp_dir("d-cache");

    // Three distinct files -> three distinct blobs on the first capture.
    let v1: &[(&str, &[u8])] = &[
        ("a.txt", b"alpha"),
        ("b.txt", b"bravo"),
        ("c.txt", b"charlie"),
    ];
    build_tree(&src, v1);

    let src_str = src.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let cap1_url = file_url(&cap1);
    let cap2_url = file_url(&cap2);

    run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &cap1_url,
            &src_str,
        ],
        &cache,
        &[],
    );
    let after_first = count_pool_objects(&pool);
    assert_eq!(
        after_first, 3,
        "first capture seeds the pool with 3 distinct blobs"
    );

    // Change exactly ONE file; the other two blobs are unchanged.
    fs::write(src.join("b.txt"), b"BRAVO-CHANGED").unwrap();
    fs::set_permissions(src.join("b.txt"), fs::Permissions::from_mode(0o644)).unwrap();

    // Second capture into the SAME pool, a NEW manifest store cap2.
    run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &cap2_url,
            &src_str,
        ],
        &cache,
        &[],
    );

    let after_second = count_pool_objects(&pool);
    assert_eq!(
        after_second,
        after_first + 1,
        "second capture must upload ONLY the one changed object (delta cost), \
         leaving the pool at {} blobs, not re-uploading the whole tree",
        after_first + 1
    );

    // The new blob is exactly the changed file's content; the old blobs remain.
    let changed_sum = Blake3Hasher::new().hash_hex(b"BRAVO-CHANGED");
    assert!(
        pool.join(sharded(".objects", &changed_sum)).is_file(),
        "the changed file's new object must be present in the pool"
    );
    for original in [&b"alpha"[..], &b"bravo"[..], &b"charlie"[..]] {
        let sum = Blake3Hasher::new().hash_hex(original);
        assert!(
            pool.join(sharded(".objects", &sum)).is_file(),
            "original blobs must remain in the shared pool"
        );
    }

    for d in [&src, &pool, &cap1, &cap2, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

// ===========================================================================
// REVIEW-GATE STRENGTHENING (phase 28, objects-store-cli-tests-review)
// Added after the impl became visible. Each test below pins a concrete impl
// branch the black-box e2e suite could only assert loosely. They are e2e
// against the real `snapdir` binary, mirroring the suite's existing style.
// ===========================================================================

/// Reads `stderr` from a `run_raw` `Output` as a lossy `String` (NOT lowercased,
/// so message-exactness assertions can match the real casing/scheme).
fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// REVIEW (error exactness, OBJECTS side): an external `custom://` `--objects-store`
/// is rejected with the SAME `sync`-style message the impl reuses
/// (`stream_store_for_adapter`'s `Adapter::External` arm), and that message NAMES
/// the offending object-pool URL — not merely a generic "not supported". This
/// pins that the objects side is built via the shared rejecting router and that
/// the surfaced URL is the OBJECTS URL (so the user can tell which side failed).
#[test]
fn external_objects_store_error_names_offending_pool_url() {
    let src = temp_dir("xn-src");
    let cap = temp_dir("xn-cap");
    let cache = temp_dir("xn-cache");
    build_tree(&src, &[("a.txt", b"hello")]);

    let src_str = src.to_string_lossy().into_owned();
    let cap_url = file_url(&cap);
    // A distinctive authority so we can assert the OBJECTS url is the one named.
    let bad_pool = "custom://objects-side-pool";

    let out = run_raw(
        &[
            "push",
            "--objects-store",
            bad_pool,
            "--store",
            &cap_url,
            &src_str,
        ],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "external --objects-store must be rejected"
    );
    let stderr = stderr_of(&out);
    // The impl reuses the `sync` rejection verbatim: pin the actionable phrase ...
    assert!(
        stderr.contains("in-process stores (file/s3/b2/gcs)"),
        "objects-side rejection must reuse the sync 'in-process stores (file/s3/b2/gcs)' \
         message; got: {stderr}"
    );
    // ... and that it names the OFFENDING OBJECTS url (not the manifest --store).
    assert!(
        stderr.contains(bad_pool),
        "rejection must name the offending --objects-store url {bad_pool:?}; got: {stderr}"
    );
    assert!(
        !stderr.contains(&cap_url),
        "the valid manifest --store {cap_url:?} must NOT be blamed; got: {stderr}"
    );

    for d in [&src, &cap, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

/// REVIEW (error exactness, MANIFEST side): with a VALID `--objects-store` pool, an
/// external `custom://` `--store` (manifest side) is rejected with the same
/// `sync`-style message, and that message NAMES the offending MANIFEST url. Pins
/// that the manifest side is ALSO routed through the rejecting
/// `stream_store_for_adapter` (not silently accepted because the pool was valid),
/// and that the blamed url is the manifest one.
#[test]
fn external_manifest_store_error_names_offending_store_url() {
    let src = temp_dir("xmn-src");
    let pool = temp_dir("xmn-pool");
    let cache = temp_dir("xmn-cache");
    build_tree(&src, &[("a.txt", b"hello")]);

    let src_str = src.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let bad_store = "custom://manifest-side-store";

    let out = run_raw(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            bad_store,
            &src_str,
        ],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "external manifest --store must be rejected even when --objects-store is valid"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("in-process stores (file/s3/b2/gcs)"),
        "manifest-side rejection must reuse the sync message; got: {stderr}"
    );
    assert!(
        stderr.contains(bad_store),
        "rejection must name the offending --store url {bad_store:?}; got: {stderr}"
    );
    // The valid pool must not be written to (rejected before any object write).
    assert_eq!(
        count_pool_objects(&pool),
        0,
        "no objects may be written when the manifest side is rejected"
    );

    for d in [&src, &pool, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

/// REVIEW (missing --store, clean error): `--objects-store` set but `--store`
/// genuinely UNSET (env removed) must fail with a clean, NON-panicking error that
/// names `--store`. Strengthens the staged case by pinning that the message is the
/// canonical `missing --store option` (the impl surfaces it BEFORE touching either
/// store) and that NOTHING is written to the pool. NOTE: the impl's nicer
/// `resolve_split_store` message that also names `$SNAPDIR_STORE` is shadowed on
/// the push path by the earlier `store_url` guard, so this pins the ACTUAL
/// observed message rather than over-asserting `$SNAPDIR_STORE`.
#[test]
fn missing_store_with_objects_store_is_clean_named_error() {
    let src = temp_dir("mn-src");
    let pool = temp_dir("mn-pool");
    let cache = temp_dir("mn-cache");
    build_tree(&src, &[("a.txt", b"hello")]);

    let src_str = src.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);

    let out = run_raw(
        &["push", "--objects-store", &pool_url, &src_str],
        &cache,
        &[],
    );
    assert!(
        !out.status.success(),
        "--objects-store without --store must fail"
    );
    let stderr = stderr_of(&out);
    assert!(
        !stderr.contains("panicked")
            && !stderr.contains("RUST_BACKTRACE")
            && !stderr.to_lowercase().contains("internal error"),
        "missing --store must be a clean error, not a panic; got: {stderr}"
    );
    assert!(
        stderr.contains("--store"),
        "error must name --store; got: {stderr}"
    );
    assert_eq!(
        count_pool_objects(&pool),
        0,
        "a missing-manifest-store failure must not write objects to the pool"
    );

    for d in [&src, &pool, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

/// REVIEW (flag > env, exact): the `--objects-store` FLAG wins even when
/// `$SNAPDIR_OBJECTS_STORE` names an INVALID (external) pool. clap's env wiring
/// must let the flag fully shadow the env: a valid flag pool + a poison env pool
/// must SUCCEED (env never consulted) and route objects to the FLAG pool. This is
/// stronger than the staged `flag_objects_store_overrides_env` (which used two
/// valid pools): here the env value would be a hard error if it were ever read.
#[test]
fn flag_objects_store_beats_poison_env() {
    let src = temp_dir("fp-src");
    let flag_pool = temp_dir("fp-flagpool");
    let cap = temp_dir("fp-cap");
    let cache = temp_dir("fp-cache");
    build_tree(&src, &[("a.txt", b"hello")]);

    let src_str = src.to_string_lossy().into_owned();
    let flag_url = file_url(&flag_pool);
    let cap_url = file_url(&cap);

    // Env points at an external (would-fail) pool; the flag must completely win.
    let out = run_raw(
        &[
            "push",
            "--objects-store",
            &flag_url,
            "--store",
            &cap_url,
            &src_str,
        ],
        &cache,
        &[("SNAPDIR_OBJECTS_STORE", "custom://poison-env-pool")],
    );
    assert!(
        out.status.success(),
        "the explicit --objects-store flag must shadow an invalid env entirely; \
         stderr: {}",
        stderr_of(&out)
    );
    let sum = Blake3Hasher::new().hash_hex(b"hello");
    assert!(
        flag_pool.join(sharded(".objects", &sum)).is_file(),
        "objects must land in the FLAG pool"
    );

    for d in [&src, &flag_pool, &cap, &cache] {
        fs::remove_dir_all(d).ok();
    }
}

/// REVIEW (`store_is_external` short-circuit): a split store (`--objects-store` set)
/// is treated as IN-PROCESS, so push takes the in-process tree/scratch branch and
/// NEVER the external emit-command path. Proven end-to-end: with a `file://` pool
/// and a `file://` manifest store, the pushed objects land as real blobs in the
/// pool's `.objects/` (the in-process `FileStore` layout), and a fresh-cache fetch
/// then offline checkout reproduces the tree — which only the in-process path
/// yields. (The external path would instead emit per-object commands against a
/// sharded cache root and write nothing to a pool tree.)
#[test]
fn split_store_uses_in_process_path_not_external_emit() {
    let src = temp_dir("sp-src");
    let pool = temp_dir("sp-pool");
    let cap = temp_dir("sp-cap");
    let dest = temp_dir("sp-dest");
    let cache = temp_dir("sp-cache");

    let leaves: &[(&str, &[u8])] = &[("x.txt", b"in-process-only"), ("y.txt", b"second")];
    build_tree(&src, leaves);

    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let cap_url = file_url(&cap);

    let id = run_ok(
        &[
            "push",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_url,
            &src_str,
        ],
        &cache,
        &[],
    );

    // In-process FileStore wrote real sharded blobs into the pool tree (the
    // external emit path would have written nothing here).
    for (_, bytes) in leaves {
        let sum = Blake3Hasher::new().hash_hex(bytes);
        assert!(
            pool.join(sharded(".objects", &sum)).is_file(),
            "split push must write in-process blobs into the pool .objects/"
        );
    }

    // A fresh-cache fetch from the split store reads the in-process blobs back,
    // and an OFFLINE checkout reproduces the tree byte-for-byte. Drives the
    // in-process fetch branch that the `store_is_external() == false`
    // short-circuit selects.
    let read_cache = temp_dir("sp-readcache");
    run_ok(
        &[
            "fetch",
            "--objects-store",
            &pool_url,
            "--store",
            &cap_url,
            "--id",
            &id,
        ],
        &read_cache,
        &[],
    );
    run_ok(&["checkout", "--id", &id, &dest_str], &read_cache, &[]);
    assert_tree_contents(&dest, leaves);
    assert_eq!(
        run_ok(&["id", &dest_str], &read_cache, &[]),
        id,
        "split round trip via the in-process path must re-manifest to the source id"
    );

    for d in [&src, &pool, &cap, &dest, &cache, &read_cache] {
        fs::remove_dir_all(d).ok();
    }
}

/// REVIEW (precedence completeness): the THIRD precedence permutation the staged
/// suite left implicit — env-named OBJECTS pool combined with a FLAG-named manifest
/// `--store` (mixed sources) must compose correctly: objects to the env pool,
/// manifest to the flag store, and the round trip restores. Pins that the two
/// global args resolve independently (each honoring its own flag-or-env source).
#[test]
fn env_objects_with_flag_store_compose() {
    let src = temp_dir("mx-src");
    let pool = temp_dir("mx-pool");
    let cap = temp_dir("mx-cap");
    let dest = temp_dir("mx-dest");
    let cache = temp_dir("mx-cache");

    let leaves: &[(&str, &[u8])] = &[("a.txt", b"hello"), ("z.txt", b"zeta")];
    build_tree(&src, leaves);

    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();
    let pool_url = file_url(&pool);
    let cap_url = file_url(&cap);

    // OBJECTS via env, MANIFEST via flag.
    let id = run_ok(
        &["push", "--store", &cap_url, &src_str],
        &cache,
        &[("SNAPDIR_OBJECTS_STORE", &pool_url)],
    );
    for (_, bytes) in leaves {
        let sum = Blake3Hasher::new().hash_hex(bytes);
        assert!(
            pool.join(sharded(".objects", &sum)).is_file(),
            "env-named pool must receive the objects"
        );
    }
    assert!(
        cap.join(sharded(".manifests", &id)).is_file(),
        "flag-named --store must receive the manifest"
    );

    // Round trip with the same mixed sources restores byte-for-byte.
    run_ok(
        &["pull", "--store", &cap_url, "--id", &id, &dest_str],
        &cache,
        &[("SNAPDIR_OBJECTS_STORE", &pool_url)],
    );
    assert_tree_contents(&dest, leaves);

    for d in [&src, &pool, &cap, &dest, &cache] {
        fs::remove_dir_all(d).ok();
    }
}
