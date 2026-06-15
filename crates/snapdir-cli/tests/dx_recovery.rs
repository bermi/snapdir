//! Adversarial spec-tests for the §6 persona-5 RECOVERY gaps (phase 30,
//! `dx-recovery-spec-tests`).
//!
//! These pin the contract that a cache which has lost (or corrupted) a content
//! object can be made whole again, and that the tooling never lies about cache
//! health. They are authored BLACK-BOX from the gate spec alone (no `src/`
//! visibility) and are EXPECTED TO FAIL against the current binary on the bug
//! clauses below — they encode the desired behavior, not today's behavior.
//!
//! Confirmed current-bug behavior (probed black-box against the debug binary):
//!   * `fetch`/`fetch --force`/`pull` short-circuit on the cached MANIFEST
//!     (`fetch --verbose` prints `CACHED: <id>`) and never re-pull a deleted
//!     OBJECT — the cache stays broken; only `flush-cache` + `fetch` heals it.
//!   * `verify-cache` only catches CORRUPT bytes (`Checksum mismatch`); a
//!     MISSING object slips through with a silent exit 0.
//!
//! Layout reminder (frozen sharded keys):
//!   `<prefix>/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>` under `.objects` /
//!   `.manifests`.
//!
//! Hermetic: every test gets its own temp source tree, `file://` store, and
//! `--cache-dir`; nothing touches the user's real `$HOME/.cache/snapdir`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use snapdir_core::{Blake3Hasher, Hasher};

/// Path to the compiled `snapdir` binary under test.
///
/// The bin target lives in the `snapdir` crate (`crates/snapdir`), so
/// `CARGO_BIN_EXE_snapdir` is not set for snapdir-cli tests; `assert_cmd`'s
/// lookup falls back to the shared target dir. Under `cargo test --workspace`
/// the binary is always built first; for a standalone run, build `-p snapdir`
/// once before.
fn snapdir_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// Creates a unique temp directory and returns its path.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-dx-recovery-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Runs `snapdir <args>` with the cache pinned via `SNAPDIR_CACHE_DIR`,
/// returning the raw output (no success assertion — recovery tests inspect both
/// exit code and streams). The cache is pinned through the env var (not the
/// `--cache-dir` flag) so it works uniformly across every subcommand, including
/// store-independent ones like `id` that do not accept `--cache-dir`.
fn run_raw(args: &[&str], cache: &Path) -> Output {
    Command::new(snapdir_bin())
        .args(args)
        .env("SNAPDIR_CACHE_DIR", cache)
        .output()
        .expect("run snapdir")
}

