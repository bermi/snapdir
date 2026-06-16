//! Adversarial integration suite for the `list_manifest_ids` primitive
//! (phase 28, gate `manifest-listing-spec-tests`).
//!
//! BLACK-BOX: authored from the gate SPEC ALONE, with NO visibility into a
//! `list_manifest_ids` implementation (none exists yet). It will NOT compile/
//! pass until the stores-impl teammate moves this file into
//! `crates/snapdir-stores/tests/manifest_list.rs` and wires the method shape.
//! Do NOT weaken any assertion to make it green — if a behavior here fails
//! against the landed impl, that is a real bug in the impl, not in this test.
//!
//! SPEC under test — `list_manifest_ids(&self) -> Result<Vec<String>, StoreError>`
//! enumerates the snapshot ids present under a store prefix's `.manifests/`
//! tree. It lives on `StreamStore` (default impl returns a `StoreError` —
//! "listing unsupported" — so non-listing stores fail closed) and is overridden
//! by the in-process backends (`FileStore` walks the `.manifests/` shard tree
//! and reconstructs the 64-hex id from the `3/3/3/rest` shard segments).
//!
//! CONTRACT pinned here:
//!   - each id returned AT MOST ONCE (dedup);
//!   - ONLY valid ids matching `^[0-9a-f]{64}$` — a stray / non-manifest key
//!     under `.manifests/` is IGNORED, not an error;
//!   - ORDER is unspecified — every assertion SORTS both sides before comparing.
//!
//! Construction shape assumed (the impl teammate may adjust the exact call site
//! while preserving these behaviors): `store.list_manifest_ids()` returning
//! `Result<Vec<String>, StoreError>`, over a `FileStore` (and a `SplitStore`
//! over two `FileStore` prefixes for the shared-pool isolation case). If the
//! real signature differs, fix ONLY the call shape, never the assertions.

// The negative-membership assertions are written as `!v.iter().any(|x| *x ==
// needle)` for readability; clippy prefers `!v.contains(&needle)`. Allowing the
// style lints here keeps every assertion byte-for-byte as authored (no logic /
// strength change) under the crate's `-D warnings` gate.
#![allow(clippy::manual_contains, clippy::explicit_auto_deref)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, Store, StoreError};

use snapdir_stores::{FileStore, SplitStore, StreamStore};

// ---------------------------------------------------------------------------
// Test scaffolding (no dev-dependencies; mirrors the existing split/shim tests).
// ---------------------------------------------------------------------------

/// A unique temp dir removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "snapdir-manifest-list-test-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Builds a real source tree under `src` and returns the matching `Manifest`
/// plus its snapshot id. Mirrors the split-store fixture: object addressing uses
/// the NON-keyed `Blake3Hasher`, a single `D ./` root entry keeps the manifest a
/// valid snapshot, and `snapshot_id` over the sorted manifest yields the id the
/// store files the manifest under. `tag_byte` lets callers produce DISTINCT ids
/// trivially (different content => different snapshot id).
fn build_tree(src: &Path, files: &[(&str, &[u8])]) -> (Manifest, String) {
    let hasher = Blake3Hasher::new();
    let mut manifest = Manifest::new();

    let mut file_sums: Vec<String> = Vec::new();
    for (rel, content) in files {
        let target = src.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&target, content).unwrap();
        let sum = hasher.hash_hex(content);
        file_sums.push(sum.clone());
        manifest.push(ManifestEntry::new(
            PathType::File,
            "600",
            sum,
            content.len() as u64,
            format!("./{rel}"),
        ));
    }

    let root_sum = directory_checksum(file_sums.iter().map(String::as_str), &hasher);
    let root_size: u64 = files.iter().map(|(_, c)| c.len() as u64).sum();
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        root_size,
        "./",
    ));

    manifest.sort();
    let id = snapshot_id(&manifest, &hasher);
    (manifest, id)
}

