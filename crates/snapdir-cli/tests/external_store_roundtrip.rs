//! End-to-end integration tests for `push`/`fetch`/`pull` against an EXTERNAL
//! `snapdir-<proto>-store` adapter (the emit-command shim), driving the real
//! `snapdir` binary against the `snapdir-mock-store` fixture from
//! crates/snapdir-stores/tests/.
//!
//! The external contract differs from the in-process stores: the
//! `--staging-dir`/`--cache-dir` values handed to the emitted scripts are
//! SHARDED store roots (`.objects/<sharded>` + `.manifests/<sharded>`), NOT
//! source/dest trees, so the CLI must route external transfers through the
//! local cache (stage-then-push on the way out; fetch-into-cache with the
//! manifest committed LAST on the way in). These tests pin that wiring
//! end-to-end — it used to be silently broken because no CLI test drove
//! `mock://`.
//!
//! Hermetic: temp dirs only, no network; the shim shells out to `bash`, which
//! is present on every supported platform/CI runner.

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
/// the binary is always built first; for a standalone
/// `cargo test -p snapdir-cli`, run `cargo build -p snapdir` once before.
fn snapdir_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// `PATH` with the directory holding the `snapdir-mock-store` fixture
/// prepended, so the router resolves the external adapter binary for
/// `mock://` URLs (and `bash` etc. stay resolvable from the original PATH).
fn path_with_mock_store() -> String {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../snapdir-stores/tests/snapdir-mock-store")
        .canonicalize()
        .expect("snapdir-mock-store fixture exists");
    let dir = fixture.parent().expect("fixture has a parent directory");
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Creates a unique temp directory and returns its path.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-cli-external-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Runs the `snapdir` binary with the mock store on PATH, returning the raw
/// output (no success assertion — for the failure-path tests).
fn run_snapdir_raw(args: &[&str], cache: &Path) -> Output {
    Command::new(snapdir_bin())
        .args(args)
        .env("SNAPDIR_CACHE_DIR", cache)
        .env("PATH", path_with_mock_store())
        .output()
        .expect("run snapdir")
}

/// Runs the `snapdir` binary, asserting success and returning trimmed stdout.
fn run_snapdir(args: &[&str], cache: &Path) -> String {
    let output = run_snapdir_raw(args, cache);
    assert!(
        output.status.success(),
        "snapdir {args:?} exited with {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout)
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

/// The (relative path, bytes) pairs of the shared source tree: a top-level
/// file, a subdir file, and a file with spaces in its name.
const TREE_FILES: [(&str, &[u8]); 3] = [
    ("a.txt", b"hello external"),
    ("sub/b.txt", b"world!!"),
    ("with space.txt", b"spaced out"),
];

/// Builds the shared multi-file source tree with explicit, deterministic
/// permissions so a checked-out copy must restore them to re-manifest to the
/// same snapshot id.
fn build_source_tree(src: &Path) {
    fs::create_dir(src.join("sub")).unwrap();
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o755)).unwrap();
    for (rel, bytes) in TREE_FILES {
        let target = src.join(rel);
        fs::write(&target, bytes).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    }
    fs::set_permissions(src, fs::Permissions::from_mode(0o755)).unwrap();
}

/// (a) + (c): push a multi-file tree to a `mock://` store — the snapshot id
/// lands on stdout, the mock store dir contains the sharded manifest + every
/// object — and a second identical push is an idempotent no-op printing the
/// same id.
#[test]
fn external_push_lands_sharded_manifest_and_objects() {
    let src = temp_dir("push-src");
    let store = temp_dir("push-store");
    let cache = temp_dir("push-cache");
    build_source_tree(&src);

    // `mock://` + absolute path: the mock strips the literal `mock://` prefix,
    // so the embedded absolute path supplies the third slash (`mock:///abs/…`).
    let store_url = format!("mock://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();

    // The id the source tree manifests to (independent of the store).
    let src_id = run_snapdir(&["id", &src_str], &cache);
    assert_eq!(src_id.len(), 64, "snapshot id should be 64 hex chars");

    // Push through the external adapter; the printed id must equal the
    // source id (the scriptable id-on-stdout contract).
    let pushed_id = run_snapdir(&["push", "--store", &store_url, &src_str], &cache);
    assert_eq!(pushed_id, src_id, "push must print the source snapshot id");

    // The manifest landed in the MOCK STORE at its exact sharded key — proof
    // the emitted script received a sharded staging dir, not a source tree.
    let manifest_key = store.join(sharded(".manifests", &src_id));
    assert!(
        manifest_key.is_file(),
        "manifest must land at sharded key {}",
        manifest_key.display()
    );

    // Every file object landed at its content-addressed sharded key with
    // matching bytes.
    for (rel, bytes) in TREE_FILES {
        let sum = Blake3Hasher::new().hash_hex(bytes);
        let obj = store.join(sharded(".objects", &sum));
        assert!(
            obj.is_file(),
            "object for {rel} must land at {}",
            obj.display()
        );
        assert_eq!(fs::read(&obj).unwrap(), bytes, "object bytes for {rel}");
    }

    // (c) Second push of the same tree: idempotent no-op (the mock emits
    // "Manifest already exists on store."), exit 0, same id.
    let repushed_id = run_snapdir(&["push", "--store", &store_url, &src_str], &cache);
    assert_eq!(repushed_id, src_id, "re-push must print the same id");

    for dir in [&src, &store, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}

/// (b) + (e): fetch the pushed snapshot with a FRESH cache (a second logical
/// client), checkout, and get a byte-identical tree that re-manifests to the
/// same id; the fresh cache holds the sharded manifest afterwards — proof the
/// external fetch arm committed the manifest via `put_manifest` (manifest
/// LAST) rather than routing the cache dir through a tree-shaped `push`.
#[test]
fn external_fetch_fresh_cache_then_checkout_reproduces_tree() {
    let src = temp_dir("fetch-src");
    let store = temp_dir("fetch-store");
    let dest = temp_dir("fetch-dest");
    let push_cache = temp_dir("fetch-cache-pusher");
    let fetch_cache = temp_dir("fetch-cache-fetcher");
    build_source_tree(&src);

    let store_url = format!("mock://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let dest_str = dest.to_string_lossy().into_owned();

    let id = run_snapdir(&["push", "--store", &store_url, &src_str], &push_cache);

    // Fetch with a FRESH cache: every object must travel store → cache through
    // the external adapter (no local shortcut exists).
    run_snapdir(&["fetch", "--store", &store_url, "--id", &id], &fetch_cache);

    // (e) The fresh cache holds the sharded manifest — the put-manifest-last
    // arm ran (and `checkout` below works offline from this cache alone).
    let cache_manifest = fetch_cache.join(sharded(".manifests", &id));
    assert!(
        cache_manifest.is_file(),
        "external fetch must commit the manifest into the cache at {}",
        cache_manifest.display()
    );
    // …and every object at its sharded content address.
    for (rel, bytes) in TREE_FILES {
        let sum = Blake3Hasher::new().hash_hex(bytes);
        let obj = fetch_cache.join(sharded(".objects", &sum));
        assert!(
            obj.is_file(),
            "external fetch must file the object for {rel} at {}",
            obj.display()
        );
    }

    // (b) Checkout from the populated cache reproduces the tree byte-for-byte.
    run_snapdir(&["checkout", "--id", &id, &dest_str], &fetch_cache);
    for (rel, bytes) in TREE_FILES {
        assert_eq!(
            fs::read(dest.join(rel)).unwrap(),
            bytes,
            "checked-out bytes for {rel}"
        );
    }

    // And the checked-out tree re-manifests to the SAME snapshot id (contents
    // + permissions round-tripped through the external store).
    let dest_id = run_snapdir(&["id", &dest_str], &fetch_cache);
    assert_eq!(
        dest_id, id,
        "checked-out tree must re-manifest to the pushed snapshot id"
    );

    for dir in [&src, &store, &dest, &push_cache, &fetch_cache] {
        fs::remove_dir_all(dir).ok();
    }
}

/// (d): fetching an unknown (well-formed 64-hex) id from an external store
/// fails with a non-zero exit and a "not found" error on stderr — the shim's
/// `ManifestNotFound` mapping surfaces through the CLI.
#[test]
fn external_fetch_unknown_id_fails_with_not_found() {
    let store = temp_dir("unknown-store");
    let cache = temp_dir("unknown-cache");
    let store_url = format!("mock://{}", store.display());
    let unknown = "0".repeat(64); // valid shape, never pushed

    let output = run_snapdir_raw(&["fetch", "--store", &store_url, "--id", &unknown], &cache);
    assert!(
        !output.status.success(),
        "fetch of an unknown id must fail, not succeed silently"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "stderr should report the manifest as not found; got: {stderr}"
    );
    // Nothing may have been committed to the cache for the unknown id.
    assert!(
        !cache.join(sharded(".manifests", &unknown)).exists(),
        "a failed fetch must not commit a cache manifest"
    );

    for dir in [&store, &cache] {
        fs::remove_dir_all(dir).ok();
    }
}