/// Runs `snapdir <args>` (cache pinned), asserts success, returns trimmed stdout.
fn run_ok(args: &[&str], cache: &Path) -> String {
    let out = run_raw(args, cache);
    assert!(
        out.status.success(),
        "snapdir {args:?} exited with {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout)
        .expect("stdout is UTF-8")
        .trim_end()
        .to_owned()
}

/// `<prefix>/<h[0..3]>/<h[3..6]>/<h[6..9]>/<h[9..]>` — the frozen sharded layout.
fn sharded(prefix: &str, hex: &str) -> String {
    format!(
        "{prefix}/{}/{}/{}/{}",
        &hex[0..3],
        &hex[3..6],
        &hex[6..9],
        &hex[9..]
    )
}

/// The (relative path, bytes) pairs of the shared source tree.
const TREE_FILES: [(&str, &[u8]); 2] = [("a.txt", b"hello"), ("sub/b.txt", b"world!!")];

/// Builds the known tiny tree with explicit, deterministic permissions so a
/// checkout must restore them to re-manifest to the same snapshot id.
fn build_tree(src: &Path) {
    fs::create_dir_all(src.join("sub")).unwrap();
    for (rel, bytes) in TREE_FILES {
        let target = src.join(rel);
        fs::write(&target, bytes).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    }
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(src, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The cache `.objects` path for `bytes`' content address.
fn object_path(cache: &Path, bytes: &[u8]) -> PathBuf {
    let sum = Blake3Hasher::new().hash_hex(bytes);
    cache.join(sharded(".objects", &sum))
}

/// Push the tree to a fresh `file://` store, then fetch it into a fresh cache so
/// the cache holds the manifest + every object. Returns `(store_url, id)`.
fn push_and_fetch(src: &Path, store: &Path, cache: &Path) -> (String, String) {
    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let id = run_ok(&["push", "--store", &store_url, &src_str], cache);
    assert_eq!(id.len(), 64, "snapshot id should be 64 hex chars: {id:?}");
    // Re-fetch into a clean cache so the cache is a faithful client copy.
    fs::remove_dir_all(cache).ok();
    run_ok(&["fetch", "--store", &store_url, "--id", &id], cache);
    for (_, bytes) in TREE_FILES {
        assert!(
            object_path(cache, bytes).is_file(),
            "fetch into a clean cache must populate every object"
        );
    }
    (store_url, id)
}

fn cleanup(dirs: &[&Path]) {
    for d in dirs {
        fs::remove_dir_all(d).ok();
    }
}

// ---------------------------------------------------------------------------
// Clause 1 — fetch must RESTORE a missing cache object.
// ---------------------------------------------------------------------------

/// Clause 1 (fetch heals): deleting a cache object then re-running `fetch` must
/// RESTORE it from the store (the cache is whole again), not report CACHED and
/// leave the hole. BUG TODAY: fetch short-circuits on the cached manifest and
/// exits 0 with the object still missing.
#[test]
fn fetch_restores_a_missing_cache_object() {
    let src = temp_dir("c1-fetch-src");
    let store = temp_dir("c1-fetch-store");
    let cache = temp_dir("c1-fetch-cache");
    build_tree(&src);
    let (store_url, id) = push_and_fetch(&src, &store, &cache);

    // Delete one object — the cache is now broken (manifest present, object gone).
    let obj = object_path(&cache, b"hello");
    fs::remove_file(&obj).unwrap();
    assert!(!obj.exists(), "object must be gone before re-fetch");

    // Re-fetch the SAME id/store: contract is "make the cache whole".
    let out = run_raw(&["fetch", "--store", &store_url, "--id", &id], &cache);
    assert!(
        out.status.success(),
        "fetch should succeed while healing the cache; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The deleted object must be back, with correct bytes.
    assert!(
        obj.is_file(),
        "fetch must RESTORE the missing object at {}, not report CACHED and leave the cache broken",
        obj.display()
    );
    assert_eq!(
        fs::read(&obj).unwrap(),
        b"hello",
        "restored object must carry the original bytes"
    );

    cleanup(&[&src, &store, &cache]);
}

/// Clause 1 (fetch --force heals): `--force` after a deletion must also restore
/// the missing object. BUG TODAY: `--force` still reports CACHED, exit 0, hole
/// remains.
#[test]
fn fetch_force_restores_a_missing_cache_object() {
    let src = temp_dir("c1-force-src");
    let store = temp_dir("c1-force-store");
    let cache = temp_dir("c1-force-cache");
    build_tree(&src);
    let (store_url, id) = push_and_fetch(&src, &store, &cache);

    let obj = object_path(&cache, b"hello");
    fs::remove_file(&obj).unwrap();

    let out = run_raw(&["fetch", "--store", &store_url, "--id", &id, "--force"], &cache);
    assert!(
        out.status.success(),
        "fetch --force should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        obj.is_file(),
        "fetch --force must RESTORE the missing object at {}",
        obj.display()
    );
    assert_eq!(fs::read(&obj).unwrap(), b"hello");

    cleanup(&[&src, &store, &cache]);
}

/// Clause 1 (pull heals + checks out): with a cache object deleted, `pull`
/// (fetch+checkout) must restore the object from the store AND complete the
/// checkout into a byte-identical tree. BUG TODAY: pull does not restore the
/// object and fails at checkout with `object not found`.
#[test]
fn pull_restores_missing_object_and_checks_out() {
    let src = temp_dir("c1-pull-src");
    let store = temp_dir("c1-pull-store");
    let cache = temp_dir("c1-pull-cache");
    let dest = temp_dir("c1-pull-dest");
    build_tree(&src);
    let (store_url, id) = push_and_fetch(&src, &store, &cache);

    let obj = object_path(&cache, b"hello");
    fs::remove_file(&obj).unwrap();

    let dest_str = dest.to_string_lossy().into_owned();
    let out = run_raw(
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
        &cache,
    );
    assert!(
        out.status.success(),
        "pull must heal the cache and check out; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The object was restored…
    assert!(
        obj.is_file(),
        "pull must RESTORE the missing object before checkout"
    );
    // …and the checkout reproduced the tree byte-for-byte.
    for (rel, bytes) in TREE_FILES {
        assert_eq!(
            fs::read(dest.join(rel)).unwrap(),
            bytes,
            "checked-out bytes for {rel}"
        );
    }
    // …re-manifesting to the same id (contents + permissions round-tripped).
    let dest_id = run_ok(&["id", &dest_str], &cache);
    assert_eq!(dest_id, id, "healed pull must re-manifest to the pushed id");

    cleanup(&[&src, &store, &cache, &dest]);
}

/// Clause 1 (post-heal checkout): after `fetch` heals a deleted object, a plain
/// offline `checkout` from the cache must succeed and reproduce the tree —
/// proving the cache really is whole again (not merely "fetch printed OK").
#[test]
fn checkout_succeeds_after_fetch_heals_missing_object() {
    let src = temp_dir("c1-co-src");
    let store = temp_dir("c1-co-store");
    let cache = temp_dir("c1-co-cache");
    let dest = temp_dir("c1-co-dest");
    build_tree(&src);
    let (store_url, id) = push_and_fetch(&src, &store, &cache);

    let obj = object_path(&cache, b"hello");
    fs::remove_file(&obj).unwrap();

    // Heal via fetch, then check out OFFLINE (no --store) from the cache alone.
    run_ok(&["fetch", "--store", &store_url, "--id", &id], &cache);
    let dest_str = dest.to_string_lossy().into_owned();
    let out = run_raw(&["checkout", "--id", &id, &dest_str], &cache);
    assert!(
        out.status.success(),
        "offline checkout must succeed once fetch healed the cache; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for (rel, bytes) in TREE_FILES {
        assert_eq!(fs::read(dest.join(rel)).unwrap(), bytes, "bytes for {rel}");
    }

    cleanup(&[&src, &store, &cache, &dest]);
}

// ---------------------------------------------------------------------------
// Clause 2 — verify-cache must DETECT a missing object.
// ---------------------------------------------------------------------------

/// Clause 2 (detect missing): after deleting a cache object, `verify-cache`
/// must exit NON-ZERO and name the missing object's address. BUG TODAY: a
/// missing object yields a silent exit 0 (only corrupt content is caught).
#[test]
fn verify_cache_detects_a_missing_object_nonzero() {
    let src = temp_dir("c2-src");
    let store = temp_dir("c2-store");
    let cache = temp_dir("c2-cache");
    build_tree(&src);
    let (_store_url, _id) = push_and_fetch(&src, &store, &cache);

    let sum = Blake3Hasher::new().hash_hex(b"hello");
    let obj = cache.join(sharded(".objects", &sum));
    fs::remove_file(&obj).unwrap();

    let out = run_raw(&["verify-cache"], &cache);
    assert!(
        !out.status.success(),
        "verify-cache must FAIL when an object is missing, not exit 0 silently"
    );
    // The missing object's address must be named (stdout or stderr).
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains(&sum),
        "verify-cache must name the missing object's address {sum}; got: {combined}"
    );

    cleanup(&[&src, &store, &cache]);
}

/// Clause 2 (detect missing, scoped by id): `verify-cache --id <id>` over a
/// snapshot whose object was deleted must also fail non-zero and name the gap.
#[test]
fn verify_cache_with_id_detects_missing_object() {
    let src = temp_dir("c2id-src");
    let store = temp_dir("c2id-store");
    let cache = temp_dir("c2id-cache");
    build_tree(&src);
    let (_store_url, id) = push_and_fetch(&src, &store, &cache);

    let sum = Blake3Hasher::new().hash_hex(b"hello");
    let obj = cache.join(sharded(".objects", &sum));
    fs::remove_file(&obj).unwrap();

    let out = run_raw(&["verify-cache", "--id", &id], &cache);
    assert!(
        !out.status.success(),
        "verify-cache --id must FAIL when the snapshot's object is missing"
    );

    cleanup(&[&src, &store, &cache]);
}

// ---------------------------------------------------------------------------
// Clause 3 — message quality: name WHAT is missing (object + affected path).
// ---------------------------------------------------------------------------

/// Clause 3 (message quality, verify-cache): the missing-object report should
/// identify the object AND, ideally, the affected file path from the manifest
/// (`a.txt`), not only the bare hash.
#[test]
fn verify_cache_missing_message_names_object_and_path() {
    let src = temp_dir("c3v-src");
    let store = temp_dir("c3v-store");
    let cache = temp_dir("c3v-cache");
    build_tree(&src);
    let (_store_url, _id) = push_and_fetch(&src, &store, &cache);

    let sum = Blake3Hasher::new().hash_hex(b"hello");
    let obj = cache.join(sharded(".objects", &sum));
    fs::remove_file(&obj).unwrap();

    let out = run_raw(&["verify-cache"], &cache);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Names the object…
    assert!(
        combined.contains(&sum),
        "missing-object message must name the object {sum}; got: {combined}"
    );
    // …and signals MISSING (not the corrupt-content wording).
    let lc = combined.to_lowercase();
    assert!(
        lc.contains("missing") || lc.contains("not found") || lc.contains("absent"),
        "missing-object message must say it is MISSING (distinct from corrupt); got: {combined}"
    );
    // …and ideally points at the affected file path from the manifest.
    assert!(
        combined.contains("a.txt"),
        "missing-object message should name the affected file path (a.txt), not only the hash; got: {combined}"
    );

    cleanup(&[&src, &store, &cache]);
}

/// Clause 3 (message quality, pull/checkout): when an object can neither be
/// found nor restored, the surfaced error should name the affected file path
/// (`a.txt`) from the manifest, not only the bare object hash. (Spawned only as
/// a quality assertion on the error path; the heal-path tests above pin the
/// primary contract.)
#[test]
fn checkout_missing_object_error_names_the_file_path() {
    let src = temp_dir("c3c-src");
    let store = temp_dir("c3c-store");
    let cache = temp_dir("c3c-cache");
    let dest = temp_dir("c3c-dest");
    build_tree(&src);
    // Push + fetch, then delete the object AND make the store unable to supply
    // it (point checkout offline) so the error path is exercised deterministically.
    let (_store_url, id) = push_and_fetch(&src, &store, &cache);
    let sum = Blake3Hasher::new().hash_hex(b"hello");
    fs::remove_file(cache.join(sharded(".objects", &sum))).unwrap();

    let dest_str = dest.to_string_lossy().into_owned();
    // Offline checkout (no --store): cannot self-heal, so it must error and the
    // message should locate the gap by file path, not just the hash.
    let out = run_raw(&["checkout", "--id", &id, &dest_str], &cache);
    assert!(
        !out.status.success(),
        "offline checkout with a missing object must fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("a.txt"),
        "missing-object checkout error should name the affected file path (a.txt), not only the hash; got: {combined}"
    );

    cleanup(&[&src, &store, &cache, &dest]);
}

// ---------------------------------------------------------------------------
// Clause 4 — KEYSTONE (no regression of the healthy / corrupt behaviors).
// ---------------------------------------------------------------------------

/// Clause 4 (keystone, healthy round-trip): stage→push→fetch→checkout with
/// NOTHING missing yields byte-identical object pools (store == cache `.objects`)
/// and the SAME snapshot id end-to-end. The heal logic must not perturb the
/// happy path.
#[test]
fn keystone_healthy_roundtrip_is_unchanged() {
    let src = temp_dir("c4-rt-src");
    let store = temp_dir("c4-rt-store");
    let cache = temp_dir("c4-rt-cache");
    let dest = temp_dir("c4-rt-dest");
    build_tree(&src);

    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();

    let src_id = run_ok(&["id", &src_str], &cache);
    let pushed = run_ok(&["push", "--store", &store_url, &src_str], &cache);
    assert_eq!(pushed, src_id, "push prints the source snapshot id");

    fs::remove_dir_all(&cache).ok();
    run_ok(&["fetch", "--store", &store_url, "--id", &src_id], &cache);

    // Object pools are byte-identical between store and freshly fetched cache.
    for (_, bytes) in TREE_FILES {
        let key = sharded(".objects", &Blake3Hasher::new().hash_hex(bytes));
        let from_store = fs::read(store.join(&key)).expect("store object");
        let from_cache = fs::read(cache.join(&key)).expect("cache object");
        assert_eq!(from_store, from_cache, "store/cache object pools must match byte-for-byte");
        assert_eq!(from_store, bytes, "object bytes must equal the source bytes");
    }

    run_ok(&["checkout", "--id", &src_id, &dest_str], &cache);
    for (rel, bytes) in TREE_FILES {
        assert_eq!(fs::read(dest.join(rel)).unwrap(), bytes, "bytes for {rel}");
    }
    let dest_id = run_ok(&["id", &dest_str], &cache);
    assert_eq!(dest_id, src_id, "healthy round-trip preserves the snapshot id");

    cleanup(&[&src, &store, &cache, &dest]);
}

/// Clause 4 (keystone, healthy verify): `verify-cache` on a clean, freshly
/// fetched cache still exits 0 with no noise on stdout. The new missing-object
/// detection must not false-positive on a whole cache.
#[test]
fn keystone_verify_cache_healthy_exits_zero_silent() {
    let src = temp_dir("c4-vok-src");
    let store = temp_dir("c4-vok-store");
    let cache = temp_dir("c4-vok-cache");
    build_tree(&src);
    push_and_fetch(&src, &store, &cache);

    let out = run_raw(&["verify-cache"], &cache);
    assert!(
        out.status.success(),
        "verify-cache on a healthy cache must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "healthy verify-cache should be silent on stdout; got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    cleanup(&[&src, &store, &cache]);
}

/// Clause 4 (keystone, corrupt detection preserved): tampering a cached object
/// in place (wrong bytes for its address) must STILL make `verify-cache` exit
/// non-zero and name the corrupt object — the existing behavior the fix must
/// not regress. `--purge` still removes it.
#[test]
fn keystone_verify_cache_still_detects_corrupt_object() {
    let src = temp_dir("c4-corr-src");
    let store = temp_dir("c4-corr-store");
    let cache = temp_dir("c4-corr-cache");
    build_tree(&src);
    push_and_fetch(&src, &store, &cache);

    let sum = Blake3Hasher::new().hash_hex(b"hello");
    let obj = cache.join(sharded(".objects", &sum));
    fs::write(&obj, b"TAMPERED").unwrap();

    let out = run_raw(&["verify-cache"], &cache);
    assert!(
        !out.status.success(),
        "corrupt object must still fail verify-cache"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains(&sum),
        "corrupt-object report must name the object {sum}; got: {combined}"
    );

    // --purge removes the corrupt object (existing behavior preserved).
    let purged = run_raw(&["verify-cache", "--purge"], &cache);
    assert!(!purged.status.success(), "purge run still reports the failure");
    assert!(!obj.exists(), "corrupt object must be purged by --purge");

    cleanup(&[&src, &store, &cache]);
}