/// Seeds N distinct snapshots into a `StreamStore` via the public
/// `put_manifest`, returning their ids. Each snapshot is a one-file tree whose
/// file content is `seed-<i>` — distinct content => distinct snapshot id, so the
/// returned ids are guaranteed unique and 64-hex.
fn seed_manifests<S: StreamStore>(store: &S, n: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let src = TempDir::new(&format!("seed-{i}"));
        let body = format!("seed-{i}\n");
        let (manifest, id) = build_tree(src.path(), &[("f", body.as_bytes())]);
        store.put_manifest(&id, &manifest).expect("put_manifest");
        ids.push(id);
    }
    ids
}

/// Sorts a vec of ids so order-unspecified results can be compared by set.
fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// `assert_eq` on the SORTED ids (the SPEC says order is unspecified — the
/// caller sorts), with a uniqueness guard so a list that secretly duplicated an
/// id can't accidentally sort-equal an expected set.
fn assert_same_set(got: Vec<String>, mut expected: Vec<String>) {
    let unique: HashSet<&String> = got.iter().collect();
    assert_eq!(
        unique.len(),
        got.len(),
        "list_manifest_ids must return each id AT MOST ONCE (dedup); got duplicates in {got:?}"
    );
    expected.sort();
    assert_eq!(sorted(got), expected);
}

// ===========================================================================
// KNOWN-SET ROUND-TRIP
// ===========================================================================

#[test]
fn list_known_set_roundtrip_returns_exactly_the_put_ids() {
    // SPEC: put_manifest a set of N manifests into a FileStore, then
    // list_manifest_ids returns EXACTLY those N ids (sorted-equal).
    let root = TempDir::new("known");
    let store = FileStore::from_root(root.path().to_path_buf());

    let ids = seed_manifests(&store, 5);

    let listed = store.list_manifest_ids().expect("list_manifest_ids");
    assert_same_set(listed, ids);
}

#[test]
fn list_single_manifest_returns_just_that_id() {
    // SPEC: round-trip with N=1 — the simplest non-empty case.
    let root = TempDir::new("single");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("single-src");
    let (manifest, id) = build_tree(src.path(), &[("only", b"only file\n")]);
    store.put_manifest(&id, &manifest).expect("put_manifest");

    assert_same_set(store.list_manifest_ids().expect("list"), vec![id]);
}

#[test]
fn list_reflects_ids_written_through_full_push() {
    // SPEC: ids written via the full `Store::push` path (not just put_manifest)
    // are listed too — list reads the on-disk `.manifests/` tree, however it was
    // populated.
    let root = TempDir::new("push");
    let store = FileStore::from_root(root.path().to_path_buf());

    let mut ids = Vec::new();
    for i in 0..3 {
        let src = TempDir::new(&format!("push-src-{i}"));
        let body = format!("pushed-{i}\n");
        let (manifest, id) = build_tree(src.path(), &[("f", body.as_bytes())]);
        store.push(&manifest, src.path()).expect("push");
        ids.push(id);
    }

    assert_same_set(store.list_manifest_ids().expect("list"), ids);
}

// ===========================================================================
// DEDUP
// ===========================================================================

