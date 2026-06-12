//! Black-box spec suite for per-side object-pool sync flags
//! `--from-objects <URI>` / `--to-objects <URI>` (phase 28, gate
//! `sync-objects-split-spec-tests`).
//!
//! AUTHORED FROM THE SPEC ONLY — the `--from-objects`/`--to-objects` sync wiring
//! does NOT exist yet, so this suite is EXPECTED to fail until the impl lands. It
//! is staged in `.gatesmith/pending-tests/` so the workspace keeps compiling; the
//! cli impl teammate moves it to `crates/snapdir-cli/tests/sync_split.rs` and
//! wires it (it may adjust exact arg/SyncReport spelling but MUST preserve the
//! BEHAVIORS pinned here).
//!
//! SPEC under test
//! ===============
//! `snapdir sync --from <STORE> --to <STORE> --id <id>` copies one snapshot
//! store->store (exists today, colocated). This feature adds PER-SIDE object-pool
//! flags:
//!   * `--from-objects <URI>` — explicit SOURCE object pool (distinct from the
//!     global `--objects-store`; source & dest can be DIFFERENT buckets/pools).
//!   * `--to-objects <URI>`   — explicit DEST object pool.
//!   * When a side's `*-objects` flag is present, that side is a SPLIT store
//!     (objects = the flag, manifests = `--from`/`--to`). Absent => plain
//!     COLOCATED store as today.
//!   * The `sync_snapshot` engine is UNCHANGED — a `SplitStore` is already a
//!     `StreamStore`, so object/manifest reads & writes route to the correct
//!     split side automatically.
//!
//! INVARIANTS pinned here (file:// stores, NO creds, drive the REAL binary e2e):
//!   (a) Cross-pool skip: if the DEST object pool ALREADY holds the snapshot's
//!       blobs, the sync copies the MANIFEST to the dest manifest location but
//!       RE-COPIES ZERO objects (dest pool `.objects` count unchanged + the
//!       SyncReport shows the skipped/zero-copied count).
//!   (b) Both-sides-split round trip: source split -> dest split, then the
//!       snapshot is fully retrievable from the dest (pull via the dest manifest
//!       location + dest pool) byte-identical to the original.
//!
//! FAILURE MODES / EDGE CASES attacked here:
//!   * Asymmetric: only `--from-objects` (source split / dest colocated) and only
//!     `--to-objects` (source colocated / dest split) — both work; objects land
//!     in / read from the right place per side.
//!   * Dest manifest already present => whole sync is a no-op (fast path).
//!   * Missing source object (a blob referenced by the source manifest absent
//!     from the source pool) => sync ERRORS and writes NO dest manifest
//!     (manifest-last / all-or-nothing).
//!   * Manifest-last across the split: after a FAILED sync the dest manifest
//!     location has no `.manifests/<id>` even if some objects landed in the dest
//!     pool.
//!   * Distinct source/dest pools: objects written to the DEST pool, NOT the
//!     source pool; the source pool is read-only / untouched.
//!
//! Note on arg shape: tests assume the surface
//!   `snapdir sync --from <S> --to <S> --id <id> [--from-objects <U>] [--to-objects <U>]`.
//! The impl teammate may adjust exact flag spelling / SyncReport text; the
//! BEHAVIORS encoded here are what must hold.

// The crate enables `clippy::pedantic` workspace-wide; these test-only stylistic
// lints (mirroring the `#![allow(...)]` in sibling suites like `objects_store.rs`
// and `progress_e2e.rs`) are suppressed so the staged suite compiles under
// `-D warnings` WITHOUT touching any assertion or behavior.
#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::doc_markdown
)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ===========================================================================
// Harness (mirrors crates/snapdir-cli/tests/sync_e2e.rs + objects_store.rs)
// ===========================================================================

/// Path to the compiled `snapdir` binary under test (the bin target lives in the
/// `snapdir` crate; `assert_cmd` resolves it from the shared target dir).
fn snapdir_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// A unique temp directory; created and returned. `tag` only aids debugging.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-syncsplit-{tag}-{}-{:?}",
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

/// Runs the `snapdir` binary with the cache pinned under `cache` and the
/// store/objects env vars REMOVED (so the developer's env cannot mask a bug).
/// Returns the raw `Output`.
fn run_raw(args: &[&str], cache: &Path) -> Output {
    Command::new(snapdir_bin())
        .args(args)
        .env("SNAPDIR_CACHE_DIR", cache)
        .env_remove("SNAPDIR_STORE")
        .env_remove("SNAPDIR_OBJECTS_STORE")
        .output()
        .expect("run snapdir")
}

/// Runs `snapdir <args>`, asserts SUCCESS, returns trimmed stdout.
fn run_ok(args: &[&str], cache: &Path) -> String {
    let out = run_raw(args, cache);
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

/// Recursively collects every regular FILE under `dir` as paths relative to
/// `dir`. Returns empty if `dir` is absent.
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

/// Number of leaf blobs under `<pool>/.objects/` (0 if absent).
fn count_pool_objects(pool: &Path) -> usize {
    collect_files(&pool.join(".objects")).len()
}

/// Number of leaf manifest blobs under `<loc>/.manifests/` (0 if absent).
fn count_manifests(loc: &Path) -> usize {
    collect_files(&loc.join(".manifests")).len()
}

/// True iff the manifest location at `loc` physically holds the snapshot `id` —
/// i.e. SOME file under `<loc>/.manifests/` whose sharded path (the `3/3/3/rest`
/// split, separators stripped) reconstructs EXACTLY to `id`. Sharding-agnostic:
/// it does not hardcode the split widths, only that the on-disk key is the id
/// with directory separators inserted. Lets a test assert the SPECIFIC failed id
/// is absent from the dest, not merely that the `.manifests/` count is 0.
fn dest_serves_manifest_id(loc: &Path, id: &str) -> bool {
    collect_files(&loc.join(".manifests")).iter().any(|rel| {
        let joined: String = rel
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();
        joined == id
    })
}

/// Builds a known multi-file tree with deterministic permissions so a checked-out
/// copy must restore them to re-manifest to the same id. `leaves` is a slice of
/// `(relative_path, contents)`. Distinct contents => distinct blobs.
fn build_tree(dir: &Path, leaves: &[(&str, &[u8])]) {
    for (rel, bytes) in leaves {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
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

/// A canonical 4-distinct-blob tree reused across tests.
fn sample_leaves() -> &'static [(&'static str, &'static [u8])] {
    &[
        ("top.txt", b"alpha-body"),
        ("sub/one.txt", b"bravo-body-bravo"),
        ("sub/two.bin", b"charlie!!!"),
        ("sub/deep/three.dat", b"delta-delta-delta"),
    ]
}

/// Asserts the tree at `dest` reproduces every `(rel, bytes)` leaf.
fn assert_tree_contents(dest: &Path, expected: &[(&str, &[u8])]) {
    for (rel, bytes) in expected {
        let got = fs::read(dest.join(rel))
            .unwrap_or_else(|e| panic!("read {rel} from dest {}: {e}", dest.display()));
        assert_eq!(&got[..], *bytes, "contents of {rel} must match source");
    }
}

/// Parses the integer immediately preceding `word` in an "N copied, M skipped"
/// style SyncReport summary line. Returns `None` if `word` is not present.
fn parse_count(line: &str, word: &str) -> Option<usize> {
    let idx = line.find(word)?;
    line[..idx]
        .split_whitespace()
        .next_back()
        .and_then(|tok| tok.parse().ok())
}

/// Finds the SyncReport summary line in `stderr` (the line mentioning the synced
/// id and a copied count). Panics with the full stderr if absent.
fn summary_line(stderr: &str) -> &str {
    stderr
        .lines()
        .find(|l| l.contains("copied"))
        .unwrap_or_else(|| panic!("expected a sync summary line with a copied count:\n{stderr}"))
}

/// Deletes ONE leaf blob from `<pool>/.objects/` (to simulate a missing source
/// object). Returns the path removed. Panics if the pool has no objects.
fn delete_one_object(pool: &Path) -> PathBuf {
    let objs = collect_files(&pool.join(".objects"));
    let rel = objs
        .iter()
        .next()
        .unwrap_or_else(|| panic!("pool {} has no objects to delete", pool.display()))
        .clone();
    let abs = pool.join(".objects").join(&rel);
    fs::remove_file(&abs).unwrap_or_else(|e| panic!("remove {}: {e}", abs.display()));
    abs
}

// ===========================================================================
// (b) BOTH-SIDES-SPLIT ROUND TRIP
// ===========================================================================

/// SPEC (b): source split (`--from` + `--from-objects`) -> dest split (`--to` +
/// `--to-objects`); afterward the snapshot is FULLY retrievable from the dest
/// (pull via the dest manifest location + dest pool) byte-identical to the
/// original, and re-manifests to the source id.
#[test]
fn sync_split_both_sides_round_trips() {
    let src = temp_dir("b-src");
    let src_mani = temp_dir("b-srcmani");
    let src_pool = temp_dir("b-srcpool");
    let dst_mani = temp_dir("b-dstmani");
    let dst_pool = temp_dir("b-dstpool");
    let cache = temp_dir("b-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_mani_url = file_url(&src_mani);
    let src_pool_url = file_url(&src_pool);
    let dst_mani_url = file_url(&dst_mani);
    let dst_pool_url = file_url(&dst_pool);

    // Push the source as a SPLIT store: objects -> src_pool, manifest -> src_mani.
    let src_id = run_ok(
        &[
            "push",
            "--objects-store",
            &src_pool_url,
            "--store",
            &src_mani_url,
            &src_str,
        ],
        &cache,
    );
    assert_eq!(src_id.len(), 64, "snapshot id is 64 hex chars");
    assert_eq!(
        count_pool_objects(&src_pool),
        leaves.len(),
        "source pool must hold one blob per distinct file"
    );

    // sync source-split -> dest-split.
    let out = run_raw(
        &[
            "sync",
            "--id",
            &src_id,
            "--from",
            &src_mani_url,
            "--from-objects",
            &src_pool_url,
            "--to",
            &dst_mani_url,
            "--to-objects",
            &dst_pool_url,
        ],
        &cache,
    );
    assert!(
        out.status.success(),
        "both-sides-split sync must succeed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim_end(),
        src_id,
        "sync must print the snapshot id to stdout"
    );

    // The DEST manifest landed in the dest manifest location; objects in the dest
    // POOL (NOT the dest manifest location).
    assert_eq!(
        count_manifests(&dst_mani),
        1,
        "the manifest must land in the dest manifest location's .manifests/"
    );
    assert_eq!(
        count_pool_objects(&dst_pool),
        leaves.len(),
        "every blob must land in the dest pool"
    );
    assert_eq!(
        count_pool_objects(&dst_mani),
        0,
        "no objects may land in the dest MANIFEST location (split: objects go to --to-objects)"
    );

    // The dest serves the snapshot: pull via the dest split (manifest + pool).
    let dest = temp_dir("b-out");
    let dest_str = dest.to_string_lossy().into_owned();
    let pullcache = temp_dir("b-pullcache");
    run_ok(
        &[
            "pull",
            "--objects-store",
            &dst_pool_url,
            "--store",
            &dst_mani_url,
            "--id",
            &src_id,
            &dest_str,
        ],
        &pullcache,
    );
    assert_tree_contents(&dest, leaves);
    assert_eq!(
        run_ok(&["id", &dest_str], &pullcache),
        src_id,
        "tree pulled from the dest split must re-manifest to the source id"
    );
}

// ===========================================================================
// (a) CROSS-POOL SKIP — dest pool already has the blobs => ZERO objects re-copied
// ===========================================================================

/// SPEC (a): when the DEST object pool ALREADY holds the snapshot's blobs (pushed
/// there earlier), a sync copies the MANIFEST to the dest manifest location but
/// re-copies ZERO objects. Asserted two ways: (1) the dest pool's `.objects`
/// count is UNCHANGED across the sync, and (2) the SyncReport reports 0 copied /
/// all skipped.
#[test]
fn sync_split_dest_pool_has_blobs_recopies_zero_objects() {
    let src = temp_dir("a-src");
    let src_mani = temp_dir("a-srcmani");
    let src_pool = temp_dir("a-srcpool");
    let dst_mani = temp_dir("a-dstmani");
    let dst_pool = temp_dir("a-dstpool");
    let cache = temp_dir("a-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_mani_url = file_url(&src_mani);
    let src_pool_url = file_url(&src_pool);
    let dst_mani_url = file_url(&dst_mani);
    let dst_pool_url = file_url(&dst_pool);

    // Source split push: objects -> src_pool, manifest -> src_mani.
    let src_id = run_ok(
        &[
            "push",
            "--objects-store",
            &src_pool_url,
            "--store",
            &src_mani_url,
            &src_str,
        ],
        &cache,
    );

    // PRE-SEED the DEST POOL with the exact same blobs by pushing the SAME tree
    // there earlier (manifest goes to a throwaway location, NOT dst_mani — we want
    // only the blobs present in the dest pool, not the manifest in dst_mani).
    let seed_mani = temp_dir("a-seedmani");
    let seed_mani_url = file_url(&seed_mani);
    run_ok(
        &[
            "push",
            "--objects-store",
            &dst_pool_url,
            "--store",
            &seed_mani_url,
            &src_str,
        ],
        &cache,
    );
    let dst_pool_objects_before = count_pool_objects(&dst_pool);
    assert_eq!(
        dst_pool_objects_before,
        leaves.len(),
        "the pre-seed must put every blob into the dest pool"
    );
    // The dest MANIFEST location must NOT yet hold the snapshot manifest (only the
    // blobs are pre-present), so the sync still has manifest work to do.
    assert_eq!(
        count_manifests(&dst_mani),
        0,
        "dest manifest location must start without the snapshot manifest"
    );

    // sync source-split -> dest-split. The dest pool already has every blob.
    let out = run_raw(
        &[
            "sync",
            "--id",
            &src_id,
            "--from",
            &src_mani_url,
            "--from-objects",
            &src_pool_url,
            "--to",
            &dst_mani_url,
            "--to-objects",
            &dst_pool_url,
        ],
        &cache,
    );
    assert!(
        out.status.success(),
        "cross-pool sync must succeed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    let summary = summary_line(&stderr);

    // (1) ZERO objects re-copied: the dest pool's object count is UNCHANGED.
    assert_eq!(
        count_pool_objects(&dst_pool),
        dst_pool_objects_before,
        "no objects may be re-uploaded when the dest pool already holds them:\n{summary}"
    );

    // (2) The SyncReport must show the skip: 0 copied (and all blobs skipped).
    let copied = parse_count(summary, "copied")
        .unwrap_or_else(|| panic!("no copied count in summary:\n{summary}"));
    assert_eq!(
        copied, 0,
        "every blob is already in the dest pool => 0 objects copied:\n{summary}"
    );
    if let Some(skipped) = parse_count(summary, "skipped") {
        assert_eq!(
            skipped,
            leaves.len(),
            "all {} present blobs must be reported skipped:\n{summary}",
            leaves.len()
        );
    }

    // BUT the manifest WAS copied to the dest manifest location.
    assert_eq!(
        count_manifests(&dst_mani),
        1,
        "the manifest must be copied to the dest manifest location even when 0 objects copied"
    );

    // And the dest now serves the snapshot byte-identically.
    let dest = temp_dir("a-out");
    let dest_str = dest.to_string_lossy().into_owned();
    let pullcache = temp_dir("a-pullcache");
    run_ok(
        &[
            "pull",
            "--objects-store",
            &dst_pool_url,
            "--store",
            &dst_mani_url,
            "--id",
            &src_id,
            &dest_str,
        ],
        &pullcache,
    );
    assert_tree_contents(&dest, leaves);
    assert_eq!(
        run_ok(&["id", &dest_str], &pullcache),
        src_id,
        "the zero-object sync must still leave the dest fully serving the snapshot"
    );
}

// ===========================================================================
// ASYMMETRIC: only --from-objects (source split, dest colocated)
// ===========================================================================

/// SPEC (asymmetric): only `--from-objects` is set — the SOURCE is a split store
/// (objects in src_pool, manifest in src_mani) and the DEST is plain COLOCATED
/// (objects AND manifest both land under `--to`). Objects must be READ from the
/// source pool and WRITTEN colocated into the dest, which then fully serves it.
#[test]
fn sync_split_source_only_dest_colocated() {
    let src = temp_dir("fo-src");
    let src_mani = temp_dir("fo-srcmani");
    let src_pool = temp_dir("fo-srcpool");
    let dst = temp_dir("fo-dst");
    let cache = temp_dir("fo-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_mani_url = file_url(&src_mani);
    let src_pool_url = file_url(&src_pool);
    let dst_url = file_url(&dst);

    let src_id = run_ok(
        &[
            "push",
            "--objects-store",
            &src_pool_url,
            "--store",
            &src_mani_url,
            &src_str,
        ],
        &cache,
    );

    // sync: source split (--from + --from-objects) -> dest colocated (--to only).
    let out = run_raw(
        &[
            "sync",
            "--id",
            &src_id,
            "--from",
            &src_mani_url,
            "--from-objects",
            &src_pool_url,
            "--to",
            &dst_url,
        ],
        &cache,
    );
    assert!(
        out.status.success(),
        "source-split/dest-colocated sync must succeed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Dest is COLOCATED: both objects AND manifest land under --to.
    assert_eq!(
        count_pool_objects(&dst),
        leaves.len(),
        "colocated dest must hold every object under its own .objects/"
    );
    assert_eq!(
        count_manifests(&dst),
        1,
        "colocated dest must hold the manifest under its own .manifests/"
    );

    // Pull from the colocated dest (NO --objects-store): byte-identical.
    let dest = temp_dir("fo-out");
    let dest_str = dest.to_string_lossy().into_owned();
    let pullcache = temp_dir("fo-pullcache");
    run_ok(
        &["pull", "--store", &dst_url, "--id", &src_id, &dest_str],
        &pullcache,
    );
    assert_tree_contents(&dest, leaves);
    assert_eq!(
        run_ok(&["id", &dest_str], &pullcache),
        src_id,
        "colocated dest must fully serve the snapshot"
    );
}

// ===========================================================================
// ASYMMETRIC: only --to-objects (source colocated, dest split)
// ===========================================================================

/// SPEC (asymmetric): only `--to-objects` is set — the SOURCE is plain COLOCATED
/// (`--from` holds objects AND manifest) and the DEST is a split store (objects
/// to to_pool, manifest to `--to`). Objects must be READ colocated from the
/// source and WRITTEN to the dest POOL (NOT the dest manifest location).
#[test]
fn sync_split_dest_only_source_colocated() {
    let src = temp_dir("to-src");
    let src_store = temp_dir("to-srcstore");
    let dst_mani = temp_dir("to-dstmani");
    let dst_pool = temp_dir("to-dstpool");
    let cache = temp_dir("to-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_store_url = file_url(&src_store);
    let dst_mani_url = file_url(&dst_mani);
    let dst_pool_url = file_url(&dst_pool);

    // Source is COLOCATED (no --objects-store): objects + manifest both under
    // src_store.
    let src_id = run_ok(&["push", "--store", &src_store_url, &src_str], &cache);
    assert_eq!(
        count_pool_objects(&src_store),
        leaves.len(),
        "colocated source must hold its objects colocated"
    );

    // sync: source colocated (--from only) -> dest split (--to + --to-objects).
    let out = run_raw(
        &[
            "sync",
            "--id",
            &src_id,
            "--from",
            &src_store_url,
            "--to",
            &dst_mani_url,
            "--to-objects",
            &dst_pool_url,
        ],
        &cache,
    );
    assert!(
        out.status.success(),
        "source-colocated/dest-split sync must succeed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Objects landed in the dest POOL; the manifest in the dest manifest location;
    // and NO objects in the dest manifest location.
    assert_eq!(
        count_pool_objects(&dst_pool),
        leaves.len(),
        "split dest must write objects to --to-objects"
    );
    assert_eq!(
        count_manifests(&dst_mani),
        1,
        "split dest must write the manifest to --to"
    );
    assert_eq!(
        count_pool_objects(&dst_mani),
        0,
        "split dest must NOT write objects into the manifest location"
    );

    // Pull from the dest split: byte-identical.
    let dest = temp_dir("to-out");
    let dest_str = dest.to_string_lossy().into_owned();
    let pullcache = temp_dir("to-pullcache");
    run_ok(
        &[
            "pull",
            "--objects-store",
            &dst_pool_url,
            "--store",
            &dst_mani_url,
            "--id",
            &src_id,
            &dest_str,
        ],
        &pullcache,
    );
    assert_tree_contents(&dest, leaves);
    assert_eq!(
        run_ok(&["id", &dest_str], &pullcache),
        src_id,
        "split dest must fully serve the snapshot"
    );
}

// ===========================================================================
// DEST MANIFEST ALREADY PRESENT => whole sync is a NO-OP (fast path)
// ===========================================================================

/// SPEC (fast path): when the dest manifest location ALREADY holds `<id>` (the
/// snapshot was synced there before), re-running the SAME split sync is a NO-OP:
/// it succeeds, reports 0 copied, and leaves BOTH the dest manifest location and
/// the dest pool physically unchanged.
#[test]
fn sync_split_dest_manifest_present_is_noop() {
    let src = temp_dir("noop-src");
    let src_mani = temp_dir("noop-srcmani");
    let src_pool = temp_dir("noop-srcpool");
    let dst_mani = temp_dir("noop-dstmani");
    let dst_pool = temp_dir("noop-dstpool");
    let cache = temp_dir("noop-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_mani_url = file_url(&src_mani);
    let src_pool_url = file_url(&src_pool);
    let dst_mani_url = file_url(&dst_mani);
    let dst_pool_url = file_url(&dst_pool);

    let src_id = run_ok(
        &[
            "push",
            "--objects-store",
            &src_pool_url,
            "--store",
            &src_mani_url,
            &src_str,
        ],
        &cache,
    );

    let sync_args = [
        "sync",
        "--id",
        &src_id,
        "--from",
        &src_mani_url,
        "--from-objects",
        &src_pool_url,
        "--to",
        &dst_mani_url,
        "--to-objects",
        &dst_pool_url,
    ];

    // First sync populates the dest split.
    run_ok(&sync_args, &cache);
    let dst_mani_after_first = collect_files(&dst_mani);
    let dst_pool_after_first = collect_files(&dst_pool);
    assert!(
        !dst_mani_after_first.is_empty() && !dst_pool_after_first.is_empty(),
        "the first sync must populate both dest sides"
    );

    // Second sync of the same id: the dest manifest is already present => no-op.
    let out2 = run_raw(&sync_args, &cache);
    assert!(out2.status.success(), "the no-op sync must succeed");
    let stderr2 = String::from_utf8(out2.stderr).unwrap();
    let summary2 = summary_line(&stderr2);
    let copied2 = parse_count(summary2, "copied")
        .unwrap_or_else(|| panic!("no copied count in second summary:\n{summary2}"));
    assert_eq!(
        copied2, 0,
        "a sync whose dest manifest is already present must copy 0 objects:\n{summary2}"
    );

    // Both dest sides physically UNCHANGED.
    assert_eq!(
        collect_files(&dst_mani),
        dst_mani_after_first,
        "the no-op sync must leave the dest manifest location unchanged"
    );
    assert_eq!(
        collect_files(&dst_pool),
        dst_pool_after_first,
        "the no-op sync must leave the dest pool unchanged"
    );
}

// ===========================================================================
// MISSING SOURCE OBJECT => sync ERRORS + NO dest manifest (manifest-last)
// ===========================================================================

/// SPEC (missing source object + manifest-last): if a blob referenced by the
/// source manifest is ABSENT from the source pool, the sync ERRORS (non-zero
/// exit) AND writes NO dest manifest — the dest manifest location must NOT gain
/// `.manifests/<id>`, even though some other objects may have landed in the dest
/// pool (all-or-nothing / manifest-last across the split).
#[test]
fn sync_split_missing_source_object_errors_and_writes_no_dest_manifest() {
    let src = temp_dir("miss-src");
    let src_mani = temp_dir("miss-srcmani");
    let src_pool = temp_dir("miss-srcpool");
    let dst_mani = temp_dir("miss-dstmani");
    let dst_pool = temp_dir("miss-dstpool");
    let cache = temp_dir("miss-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_mani_url = file_url(&src_mani);
    let src_pool_url = file_url(&src_pool);
    let dst_mani_url = file_url(&dst_mani);
    let dst_pool_url = file_url(&dst_pool);

    let src_id = run_ok(
        &[
            "push",
            "--objects-store",
            &src_pool_url,
            "--store",
            &src_mani_url,
            &src_str,
        ],
        &cache,
    );

    // Corrupt the SOURCE pool: delete one blob the manifest references. The
    // manifest in src_mani still lists it, so the sync will fail mid-copy.
    let removed = delete_one_object(&src_pool);
    assert!(
        !removed.exists(),
        "the referenced blob must actually be gone from the source pool"
    );

    // sync must FAIL (the missing object cannot be copied).
    let out = run_raw(
        &[
            "sync",
            "--id",
            &src_id,
            "--from",
            &src_mani_url,
            "--from-objects",
            &src_pool_url,
            "--to",
            &dst_mani_url,
            "--to-objects",
            &dst_pool_url,
        ],
        &cache,
    );
    assert!(
        !out.status.success(),
        "a sync with a missing source object must FAIL (non-zero exit), stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // MANIFEST-LAST / all-or-nothing: the dest manifest location must NOT have
    // gained the snapshot manifest, regardless of any objects that landed.
    assert_eq!(
        count_manifests(&dst_mani),
        0,
        "a FAILED sync must NOT publish the manifest to the dest manifest location (manifest-last)"
    );
    // STRENGTHEN (adversary review): assert the SPECIFIC failed id is physically
    // absent from the dest `.manifests/` shard tree — not just that the leaf count
    // is 0. A count of 0 could be satisfied vacuously if manifests landed
    // elsewhere; this pins that NO on-disk key under the dest reconstructs to the
    // failed snapshot id (manifest-last, by exact id).
    assert!(
        !dest_serves_manifest_id(&dst_mani, &src_id),
        "the dest .manifests/ tree must NOT physically contain the failed id {src_id}"
    );
    // Belt-and-suspenders: the specific id must not be resolvable from the dest.
    // (Arg-shape fix: `manifest` is a local-tree describe command and does NOT
    // resolve a pinned id from a store — `fetch` is snapdir's store-resolution
    // surface, so this is the correct way to express the SAME assertion that the
    // dest does not serve a manifest for the failed id. The pull cache is fresh,
    // so the manifest must come from the dest store, which has none => failure.)
    let probecache = temp_dir("miss-probecache");
    let probe = run_raw(
        &[
            "fetch",
            "--objects-store",
            &dst_pool_url,
            "--store",
            &dst_mani_url,
            "--id",
            &src_id,
        ],
        &probecache,
    );
    assert!(
        !probe.status.success(),
        "the dest must not serve a manifest for the failed sync's id"
    );
}

// ===========================================================================
// DISTINCT POOLS — objects go to the DEST pool; the SOURCE pool is untouched
// ===========================================================================

/// SPEC (distinct source/dest pools / read-only source): with DISTINCT source and
/// dest pools, a successful split sync writes the blobs into the DEST pool and
/// leaves the SOURCE pool BYTE-FOR-BYTE unchanged (read-only) — neither new
/// objects nor a manifest appear in the source pool / source manifest location.
#[test]
fn sync_split_source_pool_untouched_objects_land_in_dest_pool() {
    let src = temp_dir("ro-src");
    let src_mani = temp_dir("ro-srcmani");
    let src_pool = temp_dir("ro-srcpool");
    let dst_mani = temp_dir("ro-dstmani");
    let dst_pool = temp_dir("ro-dstpool");
    let cache = temp_dir("ro-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_mani_url = file_url(&src_mani);
    let src_pool_url = file_url(&src_pool);
    let dst_mani_url = file_url(&dst_mani);
    let dst_pool_url = file_url(&dst_pool);

    let src_id = run_ok(
        &[
            "push",
            "--objects-store",
            &src_pool_url,
            "--store",
            &src_mani_url,
            &src_str,
        ],
        &cache,
    );

    // Snapshot the SOURCE sides before the sync.
    let src_pool_before = collect_files(&src_pool);
    let src_mani_before = collect_files(&src_mani);
    assert!(
        !src_pool_before.is_empty(),
        "source pool must hold the blobs before the sync"
    );

    // sync split -> split (distinct pools).
    run_ok(
        &[
            "sync",
            "--id",
            &src_id,
            "--from",
            &src_mani_url,
            "--from-objects",
            &src_pool_url,
            "--to",
            &dst_mani_url,
            "--to-objects",
            &dst_pool_url,
        ],
        &cache,
    );

    // The SOURCE sides are untouched (read-only).
    assert_eq!(
        collect_files(&src_pool),
        src_pool_before,
        "the source pool must be byte-for-byte unchanged (read-only) after the sync"
    );
    assert_eq!(
        collect_files(&src_mani),
        src_mani_before,
        "the source manifest location must be unchanged after the sync"
    );

    // The blobs landed in the DEST pool, NOT duplicated back into the source pool.
    assert_eq!(
        count_pool_objects(&dst_pool),
        leaves.len(),
        "objects must be written to the DEST pool"
    );
    // The dest pool is a DISTINCT directory from the source pool.
    assert_ne!(
        dst_pool, src_pool,
        "source and dest pools must be distinct directories in this test"
    );
}

// ===========================================================================
// PER-SIDE EXTERNAL-ADAPTER REJECTION on --from-objects / --to-objects
// ===========================================================================

/// SPEC (in-process-only / per-side rejection): an external `snapdir-*-store`
/// URL (any non file/s3/b2/gcs scheme, here `custom://`) is REJECTED on EITHER
/// `*-objects` leg — sync requires in-process stores. This pins that the split
/// is built via the same `stream_store_for_adapter` gate as the rest of sync, so
/// an external object pool cannot sneak in on a per-side flag. Asserts (1) a
/// non-zero exit and (2) the actionable in-process error message — and that NO
/// child `snapdir-*-store` binary is shelled out (the rejection precedes any
/// subprocess: a `custom://` adapter has no such binary, so a non-rejecting impl
/// that tried to spawn it would fail with a DIFFERENT, exec-not-found error).
#[test]
fn sync_split_external_objects_uri_rejected_per_side() {
    let src = temp_dir("ext-src");
    let src_mani = temp_dir("ext-srcmani");
    let src_pool = temp_dir("ext-srcpool");
    let dst_mani = temp_dir("ext-dstmani");
    let dst_pool = temp_dir("ext-dstpool");
    let cache = temp_dir("ext-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_mani_url = file_url(&src_mani);
    let src_pool_url = file_url(&src_pool);
    let dst_mani_url = file_url(&dst_mani);
    let dst_pool_url = file_url(&dst_pool);

    let src_id = run_ok(
        &[
            "push",
            "--objects-store",
            &src_pool_url,
            "--store",
            &src_mani_url,
            &src_str,
        ],
        &cache,
    );

    // An external object pool on the SOURCE side (--from-objects) is rejected.
    let from_ext = run_raw(
        &[
            "sync",
            "--id",
            &src_id,
            "--from",
            &src_mani_url,
            "--from-objects",
            "custom://external/source/pool",
            "--to",
            &dst_mani_url,
            "--to-objects",
            &dst_pool_url,
        ],
        &cache,
    );
    assert!(
        !from_ext.status.success(),
        "an external --from-objects URL must be rejected (non-zero exit)"
    );
    let from_err = String::from_utf8_lossy(&from_ext.stderr);
    assert!(
        from_err.contains("in-process") || from_err.contains("not supported"),
        "the rejection must name the in-process-only contract, got:\n{from_err}"
    );
    // The rejection happens BEFORE any store mutation: the dest got nothing.
    assert_eq!(
        count_manifests(&dst_mani),
        0,
        "a rejected external --from-objects sync must not publish a dest manifest"
    );
    assert_eq!(
        count_pool_objects(&dst_pool),
        0,
        "a rejected external --from-objects sync must not write dest objects"
    );

    // An external object pool on the DEST side (--to-objects) is rejected too.
    let to_ext = run_raw(
        &[
            "sync",
            "--id",
            &src_id,
            "--from",
            &src_mani_url,
            "--from-objects",
            &src_pool_url,
            "--to",
            &dst_mani_url,
            "--to-objects",
            "custom://external/dest/pool",
        ],
        &cache,
    );
    assert!(
        !to_ext.status.success(),
        "an external --to-objects URL must be rejected (non-zero exit)"
    );
    let to_err = String::from_utf8_lossy(&to_ext.stderr);
    assert!(
        to_err.contains("in-process") || to_err.contains("not supported"),
        "the rejection must name the in-process-only contract, got:\n{to_err}"
    );
    assert_eq!(
        count_manifests(&dst_mani),
        0,
        "a rejected external --to-objects sync must not publish a dest manifest"
    );
}

// ===========================================================================
// PER-SIDE --from-objects/--to-objects are INDEPENDENT of the global
// --objects-store (the global must not influence sync routing)
// ===========================================================================

/// SPEC (per-side distinct from the global `--objects-store`): the SPEC names
/// `--from-objects`/`--to-objects` as DISTINCT from the global `--objects-store`.
/// Pin that the global is IGNORED by sync routing: even with a global
/// `--objects-store` pointed at a bogus/empty pool present on the SAME command,
/// the per-side flags drive the routing — objects are READ from `--from-objects`
/// and WRITTEN to `--to-objects`, and the bogus global pool is never touched
/// (stays empty) and never masks the per-side pools. Proves the per-side flags
/// are not silently aliased to / overridden by the global.
#[test]
fn sync_split_objects_flags_independent_of_global_objects_store() {
    let src = temp_dir("ind-src");
    let src_mani = temp_dir("ind-srcmani");
    let src_pool = temp_dir("ind-srcpool");
    let dst_mani = temp_dir("ind-dstmani");
    let dst_pool = temp_dir("ind-dstpool");
    let bogus_global = temp_dir("ind-bogusglobal");
    let cache = temp_dir("ind-cache");

    let leaves = sample_leaves();
    build_tree(&src, leaves);
    let src_str = src.to_string_lossy().into_owned();

    let src_mani_url = file_url(&src_mani);
    let src_pool_url = file_url(&src_pool);
    let dst_mani_url = file_url(&dst_mani);
    let dst_pool_url = file_url(&dst_pool);
    let bogus_global_url = file_url(&bogus_global);

    let src_id = run_ok(
        &[
            "push",
            "--objects-store",
            &src_pool_url,
            "--store",
            &src_mani_url,
            &src_str,
        ],
        &cache,
    );

    // Sync with the per-side flags AND a global --objects-store at a bogus, empty
    // pool. The global must be ignored: routing follows --from-objects/--to-objects.
    let out = run_raw(
        &[
            "sync",
            "--objects-store",
            &bogus_global_url,
            "--id",
            &src_id,
            "--from",
            &src_mani_url,
            "--from-objects",
            &src_pool_url,
            "--to",
            &dst_mani_url,
            "--to-objects",
            &dst_pool_url,
        ],
        &cache,
    );
    assert!(
        out.status.success(),
        "sync must succeed routing by the per-side flags, ignoring the global \
         --objects-store\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The blobs landed in the per-side DEST pool, NOT the bogus global pool.
    assert_eq!(
        count_pool_objects(&dst_pool),
        leaves.len(),
        "objects must land in --to-objects, not the global --objects-store pool"
    );
    assert_eq!(
        count_pool_objects(&bogus_global),
        0,
        "the global --objects-store pool must be untouched by sync (ignored)"
    );
    // Manifest landed in --to; the dest serves the snapshot's id.
    assert_eq!(
        count_manifests(&dst_mani),
        1,
        "the manifest must land in --to"
    );
    assert!(
        dest_serves_manifest_id(&dst_mani, &src_id),
        "the dest .manifests/ tree must physically hold the synced id {src_id}"
    );

    // And it is fully retrievable from the per-side dest split.
    let dest = temp_dir("ind-out");
    let dest_str = dest.to_string_lossy().into_owned();
    let pullcache = temp_dir("ind-pullcache");
    run_ok(
        &[
            "pull",
            "--objects-store",
            &dst_pool_url,
            "--store",
            &dst_mani_url,
            "--id",
            &src_id,
            &dest_str,
        ],
        &pullcache,
    );
    assert_tree_contents(&dest, leaves);
    assert_eq!(
        run_ok(&["id", &dest_str], &pullcache),
        src_id,
        "the per-side-routed sync must leave the dest fully serving the snapshot"
    );
}