#[test]
fn list_dedups_a_reput_manifest_to_a_single_id() {
    // SPEC: each id returned AT MOST ONCE. Putting the SAME manifest twice (an
    // idempotent re-put writes the same sharded path) must still list its id
    // exactly ONCE — never twice.
    let root = TempDir::new("dedup");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("dedup-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"dup\n")]);
    store.put_manifest(&id, &manifest).expect("put once");
    store.put_manifest(&id, &manifest).expect("put again");

    let listed = store.list_manifest_ids().expect("list");
    assert_eq!(
        listed.iter().filter(|x| **x == id).count(),
        1,
        "a re-put manifest must appear exactly once: {listed:?}"
    );
    assert_same_set(listed, vec![id]);
}

// ===========================================================================
// HEX-ONLY FILTERING / MALFORMED-SHARD SKIP (ignored, NOT an error)
// ===========================================================================

#[test]
fn list_ignores_a_short_non_hex_shard_path_without_error() {
    // SPEC: a stray / non-manifest key under `.manifests/` (e.g. a short or
    // non-hex shard path) is IGNORED, not an error. Plant ONE real manifest plus
    // a too-short shard tree that cannot reconstruct a 64-hex id; list must
    // return ONLY the real id (no error, no garbage).
    let root = TempDir::new("short");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("short-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"real\n")]);
    store.put_manifest(&id, &manifest).expect("put real");

    // A stray file whose 3/3/3/rest reconstruction is far too short for 64 hex.
    let stray = root.path().join(".manifests").join("aaa").join("bbb");
    fs::create_dir_all(&stray).unwrap();
    fs::write(stray.join("c"), b"junk").unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("a malformed shard path must be skipped, NOT error");
    assert_same_set(listed, vec![id]);
}

#[test]
fn list_ignores_non_hex_characters_in_a_full_length_shard_path() {
    // SPEC: ONLY ids matching `^[0-9a-f]{64}$` are returned. Plant a shard tree
    // whose segments reconstruct to a 64-CHARACTER but NON-hex string (contains
    // 'z'); it must be filtered out, leaving only the genuine manifest id.
    let root = TempDir::new("nonhex");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("nonhex-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"genuine\n")]);
    store.put_manifest(&id, &manifest).expect("put real");

    // 64 chars total across the 3/3/3/55 shard split, but with non-hex 'z's.
    let s0 = "zzz";
    let s1 = "zzz";
    let s2 = "zzz";
    let rest = "z".repeat(55);
    assert_eq!(s0.len() + s1.len() + s2.len() + rest.len(), 64);
    let bogus = root.path().join(".manifests").join(s0).join(s1).join(s2);
    fs::create_dir_all(&bogus).unwrap();
    fs::write(bogus.join(&rest), b"not a manifest").unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("a non-hex shard path must be skipped, NOT error");
    assert_same_set(listed, vec![id]);
}

#[test]
fn list_ignores_uppercase_hex_shard_path() {
    // SPEC: the regex is lowercase `^[0-9a-f]{64}$` — an UPPERCASE 64-hex id (an
    // out-of-spec key snapdir never writes) must be IGNORED, not surfaced.
    let root = TempDir::new("upper");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("upper-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"lower\n")]);
    store.put_manifest(&id, &manifest).expect("put real");

    // An uppercased copy of a valid id, planted at its (uppercased) shard path.
    let upper = id.to_uppercase();
    let p = manifest_path(&upper); // 3/3/3/rest split of the uppercase string
    let disk = root.path().join(&p);
    fs::create_dir_all(disk.parent().unwrap()).unwrap();
    fs::write(&disk, b"uppercase impostor").unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("an uppercase-hex key must be skipped, NOT error");
    assert!(
        !listed.iter().any(|x| *x == upper),
        "uppercase id must not be listed: {listed:?}"
    );
    assert_same_set(listed, vec![id]);
}

#[test]
fn list_ignores_an_extra_directory_level_under_manifests() {
    // SPEC: a non-manifest key whose path has the WRONG shard depth (an extra
    // nested dir so the reconstruction can't be a clean 3/3/3/rest 64-hex id)
    // is ignored, not an error. Mixes a valid id with the malformed sibling.
    let root = TempDir::new("depth");
    let store = FileStore::from_root(root.path().to_path_buf());

    let ids = seed_manifests(&store, 2);

    // Bury a file one level too deep under a plausible-looking shard prefix.
    let deep = root
        .path()
        .join(".manifests")
        .join("abc")
        .join("def")
        .join("012")
        .join("extra-level");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("blob"), b"too deep").unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("an over-deep shard path must be skipped, NOT error");
    assert_same_set(listed, ids);
}

// ===========================================================================
// ORDER-INSENSITIVE (caller sorts)
// ===========================================================================

#[test]
fn list_is_order_insensitive_compare_as_a_set() {
    // SPEC: ORDER is unspecified — the caller sorts. We assert the SET equality
    // after sorting; we must NOT depend on insertion or filesystem-walk order.
    let root = TempDir::new("order");
    let store = FileStore::from_root(root.path().to_path_buf());

    // Seed several manifests; their ids land in pseudo-random hash order.
    let ids = seed_manifests(&store, 7);

    let listed = store.list_manifest_ids().expect("list");
    // Sorted-equal is the ONLY contract; the raw order is not asserted.
    assert_same_set(listed, ids);
}

#[test]
fn list_two_calls_yield_the_same_set() {
    // SPEC: idempotent read — listing twice over an unchanged store yields the
    // same SET (sorted-equal), no spurious additions/drops between calls.
    let root = TempDir::new("twice");
    let store = FileStore::from_root(root.path().to_path_buf());
    let _ids = seed_manifests(&store, 4);

    let a = sorted(store.list_manifest_ids().expect("first"));
    let b = sorted(store.list_manifest_ids().expect("second"));
    assert_eq!(a, b, "two listings of an unchanged store must be set-equal");
}

// ===========================================================================
// EMPTY PREFIX (empty vec, NOT an error)
// ===========================================================================

#[test]
fn list_empty_prefix_no_manifests_dir_returns_empty_vec() {
    // SPEC empty prefix: no `.manifests/` tree yet => EMPTY vec, NOT an error.
    let root = TempDir::new("empty-none");
    let store = FileStore::from_root(root.path().to_path_buf());

    let listed = store
        .list_manifest_ids()
        .expect("an absent .manifests/ must yield Ok(empty), not an error");
    assert!(
        listed.is_empty(),
        "no manifests => empty vec, got {listed:?}"
    );
}

#[test]
fn list_empty_manifests_dir_returns_empty_vec() {
    // SPEC empty prefix: an EXISTING but empty `.manifests/` dir => empty vec.
    let root = TempDir::new("empty-dir");
    fs::create_dir_all(root.path().join(".manifests")).unwrap();
    let store = FileStore::from_root(root.path().to_path_buf());

    let listed = store
        .list_manifest_ids()
        .expect("an empty .manifests/ must yield Ok(empty), not an error");
    assert!(
        listed.is_empty(),
        "empty .manifests/ => empty vec, got {listed:?}"
    );
}

#[test]
fn list_prefix_with_only_objects_no_manifests_returns_empty_vec() {
    // SPEC: a store that has pushed OBJECTS but no manifest yet (e.g. only
    // `.objects/` populated) lists ZERO manifests, not an error and not the
    // object shards.
    let root = TempDir::new("only-objects");
    let store = FileStore::from_root(root.path().to_path_buf());

    let blob = b"loose object\n".to_vec();
    let sum = Blake3Hasher::new().hash_hex(&blob);
    store.put_object(&sum, blob).expect("put_object");

    let listed = store.list_manifest_ids().expect("list");
    assert!(
        listed.is_empty(),
        "objects without manifests => empty vec, got {listed:?}"
    );
    assert!(
        !listed.iter().any(|x| *x == sum),
        "an object checksum must never be reported as a manifest id"
    );
}

// ===========================================================================
// NONEXISTENT ROOT (Err, NOT an empty vec) — the §6 silent-empty-store bug
// ===========================================================================

#[test]
fn list_nonexistent_root_errors_not_empty() {
    // SPEC: a store whose ROOT directory does not exist (a typo'd / never-created
    // location) must NOT masquerade as an empty store — that fabricates a full
    // deletion delta downstream (`diff --to file:///nope` exit 0 + bogus `D`).
    // It must return an Err whose message NAMES the missing location.
    let parent = TempDir::new("nonexistent-root");
    let missing = parent.path().join("no-such-store-subdir");
    assert!(!missing.exists(), "precondition: root must not exist");

    let store = FileStore::from_root(missing.clone());
    let err = store
        .list_manifest_ids()
        .expect_err("a nonexistent store root must error, not return Ok(empty)");

    // The error must name the bad location so the operator can see the typo.
    let needle = missing.display().to_string();
    let rendered = err.to_string();
    assert!(
        rendered.contains(&needle),
        "error must name the missing store location {needle:?}, got: {rendered}"
    );
}

#[test]
fn list_existing_empty_root_is_ok_not_error() {
    // CRITICAL DISTINCTION control: an EXISTING store dir that legitimately has
    // no manifests yet (a fresh push target — real dir, no `.manifests/`) must
    // STILL return Ok(empty), never the nonexistent-root error.
    let root = TempDir::new("fresh-existing-empty");
    assert!(root.path().exists(), "precondition: root exists");

    let store = FileStore::from_root(root.path().to_path_buf());
    let listed = store
        .list_manifest_ids()
        .expect("an existing empty store root must be Ok(empty), not an error");
    assert!(
        listed.is_empty(),
        "fresh empty store => empty vec, got {listed:?}"
    );
}

/// REVIEW ADDITION (impl now visible — pin the exact variant + phrasing the src
/// uses): the nonexistent-root error is specifically `StoreError::Backend` (not a
/// raw `Io` `NotFound`), and its message literally says `store location does not
/// exist`. Pinning the variant matters because downstream diff/sync render-and-
/// classify errors; an `Io(NotFound)` could be mistaken for "empty" again, which
/// is exactly the §6 bug. (`FileStore::list_manifest_ids` in `file_store.rs`.)
#[test]
fn list_nonexistent_root_is_backend_variant_naming_does_not_exist() {
    let parent = TempDir::new("nonexistent-root-variant");
    let missing = parent.path().join("typo-store");
    assert!(!missing.exists(), "precondition: root must not exist");

    let store = FileStore::from_root(missing.clone());
    let err = store
        .list_manifest_ids()
        .expect_err("a nonexistent store root must error");

    match &err {
        StoreError::Backend { message, .. } => {
            assert!(
                message.contains("does not exist"),
                "the Backend error must say 'does not exist'; got: {message}"
            );
            assert!(
                message.contains(&missing.display().to_string()),
                "the Backend error must name the missing location; got: {message}"
            );
        }
        other => panic!(
            "a nonexistent root must be StoreError::Backend (not e.g. Io NotFound, \
             which downstream could re-read as 'empty'); got: {other:?}"
        ),
    }
}

// ===========================================================================
// SHARED-POOL ISOLATION (SplitStore over two FileStore prefixes, one pool)
// ===========================================================================

#[test]
fn list_split_lists_only_its_own_manifests_not_the_shared_pool_objects() {
    // SPEC shared-pool isolation: a manifest store that SHARES its `.objects`
    // pool lists ONLY its OWN `.manifests/` ids — never the pool's objects. Push
    // a real tree through a SplitStore (objects -> shared pool, manifest -> the
    // manifests side); listing must return that one manifest id and NONE of the
    // object checksums sitting in the shared pool's `.objects`.
    let pool = TempDir::new("iso-pool");
    let man = TempDir::new("iso-man");
    let src = TempDir::new("iso-src");

    let objects = FileStore::from_root(pool.path().to_path_buf());
    let manifests = FileStore::from_root(man.path().to_path_buf());
    let store = SplitStore::new(objects, manifests);

    let (manifest, id) = build_tree(
        src.path(),
        &[("a", b"alpha\n"), ("b", b"beta\n"), ("c", b"gamma\n")],
    );
    store.push(&manifest, src.path()).expect("push");

    let listed = store.list_manifest_ids().expect("list");
    // Exactly the one manifest id — no object checksums from the shared pool.
    assert_same_set(listed.clone(), vec![id.clone()]);
    let objects: [(&str, &[u8]); 3] = [("a", b"alpha\n"), ("b", b"beta\n"), ("c", b"gamma\n")];
    for (_, content) in &objects {
        let osum = Blake3Hasher::new().hash_hex(*content);
        assert!(
            !listed.iter().any(|x| *x == osum),
            "a shared-pool object checksum {osum} must NOT be listed as a manifest id"
        );
    }
}

#[test]
fn list_split_two_prefixes_over_one_pool_do_not_see_each_others_manifests() {
    // SPEC shared-pool isolation: two manifest prefixes A and B sharing ONE
    // objects pool each list ONLY their own `.manifests/` ids — never the other
    // prefix's manifests. Push DIFFERENT trees to A and B (same shared pool);
    // each side's list is its own single id, disjoint from the other.
    let pool = TempDir::new("two-pool");
    let man_a = TempDir::new("two-man-a");
    let man_b = TempDir::new("two-man-b");
    let src_a = TempDir::new("two-src-a");
    let src_b = TempDir::new("two-src-b");

    let store_a = SplitStore::new(
        FileStore::from_root(pool.path().to_path_buf()),
        FileStore::from_root(man_a.path().to_path_buf()),
    );
    let store_b = SplitStore::new(
        FileStore::from_root(pool.path().to_path_buf()),
        FileStore::from_root(man_b.path().to_path_buf()),
    );

    let (man_tree_a, id_a) = build_tree(src_a.path(), &[("only-a", b"distinct A bytes\n")]);
    let (man_tree_b, id_b) = build_tree(src_b.path(), &[("only-b", b"distinct B bytes\n")]);
    store_a.push(&man_tree_a, src_a.path()).expect("push A");
    store_b.push(&man_tree_b, src_b.path()).expect("push B");

    assert_ne!(id_a, id_b, "the two trees must have distinct ids");

    let listed_a = store_a.list_manifest_ids().expect("list A");
    let listed_b = store_b.list_manifest_ids().expect("list B");

    assert_same_set(listed_a.clone(), vec![id_a.clone()]);
    assert_same_set(listed_b.clone(), vec![id_b.clone()]);
    assert!(
        !listed_a.iter().any(|x| *x == id_b),
        "prefix A must NOT see prefix B's manifest id"
    );
    assert!(
        !listed_b.iter().any(|x| *x == id_a),
        "prefix B must NOT see prefix A's manifest id"
    );
}

#[test]
fn list_filestore_isolated_from_objects_planted_under_its_own_objects_dir() {
    // SPEC: list reads ONLY `.manifests/` — a plain FileStore with both objects
    // AND manifests must list its manifest ids and NEVER any `.objects` shard,
    // even though both trees use the IDENTICAL 3/3/3/rest sharding.
    let root = TempDir::new("colo-iso");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("colo-iso-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"colo bytes\n")]);
    store.push(&manifest, src.path()).expect("push");

    // After a push there's at least one `.objects/<shard>/...` blob present.
    let listed = store.list_manifest_ids().expect("list");
    let osum = Blake3Hasher::new().hash_hex(b"colo bytes\n");
    assert!(
        !listed.iter().any(|x| *x == osum),
        "the object checksum {osum} (under .objects/) must not be listed"
    );
    assert_same_set(listed, vec![id]);
}

// ===========================================================================
// DEFAULT-IMPL FAIL-CLOSED (a StreamStore that does NOT override the method)
// ===========================================================================

/// A minimal `StreamStore` that does NOT override `list_manifest_ids`, so it
/// inherits the trait's DEFAULT impl. Per the SPEC the default fails closed —
/// returning a `StoreError` ("listing unsupported") — so non-listing stores
/// never silently claim an empty/garbage listing. Every other method is a
/// trivial stub; only `list_manifest_ids` is under test here.
struct NonListingStore;

impl Store for NonListingStore {
    fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError> {
        Err(StoreError::ManifestNotFound { id: id.to_string() })
    }
    fn fetch_files(&self, _manifest: &Manifest, _dest: &Path) -> Result<(), StoreError> {
        Ok(())
    }
    fn push(&self, _manifest: &Manifest, _source: &Path) -> Result<(), StoreError> {
        Ok(())
    }
}

impl StreamStore for NonListingStore {
    fn has_object(&self, _checksum: &str) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::ObjectNotFound {
            checksum: checksum.to_string(),
        })
    }
    fn put_object(&self, _checksum: &str, _bytes: Vec<u8>) -> Result<(), StoreError> {
        Ok(())
    }
    fn put_manifest(&self, _id: &str, _manifest: &Manifest) -> Result<(), StoreError> {
        Ok(())
    }
    // NOTE: deliberately does NOT override `list_manifest_ids` — it must inherit
    // the fail-closed default.
}

#[test]
fn list_default_impl_fails_closed_with_store_error() {
    // SPEC default-impl fail-closed: a StreamStore that does NOT override
    // list_manifest_ids returns a StoreError ("listing unsupported"), never
    // Ok(empty). A store that cannot enumerate must NOT pretend it has zero
    // snapshots.
    let store = NonListingStore;
    let result = store.list_manifest_ids();
    assert!(
        result.is_err(),
        "the default list_manifest_ids must fail closed (Err), got {result:?}"
    );
    // It should be a Backend-style "unsupported" error, not a NotFound/Integrity.
    match result {
        Err(StoreError::Backend { .. }) => {}
        Err(other) => {
            panic!("expected a Backend(\"listing unsupported\")-style error, got {other:?}")
        }
        Ok(v) => panic!("expected fail-closed Err, got Ok({v:?})"),
    }
}

// ===========================================================================
// SHARD-RECONSTRUCTION BOUNDARIES (now-visible impl: the helper requires
// EXACTLY 4 segments `3/3/3/rest` AND total length == 64 hex). These pin the
// `segments.len() != 4` and `is_hex64` length branches that the black-box
// suite could only approach indirectly.
//
// NOTE: S3/GCS/B2 reuse the SAME reconstruction (`manifest_ids_from_keys` ->
// `manifest_id_from_shard_segments`), so these FileStore boundary tests cover
// that shared key->id logic. The S3/GCS LIVE paths (the listing transport
// itself) are creds-gated and not exercised here.
// ===========================================================================

/// A valid lowercase 64-hex id, planted at its real `3/3/3/rest` shard path.
/// Distinct from any seeded snapshot id; used to drive the boundary plants.
const VALID_ID: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

#[test]
fn list_ignores_a_too_shallow_key_whose_first_segments_already_total_64() {
    // SPEC + IMPL (`segments.len() != 4`): a key at the WRONG (too-SHALLOW)
    // depth must be skipped even when its segments happen to concatenate to a
    // 64-hex string. Plant `.manifests/<32hex>/<32hex>` (depth 2, concat == 64
    // valid hex). is_hex64 alone would accept the concat, so this pins that the
    // DEPTH==4 guard runs FIRST and rejects it. Mixed with one real manifest.
    let root = TempDir::new("shallow64");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("shallow64-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"shallow real\n")]);
    store.put_manifest(&id, &manifest).expect("put real");

    // Two 32-hex halves of VALID_ID => concat is the valid 64-hex id, but only
    // TWO segments deep, not four.
    let half_a = &VALID_ID[..32];
    let half_b = &VALID_ID[32..];
    let shallow = root.path().join(".manifests").join(half_a);
    fs::create_dir_all(&shallow).unwrap();
    fs::write(shallow.join(half_b), b"too shallow").unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("a too-shallow key must be skipped, NOT error");
    assert!(
        !listed.iter().any(|x| *x == VALID_ID),
        "a depth-2 key that concatenates to 64-hex must NOT be listed: {listed:?}"
    );
    assert_same_set(listed, vec![id]);
}

#[test]
fn list_ignores_a_correct_depth_key_whose_rest_makes_total_length_below_64() {
    // SPEC + IMPL (`is_hex64` length check at depth 4): a key at the CORRECT
    // `3/3/3/rest` depth whose segments are all valid hex but whose total
    // length is 63 (one short of 64) must be skipped — depth is right, length
    // is wrong. Pins the length arm of is_hex64 independently of the depth arm.
    let root = TempDir::new("len63");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("len63-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"len63 real\n")]);
    store.put_manifest(&id, &manifest).expect("put real");

    // 3 + 3 + 3 + 54 = 63 valid-hex chars across the correct 4 segments.
    let s0 = &VALID_ID[0..3];
    let s1 = &VALID_ID[3..6];
    let s2 = &VALID_ID[6..9];
    let rest = "a".repeat(54);
    assert_eq!(s0.len() + s1.len() + s2.len() + rest.len(), 63);
    let short = root.path().join(".manifests").join(s0).join(s1).join(s2);
    fs::create_dir_all(&short).unwrap();
    fs::write(short.join(&rest), b"one char short").unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("a 63-char (length != 64) key must be skipped, NOT error");
    assert_same_set(listed, vec![id]);
}

#[test]
fn list_ignores_a_correct_depth_key_whose_rest_makes_total_length_above_64() {
    // SPEC + IMPL (`is_hex64` length check at depth 4): the symmetric over-long
    // case — correct depth, all valid hex, total length 65 (one over 64). Must
    // be skipped; pins that is_hex64 rejects > 64 just as it rejects < 64.
    let root = TempDir::new("len65");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("len65-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"len65 real\n")]);
    store.put_manifest(&id, &manifest).expect("put real");

    // 3 + 3 + 3 + 56 = 65 valid-hex chars across the correct 4 segments.
    let s0 = &VALID_ID[0..3];
    let s1 = &VALID_ID[3..6];
    let s2 = &VALID_ID[6..9];
    let rest = "b".repeat(56);
    assert_eq!(s0.len() + s1.len() + s2.len() + rest.len(), 65);
    let long = root.path().join(".manifests").join(s0).join(s1).join(s2);
    fs::create_dir_all(&long).unwrap();
    fs::write(long.join(&rest), b"one char long").unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("a 65-char (length != 64) key must be skipped, NOT error");
    assert_same_set(listed, vec![id]);
}

#[test]
fn list_does_not_list_a_64hex_value_that_is_a_directory_name_not_a_leaf() {
    // SPEC + IMPL (the walk inserts ONLY on `is_dir() == false`): a 64-hex id
    // that exists in the tree purely as the NAME of a DIRECTORY (the leaf
    // `rest` segment is a dir, with no file under it) must NOT be listed —
    // list reconstructs ids from FILE leaves, not directory nodes. This pins
    // the file-vs-dir classification in `push_manifest_walk_entry`.
    let root = TempDir::new("dir-leaf");
    let store = FileStore::from_root(root.path().to_path_buf());

    let src = TempDir::new("dir-leaf-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"dir-leaf real\n")]);
    store.put_manifest(&id, &manifest).expect("put real");

    // Build the real `3/3/3/rest` shard path of VALID_ID, but make `rest` a
    // DIRECTORY (mkdir, no file leaf under it). The reconstruction-to-64-hex is
    // valid, yet there is no FILE leaf, so it must not surface.
    let p = manifest_path(VALID_ID); // ".manifests/abc/def/012/3456...."
    let as_dir = root.path().join(&p);
    fs::create_dir_all(&as_dir).unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("a 64-hex directory node must be skipped, NOT error");
    assert!(
        !listed.iter().any(|x| *x == VALID_ID),
        "a 64-hex value that is only a directory name must NOT be listed: {listed:?}"
    );
    assert_same_set(listed, vec![id]);
}

#[test]
fn list_returns_only_the_valids_among_several_mixed_invalid_keys() {
    // SPEC + IMPL: a tree mixing SEVERAL distinct malformed keys (wrong depth,
    // wrong length, non-hex, uppercase) with SEVERAL genuine manifests returns
    // EXACTLY the genuine ids — every invalid skipped without error, no valid
    // dropped. Exercises the accumulate-while-skipping loop over many entries.
    let root = TempDir::new("mixed");
    let store = FileStore::from_root(root.path().to_path_buf());

    // Several genuine manifests.
    let ids = seed_manifests(&store, 3);

    let manifests = root.path().join(".manifests");

    // (a) too shallow: depth-1 file directly under .manifests/.
    fs::write(manifests.join("not-a-shard"), b"x").unwrap();

    // (b) too deep: 5 segments.
    let deep = manifests.join("aaa").join("bbb").join("ccc").join("ddd");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("eee"), b"x").unwrap();

    // (c) correct depth, non-hex leaf (contains 'z'), length 64.
    let nonhex = manifests.join("zzz").join("zzz").join("zzz");
    fs::create_dir_all(&nonhex).unwrap();
    fs::write(nonhex.join("z".repeat(55)), b"x").unwrap();

    // (d) uppercase 64-hex at its uppercased shard path.
    let up = manifest_path(&VALID_ID.to_uppercase());
    let up_disk = root.path().join(&up);
    fs::create_dir_all(up_disk.parent().unwrap()).unwrap();
    fs::write(&up_disk, b"x").unwrap();

    // (e) correct depth, valid hex, wrong length (63).
    let short = manifests
        .join(&VALID_ID[0..3])
        .join(&VALID_ID[3..6])
        .join(&VALID_ID[6..9]);
    fs::create_dir_all(&short).unwrap();
    fs::write(short.join("c".repeat(54)), b"x").unwrap();

    let listed = store
        .list_manifest_ids()
        .expect("a mix of malformed keys must be skipped, NOT error");
    assert_same_set(listed, ids);
}
