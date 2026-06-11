//! Adversarial integration suite for `SplitStore` (phase 28, gate
//! `split-store-spec-tests`).
//!
//! BLACK-BOX: authored from the gate SPEC alone, with NO visibility into a
//! `SplitStore` implementation (none exists yet). It will NOT compile/pass until
//! the stores-impl teammate moves this file into
//! `crates/snapdir-stores/tests/split.rs` and wires the constructor shape. Do
//! not weaken any assertion to make it green — if a behavior here fails against
//! the landed impl, that is a real bug in the impl, not in this test.
//!
//! `SplitStore` wraps TWO stores:
//!   - an `objects` pool — only its `.objects/` content-addressed blobs are used;
//!   - a `manifests` location — only its `.manifests/<id>` is used.
//!
//! Object ops (`has`/`get`/`put_object`, `objects_needed`) route to the OBJECTS
//! store; manifest ops (`get`/`put_manifest`) route to the MANIFESTS store. It
//! implements both `Store` and `StreamStore`.
//!
//! Construction shape assumed (the impl teammate may adjust the exact call site
//! while preserving these behaviors): `SplitStore::new(objects_store,
//! manifests_store)` over two `FileStore`s. If the real constructor differs
//! (e.g. takes URLs / owned vs borrowed), fix ONLY the call shape, never the
//! assertions.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};

use snapdir_stores::{FileStore, SplitStore, StreamStore};

// ---------------------------------------------------------------------------
// Test scaffolding (no dev-dependencies; mirrors the existing shim test).
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
            "snapdir-split-test-{}-{tag}-{n}",
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

/// Builds a `SplitStore` over a `FileStore` rooted at `objects_root` (the pool)
/// and a `FileStore` rooted at `manifests_root` (the manifest location).
fn split_over(objects_root: &Path, manifests_root: &Path) -> SplitStore {
    let objects = FileStore::from_root(objects_root.to_path_buf());
    let manifests = FileStore::from_root(manifests_root.to_path_buf());
    SplitStore::new(objects, manifests)
}

/// Writes a real source tree under `src` and returns the matching `Manifest`
/// plus its snapshot id. Files are `(relative path, content)`. A `D ./` root
/// entry is always synthesized; nested directory entries are synthesized for
/// each unique parent component. Object addressing uses the NON-keyed
/// `Blake3Hasher` (the content-address hasher the file store files objects
/// under), exactly as the shipped shim/file-store tests do.
fn build_tree(src: &Path, files: &[(&str, &[u8])]) -> (Manifest, String) {
    let hasher = Blake3Hasher::new();
    let mut manifest = Manifest::new();

    // Materialize the files on disk and collect their entries + checksums.
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

    // A single root directory entry keeps the manifest a valid snapshot. Its
    // checksum is the directory_checksum over the direct file children — exact
    // shape is not the contract under test here (the SPEC pins OBJECT + manifest
    // BYTES, and objects are files only), so a deterministic root entry that is
    // identical across colocated/split builds is all parity requires.
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

/// Counts the regular files under `<root>/.objects` (the blob count the SPEC's
/// invariant (a) asserts is unchanged on a second push of the same tree).
fn count_objects(root: &Path) -> usize {
    fn walk(dir: &Path, acc: &mut usize) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, acc);
                } else if p.is_file() {
                    *acc += 1;
                }
            }
        }
    }
    let mut n = 0;
    walk(&root.join(".objects"), &mut n);
    n
}

/// Returns the on-disk sharded path of an object blob under `root`.
fn object_disk(root: &Path, checksum: &str) -> PathBuf {
    root.join(object_path(checksum))
}

/// Returns the on-disk sharded path of a manifest under `root`.
fn manifest_disk(root: &Path, id: &str) -> PathBuf {
    root.join(manifest_path(id))
}

// ===========================================================================
// HAPPY-PATH INVARIANTS (a) (b) (c)
// ===========================================================================

#[test]
fn split_routes_object_ops_to_objects_pool_manifest_to_manifests_location() {
    // SPEC routing: object ops -> objects store; manifest ops -> manifests store.
    let objects = TempDir::new("route-obj");
    let manifests = TempDir::new("route-man");
    let src = TempDir::new("route-src");
    let (manifest, id) = build_tree(src.path(), &[("a.txt", b"alpha\n")]);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push");

    // Blobs land ONLY in the objects pool; the manifests location holds NO
    // `.objects`. The manifest lands ONLY in the manifests location.
    let sum = Blake3Hasher::new().hash_hex(b"alpha\n");
    assert!(
        object_disk(objects.path(), &sum).is_file(),
        "object must land in the OBJECTS pool"
    );
    assert!(
        !objects.path().join(".manifests").exists() || !manifest_disk(objects.path(), &id).exists(),
        "manifest must NOT land in the objects pool"
    );
    assert!(
        manifest_disk(manifests.path(), &id).is_file(),
        "manifest must land in the MANIFESTS location"
    );
    assert!(
        !manifests.path().join(".objects").exists() || count_objects(manifests.path()) == 0,
        "objects must NOT land in the manifests location"
    );
}

#[test]
fn split_shared_pool_two_manifest_prefixes_upload_zero_new_objects_second_time() {
    // SPEC invariant (a): two manifest locations A and B sharing ONE objects
    // pool — pushing the SAME tree to A then to B uploads ZERO new objects the
    // second time.
    let objects = TempDir::new("share-obj");
    let man_a = TempDir::new("share-man-a");
    let man_b = TempDir::new("share-man-b");
    let src = TempDir::new("share-src");
    let (manifest, id) = build_tree(
        src.path(),
        &[("x", b"data-x\n"), ("y", b"data-y\n"), ("z", b"data-z\n")],
    );

    let store_a = split_over(objects.path(), man_a.path());
    store_a.push(&manifest, src.path()).expect("push A");
    let after_a = count_objects(objects.path());
    assert!(
        after_a >= 3,
        "all file objects must have landed once: {after_a}"
    );

    let store_b = split_over(objects.path(), man_b.path());
    store_b.push(&manifest, src.path()).expect("push B");
    let after_b = count_objects(objects.path());

    assert_eq!(
        after_a, after_b,
        "second push to a DIFFERENT manifest prefix over the SAME pool must \
         upload ZERO new objects (dedup), but the .objects count changed"
    );
    // Both manifest locations now resolve the snapshot.
    assert!(manifest_disk(man_a.path(), &id).is_file());
    assert!(manifest_disk(man_b.path(), &id).is_file());
}

#[test]
fn split_push_then_fetch_files_roundtrips_byte_identical() {
    // SPEC invariant (b): push -> fetch_files round-trips byte-identical content.
    let objects = TempDir::new("rt-obj");
    let manifests = TempDir::new("rt-man");
    let src = TempDir::new("rt-src");
    let dest = TempDir::new("rt-dest");
    let files: &[(&str, &[u8])] = &[
        ("readme.md", b"# hello\nworld\n"),
        ("nested/deep/leaf.bin", &[0u8, 1, 2, 3, 255, 254, 0]),
        ("unicode-\u{2728}.txt", "spark\u{2728}\n".as_bytes()),
    ];
    let (manifest, _id) = build_tree(src.path(), files);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push");
    store
        .fetch_files(&manifest, dest.path())
        .expect("fetch_files");

    for (rel, content) in files {
        let got = fs::read(dest.path().join(rel)).expect("materialized file");
        assert_eq!(&got, content, "round-trip must be byte-identical for {rel}");
    }
}

#[test]
fn split_layout_parity_objects_bytes_match_colocated_filestore() {
    // SPEC invariant (c) — LAYOUT PARITY: the `.objects` bytes produced by a
    // SplitStore push are byte-for-byte identical to a single COLOCATED
    // FileStore push of the same tree (frozen sharded-layout interop).
    let files: &[(&str, &[u8])] = &[
        ("one", b"first\n"),
        ("dir/two", b"second\n"),
        ("dir/sub/three", b"third\n"),
    ];

    // Colocated reference push.
    let colo = TempDir::new("parity-colo");
    let colo_src = TempDir::new("parity-colo-src");
    let (colo_manifest, _) = build_tree(colo_src.path(), files);
    FileStore::from_root(colo.path().to_path_buf())
        .push(&colo_manifest, colo_src.path())
        .expect("colocated push");

    // Split push into a separate objects pool.
    let objects = TempDir::new("parity-obj");
    let manifests = TempDir::new("parity-man");
    let split_src = TempDir::new("parity-split-src");
    let (split_manifest, _) = build_tree(split_src.path(), files);
    split_over(objects.path(), manifests.path())
        .push(&split_manifest, split_src.path())
        .expect("split push");

    // Every object blob is byte-identical AND at the same sharded path.
    for (_, content) in files {
        let sum = Blake3Hasher::new().hash_hex(content);
        let colo_blob = fs::read(object_disk(colo.path(), &sum)).expect("colo blob");
        let split_blob = fs::read(object_disk(objects.path(), &sum)).expect("split blob");
        assert_eq!(
            colo_blob, split_blob,
            "object {sum} bytes must match the colocated store"
        );
    }
    assert_eq!(
        count_objects(colo.path()),
        count_objects(objects.path()),
        "the split pool must hold exactly the same object set as colocated"
    );
}

#[test]
fn split_layout_parity_manifest_bytes_match_colocated_filestore() {
    // SPEC invariant (c) — LAYOUT PARITY: the A-side manifest bytes are
    // byte-for-byte identical to the manifest a colocated FileStore push writes.
    let files: &[(&str, &[u8])] = &[("p", b"P\n"), ("q/r", b"QR\n")];

    let colo = TempDir::new("mparity-colo");
    let colo_src = TempDir::new("mparity-colo-src");
    let (colo_manifest, colo_id) = build_tree(colo_src.path(), files);
    FileStore::from_root(colo.path().to_path_buf())
        .push(&colo_manifest, colo_src.path())
        .expect("colocated push");

    let objects = TempDir::new("mparity-obj");
    let manifests = TempDir::new("mparity-man");
    let split_src = TempDir::new("mparity-split-src");
    let (split_manifest, split_id) = build_tree(split_src.path(), files);
    split_over(objects.path(), manifests.path())
        .push(&split_manifest, split_src.path())
        .expect("split push");

    assert_eq!(
        colo_id, split_id,
        "the same tree must yield the same snapshot id"
    );
    let colo_bytes = fs::read(manifest_disk(colo.path(), &colo_id)).expect("colo manifest");
    let split_bytes = fs::read(manifest_disk(manifests.path(), &split_id)).expect("split manifest");
    assert_eq!(
        colo_bytes, split_bytes,
        "the manifest bytes must be byte-for-byte identical to the colocated store"
    );
}

#[test]
fn split_get_manifest_reads_from_manifests_location_and_id_verifies() {
    // SPEC: manifest ops route to the manifests store; get_manifest round-trips
    // and the stored bytes hash back to the id (StreamStore/Store contract).
    let objects = TempDir::new("getm-obj");
    let manifests = TempDir::new("getm-man");
    let src = TempDir::new("getm-src");
    let (manifest, id) = build_tree(src.path(), &[("f", b"F\n")]);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push");

    let got = store.get_manifest(&id).expect("get_manifest");
    assert_eq!(got.to_string(), manifest.to_string());
}

// ===========================================================================
// STREAMSTORE OBJECT-LEVEL ROUTING
// ===========================================================================

#[test]
fn split_stream_put_get_has_object_route_to_objects_pool() {
    // SPEC: has_object/get_object/put_object route to the OBJECTS store.
    let objects = TempDir::new("stream-obj");
    let manifests = TempDir::new("stream-man");
    let store = split_over(objects.path(), manifests.path());

    let bytes = b"streamed payload\n".to_vec();
    let sum = Blake3Hasher::new().hash_hex(&bytes);

    assert!(!store.has_object(&sum).expect("has before"));
    store.put_object(&sum, bytes.clone()).expect("put_object");
    assert!(store.has_object(&sum).expect("has after"));
    assert_eq!(store.get_object(&sum).expect("get_object"), bytes);

    // The blob physically landed in the OBJECTS pool, not the manifests side.
    assert!(object_disk(objects.path(), &sum).is_file());
    assert!(!object_disk(manifests.path(), &sum).exists());
}

#[test]
fn split_objects_needed_probes_only_the_objects_pool() {
    // SPEC: objects_needed routes to the objects store (per-object skip probe).
    let objects = TempDir::new("needed-obj");
    let manifests = TempDir::new("needed-man");
    let store = split_over(objects.path(), manifests.path());

    let present = b"present\n".to_vec();
    let present_sum = Blake3Hasher::new().hash_hex(&present);
    let absent_sum = Blake3Hasher::new().hash_hex(b"absent\n");
    store
        .put_object(&present_sum, present)
        .expect("seed present");

    let needed = store
        .objects_needed(&[present_sum.clone(), absent_sum.clone()])
        .expect("objects_needed");
    assert_eq!(
        needed,
        vec![absent_sum],
        "objects_needed must report exactly the blob absent from the OBJECTS pool, \
         preserving input order"
    );
}

#[test]
fn split_objects_needed_fails_closed_on_invalid_checksum() {
    // SPEC/StreamStore contract: a malformed checksum is a hard error, answered
    // before any probe (fail closed) — the split wrapper must not weaken this.
    let objects = TempDir::new("needfc-obj");
    let manifests = TempDir::new("needfc-man");
    let store = split_over(objects.path(), manifests.path());

    let err = store
        .objects_needed(&["not-hex".to_string()])
        .expect_err("invalid checksum must error");
    assert!(matches!(err, StoreError::Backend { .. }), "got {err:?}");
}

// ===========================================================================
// FAILURE MODES / EDGE CASES
// ===========================================================================

#[test]
fn split_fetch_errors_when_manifest_present_but_object_missing() {
    // SPEC failure mode: a manifest present but a referenced object missing must
    // ERROR — never a silent half-snapshot.
    let objects = TempDir::new("miss-obj");
    let manifests = TempDir::new("miss-man");
    let src = TempDir::new("miss-src");
    let dest = TempDir::new("miss-dest");
    let (manifest, id) = build_tree(src.path(), &[("needed", b"payload\n")]);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push");

    // Delete the object from the pool but KEEP the manifest in its location.
    let sum = Blake3Hasher::new().hash_hex(b"payload\n");
    fs::remove_file(object_disk(objects.path(), &sum)).expect("remove object");
    assert!(manifest_disk(manifests.path(), &id).is_file());

    let err = store
        .fetch_files(&manifest, dest.path())
        .expect_err("fetch with a missing object must ERROR");
    assert!(
        matches!(err, StoreError::ObjectNotFound { .. }),
        "expected ObjectNotFound, got {err:?}"
    );
    // No half-snapshot: the would-be file must NOT exist at dest.
    assert!(
        !dest.path().join("needed").exists(),
        "a failed fetch must not leave a partial materialized file"
    );
}

#[test]
fn split_get_object_rejects_corrupted_blob_on_read() {
    // SPEC failure mode: a corrupted/edited blob fails BLAKE3 verification on
    // read (StreamStore verify-on-read, preserved through the split wrapper).
    let objects = TempDir::new("corrupt-obj");
    let manifests = TempDir::new("corrupt-man");
    let store = split_over(objects.path(), manifests.path());

    let bytes = b"genuine\n".to_vec();
    let sum = Blake3Hasher::new().hash_hex(&bytes);
    store.put_object(&sum, bytes).expect("put_object");

    // Tamper the on-disk blob in the objects pool.
    fs::write(object_disk(objects.path(), &sum), b"TAMPERED\n").expect("corrupt");

    let err = store
        .get_object(&sum)
        .expect_err("corrupt blob must fail verification");
    assert!(matches!(err, StoreError::Integrity { .. }), "got {err:?}");
}

#[test]
fn split_fetch_corrupted_blob_surfaces_integrity_error() {
    // SPEC failure mode: a corrupted blob must not silently materialize through
    // fetch_files — the BLAKE3 check guards the whole-tree read too.
    let objects = TempDir::new("fcorrupt-obj");
    let manifests = TempDir::new("fcorrupt-man");
    let src = TempDir::new("fcorrupt-src");
    let dest = TempDir::new("fcorrupt-dest");
    let (manifest, _id) = build_tree(src.path(), &[("doc", b"the real bytes\n")]);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push");

    let sum = Blake3Hasher::new().hash_hex(b"the real bytes\n");
    fs::write(object_disk(objects.path(), &sum), b"evil\n").expect("corrupt");

    let err = store
        .fetch_files(&manifest, dest.path())
        .expect_err("fetch of a corrupt blob must error");
    assert!(
        matches!(err, StoreError::Integrity { .. } | StoreError::Io(_)),
        "expected Integrity (corruption) error, got {err:?}"
    );
}

#[test]
fn split_fetch_files_into_clashing_destination_does_not_silently_corrupt() {
    // SPEC failure mode: fetch_files into a non-empty / clashing destination.
    // A pre-existing WRONG file at the target path must end up either repaired
    // to the manifest content or surfaced as an error — never left as the stale
    // clashing bytes silently passed off as the snapshot.
    let objects = TempDir::new("clash-obj");
    let manifests = TempDir::new("clash-man");
    let src = TempDir::new("clash-src");
    let dest = TempDir::new("clash-dest");
    let (manifest, _id) = build_tree(src.path(), &[("file", b"correct content\n")]);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push");

    // Pre-populate dest with WRONG content at the clashing path.
    fs::write(dest.path().join("file"), b"STALE WRONG BYTES\n").expect("seed clash");

    let result = store.fetch_files(&manifest, dest.path());
    match result {
        Ok(()) => {
            let got = fs::read(dest.path().join("file")).expect("read materialized");
            assert_eq!(
                got, b"correct content\n",
                "a clashing destination file must be repaired to the manifest content, \
                 never left as stale bytes"
            );
        }
        Err(e) => {
            // Refusing to overwrite is acceptable; silently keeping stale bytes
            // (Ok with wrong content) is the failure this test forbids.
            assert!(
                matches!(
                    e,
                    StoreError::Io(_) | StoreError::Backend { .. } | StoreError::Integrity { .. }
                ),
                "unexpected error kind for clashing dest: {e:?}"
            );
        }
    }
}

#[test]
fn split_empty_tree_pushes_no_objects_and_roundtrips() {
    // SPEC edge case: empty tree. A manifest with only the root directory entry
    // (no files) must push with ZERO objects and fetch into an existing dest.
    let objects = TempDir::new("empty-obj");
    let manifests = TempDir::new("empty-man");
    let src = TempDir::new("empty-src");
    let dest = TempDir::new("empty-dest");
    let (manifest, id) = build_tree(src.path(), &[]);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push empty");

    assert_eq!(
        count_objects(objects.path()),
        0,
        "an empty tree must upload zero objects"
    );
    assert!(
        manifest_disk(manifests.path(), &id).is_file(),
        "the empty-tree manifest must still be written"
    );
    store
        .fetch_files(&manifest, dest.path())
        .expect("fetch empty");
    assert_eq!(
        store.get_manifest(&id).expect("get").to_string(),
        manifest.to_string()
    );
}

#[test]
fn split_duplicate_content_files_dedupe_to_one_object() {
    // SPEC edge case: duplicate-content files dedupe to a single object.
    let objects = TempDir::new("dup-obj");
    let manifests = TempDir::new("dup-man");
    let src = TempDir::new("dup-src");
    let dest = TempDir::new("dup-dest");
    let dup: &[u8] = b"identical payload\n";
    let (manifest, _id) = build_tree(
        src.path(),
        &[("first", dup), ("second", dup), ("dir/third", dup)],
    );

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push");

    assert_eq!(
        count_objects(objects.path()),
        1,
        "three files with identical content must dedupe to exactly ONE object"
    );

    // Round-trip still materializes all three distinct paths byte-identically.
    store.fetch_files(&manifest, dest.path()).expect("fetch");
    for rel in ["first", "second", "dir/third"] {
        assert_eq!(fs::read(dest.path().join(rel)).unwrap(), dup, "{rel}");
    }
}

#[test]
fn split_interrupted_push_leaves_no_manifest_at_manifests_location() {
    // SPEC failure mode: an interrupted push (an object write fails mid-way)
    // leaves NO manifest at the manifests location (objects-before-manifest,
    // all-or-nothing). We force the object copy to fail by deleting the SOURCE
    // file the push must read from, AFTER building the manifest.
    let objects = TempDir::new("intr-obj");
    let manifests = TempDir::new("intr-man");
    let src = TempDir::new("intr-src");
    let (manifest, id) = build_tree(
        src.path(),
        &[("good", b"good bytes\n"), ("doomed", b"doomed bytes\n")],
    );

    // Remove a source file so the object copy fails mid-push.
    fs::remove_file(src.path().join("doomed")).expect("remove source");

    let store = split_over(objects.path(), manifests.path());
    let result = store.push(&manifest, src.path());
    assert!(
        result.is_err(),
        "a push whose source object is missing must fail, not silently succeed"
    );

    // The crux: NO manifest may be observable at the manifests location after a
    // failed (interrupted) push.
    assert!(
        !manifest_disk(manifests.path(), &id).exists(),
        "an interrupted push must leave NO manifest at the manifests location"
    );
}

#[test]
fn split_push_skips_objects_already_in_pool_then_writes_manifest_last() {
    // SPEC: per-object skip probed on objects.has_object; manifest written LAST.
    // Pre-seed ONE of the two objects directly into the pool, then push: the
    // pre-seeded blob is skipped, the other is uploaded, and the manifest lands.
    let objects = TempDir::new("skip-obj");
    let manifests = TempDir::new("skip-man");
    let src = TempDir::new("skip-src");
    let (manifest, id) = build_tree(
        src.path(),
        &[("seeded", b"seed me\n"), ("fresh", b"fresh me\n")],
    );

    let store = split_over(objects.path(), manifests.path());
    let seeded = b"seed me\n".to_vec();
    let seeded_sum = Blake3Hasher::new().hash_hex(&seeded);
    store.put_object(&seeded_sum, seeded).expect("pre-seed");
    assert_eq!(count_objects(objects.path()), 1);

    store.push(&manifest, src.path()).expect("push");

    // Both objects now present, manifest written last.
    assert_eq!(
        count_objects(objects.path()),
        2,
        "fresh object must be uploaded"
    );
    let fresh_sum = Blake3Hasher::new().hash_hex(b"fresh me\n");
    assert!(object_disk(objects.path(), &fresh_sum).is_file());
    assert!(
        manifest_disk(manifests.path(), &id).is_file(),
        "the manifest must be written LAST after objects land"
    );
}

#[test]
fn split_manifest_fast_path_probes_the_manifests_side_not_the_pool() {
    // SPEC: the manifest-present fast-path is probed on the MANIFESTS side. If
    // the manifest already exists at the manifests location, a re-push must be a
    // no-op even when the objects pool is missing the blobs (the fast path must
    // not depend on the pool). Conversely it must NOT be fooled by a manifest of
    // the SAME id sitting only in the objects pool.
    let objects = TempDir::new("fast-obj");
    let manifests = TempDir::new("fast-man");
    let src = TempDir::new("fast-src");
    let (manifest, id) = build_tree(src.path(), &[("only", b"only file\n")]);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("first push");

    // Wipe the objects pool entirely; the manifest still lives on the manifests
    // side, so a second push must short-circuit (skip-if-present) without
    // erroring on the now-empty pool.
    fs::remove_dir_all(objects.path().join(".objects")).ok();
    store
        .push(&manifest, src.path())
        .expect("re-push must be a manifest-side no-op fast path");
    assert_eq!(
        count_objects(objects.path()),
        0,
        "the manifest fast path must not re-touch the objects pool"
    );
    assert!(manifest_disk(manifests.path(), &id).is_file());
}

#[test]
fn split_get_manifest_missing_id_maps_to_manifest_not_found() {
    // SPEC/Store contract: an absent manifest id on the manifests side surfaces
    // ManifestNotFound (routed to the manifests store, not the pool).
    let objects = TempDir::new("nfm-obj");
    let manifests = TempDir::new("nfm-man");
    let store = split_over(objects.path(), manifests.path());

    let missing = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    match store.get_manifest(missing) {
        Err(StoreError::ManifestNotFound { id }) => assert_eq!(id, missing),
        other => panic!("expected ManifestNotFound, got {other:?}"),
    }
}

#[test]
fn split_put_manifest_routes_to_manifests_side_objects_pool_unaffected() {
    // SPEC: put_manifest (StreamStore) routes to the manifests location; it must
    // not write into the objects pool.
    let objects = TempDir::new("pm-obj");
    let manifests = TempDir::new("pm-man");
    let src = TempDir::new("pm-src");
    let (manifest, id) = build_tree(src.path(), &[("g", b"G\n")]);

    let store = split_over(objects.path(), manifests.path());
    store.put_manifest(&id, &manifest).expect("put_manifest");

    assert!(
        manifest_disk(manifests.path(), &id).is_file(),
        "put_manifest must write to the MANIFESTS location"
    );
    assert!(
        !manifest_disk(objects.path(), &id).exists(),
        "put_manifest must NOT write into the objects pool"
    );
}

#[test]
fn split_put_object_rejects_blob_not_matching_its_address() {
    // SPEC/StreamStore contract: put_object verifies BEFORE writing — a blob
    // whose bytes do not hash to `checksum` stores nothing. The split wrapper
    // must preserve this on the objects-pool side.
    let objects = TempDir::new("badput-obj");
    let manifests = TempDir::new("badput-man");
    let store = split_over(objects.path(), manifests.path());

    let wrong_address = Blake3Hasher::new().hash_hex(b"the address bytes\n");
    let err = store
        .put_object(&wrong_address, b"DIFFERENT bytes\n".to_vec())
        .expect_err("mismatched put must error");
    assert!(matches!(err, StoreError::Integrity { .. }), "got {err:?}");
    assert!(
        !object_disk(objects.path(), &wrong_address).exists(),
        "nothing may be stored when the blob fails its address check"
    );
}

// ===========================================================================
// REVIEW-GATE STRENGTHENING (phase 28, split-store-tests-review)
//
// The implementation is now visible (src/split.rs). The black-box suite above
// could only inject a mid-push failure by deleting a SOURCE file (the
// `std::fs::read` arm). These tests reach the OTHER failure arm the impl
// exposes — a pool whose `put_object` itself rejects a blob — plus the precise
// routing/contract clauses the now-visible delegation reveals. They use a
// fault-injecting `StreamStore` double that wraps a real `FileStore` objects
// pool, which only the visible constructor (`SplitStore::new` over any
// `impl StreamStore + Sync + 'static`) makes possible to assemble.
// ===========================================================================

use std::sync::atomic::AtomicUsize;

use snapdir_core::store::Store as _StoreTrait;

/// A `StreamStore` that delegates everything to an inner `FileStore` EXCEPT it
/// makes `put_object` fail (`StoreError::Backend`) for one specific checksum,
/// and counts how many `put_object` calls landed. Lets us inject a mid-push
/// failure in the objects-pool `put_object` itself (the SPEC's "mid-push
/// failure" seam) and assert no manifest lands on the manifests side.
struct FailingPool {
    inner: FileStore,
    fail_checksum: String,
    puts_attempted: AtomicUsize,
    puts_succeeded: AtomicUsize,
}

impl FailingPool {
    fn new(root: &Path, fail_checksum: &str) -> Self {
        Self {
            inner: FileStore::from_root(root.to_path_buf()),
            fail_checksum: fail_checksum.to_string(),
            puts_attempted: AtomicUsize::new(0),
            puts_succeeded: AtomicUsize::new(0),
        }
    }
}

impl _StoreTrait for FailingPool {
    fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError> {
        self.inner.get_manifest(id)
    }
    fn fetch_files(&self, manifest: &Manifest, dest: &Path) -> Result<(), StoreError> {
        self.inner.fetch_files(manifest, dest)
    }
    fn push(&self, manifest: &Manifest, source: &Path) -> Result<(), StoreError> {
        self.inner.push(manifest, source)
    }
}

impl StreamStore for FailingPool {
    fn has_object(&self, checksum: &str) -> Result<bool, StoreError> {
        self.inner.has_object(checksum)
    }
    fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
        self.inner.get_object(checksum)
    }
    fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        self.puts_attempted.fetch_add(1, Ordering::Relaxed);
        if checksum == self.fail_checksum {
            return Err(StoreError::Backend {
                message: format!("injected put_object failure for {checksum}"),
                source: None,
            });
        }
        self.inner.put_object(checksum, bytes)?;
        self.puts_succeeded.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        self.inner.put_manifest(id, manifest)
    }
}

#[test]
fn split_put_object_failure_midpush_leaves_no_manifest_and_aborts() {
    // SPEC mid-push failure (objects-before-manifest + all-or-nothing): when the
    // objects-pool `put_object` itself REJECTS a blob mid-push, the push must
    // error and NO manifest may land at the manifests location. This exercises
    // the `self.objects.put_object(...)?` early-return arm in src/split.rs that
    // the source-deletion test cannot reach.
    let objects = TempDir::new("putfail-obj");
    let manifests = TempDir::new("putfail-man");
    let src = TempDir::new("putfail-src");
    // Two distinct file objects; we poison the second one's address.
    let (manifest, id) = build_tree(
        src.path(),
        &[("ok", b"good content\n"), ("bad", b"poison content\n")],
    );
    let poison_sum = Blake3Hasher::new().hash_hex(b"poison content\n");

    let pool = FailingPool::new(objects.path(), &poison_sum);
    let mani = FileStore::from_root(manifests.path().to_path_buf());
    let store = SplitStore::new(pool, mani);

    let err = store
        .push(&manifest, src.path())
        .expect_err("a put_object failure mid-push must abort the push");
    assert!(matches!(err, StoreError::Backend { .. }), "got {err:?}");

    // The crux: NO manifest may be observable at the manifests location.
    assert!(
        !manifest_disk(manifests.path(), &id).exists(),
        "a push that fails in put_object must leave NO manifest (all-or-nothing)"
    );
    // And get_manifest must still report it absent (not a half-written slot).
    assert!(
        matches!(
            store.get_manifest(&id),
            Err(StoreError::ManifestNotFound { .. })
        ),
        "no snapshot may resolve after an interrupted push"
    );
}

#[test]
fn split_objects_needed_routes_to_pool_not_manifests_and_keeps_duplicates() {
    // SPEC: objects_needed delegates to the OBJECTS pool (not manifests) and
    // preserves the order-preserving / NO-dedup contract. Pins the
    // `self.objects.objects_needed(checksums)` delegation in src/split.rs: an
    // object present ONLY on the manifests side must still be reported needed
    // (proves routing), and an absent checksum supplied TWICE is reported twice.
    let objects = TempDir::new("needroute-obj");
    let manifests = TempDir::new("needroute-man");
    let store = split_over(objects.path(), manifests.path());

    let in_pool = b"lives in pool\n".to_vec();
    let in_pool_sum = Blake3Hasher::new().hash_hex(&in_pool);
    // Seed a blob ONLY into the manifests-side store's object pool. If the split
    // wrongly probed the manifests side, it would (incorrectly) treat this as
    // present.
    let decoy = b"decoy in manifests pool\n".to_vec();
    let decoy_sum = Blake3Hasher::new().hash_hex(&decoy);
    FileStore::from_root(manifests.path().to_path_buf())
        .put_object(&decoy_sum, decoy)
        .expect("seed decoy on manifests side");

    store.put_object(&in_pool_sum, in_pool).expect("seed pool");

    // Order: present-in-pool, decoy (present only on manifests side -> needed),
    // decoy again (duplicate must be reported twice, no dedup).
    let needed = store
        .objects_needed(&[in_pool_sum.clone(), decoy_sum.clone(), decoy_sum.clone()])
        .expect("objects_needed");
    assert_eq!(
        needed,
        vec![decoy_sum.clone(), decoy_sum],
        "objects_needed must probe the OBJECTS pool only (decoy on the manifests \
         side is still needed) and preserve duplicates in input order"
    );
}

#[test]
fn split_fetch_reads_objects_from_pool_regardless_of_which_manifests_side() {
    // SPEC: fetch_files reads object blobs from the OBJECTS pool while the
    // manifest is sourced independently. Push to a SHARED pool under manifests
    // location A, then build a SECOND SplitStore reusing the SAME pool but a
    // FRESH (empty) manifests side, write the manifest there, and fetch: the
    // objects must still materialize from the shared pool. Pins that
    // `fetch_files` delegates to `self.objects` and is decoupled from the
    // manifests routing.
    let objects = TempDir::new("fetchroute-obj");
    let man_a = TempDir::new("fetchroute-man-a");
    let man_b = TempDir::new("fetchroute-man-b");
    let src = TempDir::new("fetchroute-src");
    let dest = TempDir::new("fetchroute-dest");
    let (manifest, id) = build_tree(
        src.path(),
        &[
            ("alpha", b"alpha bytes\n"),
            ("beta/gamma", b"gamma bytes\n"),
        ],
    );

    // First store: objects land in the shared pool; manifest lands at A.
    let store_a = split_over(objects.path(), man_a.path());
    store_a.push(&manifest, src.path()).expect("push to A");

    // Second store: SAME pool, a DIFFERENT empty manifests side. Replicate just
    // the manifest object there (as a store-to-store copy would).
    let store_b = split_over(objects.path(), man_b.path());
    store_b
        .put_manifest(&id, &manifest)
        .expect("replicate manifest to B");

    // B has the manifest and shares the pool, so fetch must succeed entirely
    // from the shared pool's blobs.
    store_b
        .fetch_files(&manifest, dest.path())
        .expect("fetch via B must read objects from the shared pool");
    assert_eq!(
        fs::read(dest.path().join("alpha")).unwrap(),
        b"alpha bytes\n"
    );
    assert_eq!(
        fs::read(dest.path().join("beta/gamma")).unwrap(),
        b"gamma bytes\n"
    );
}

#[test]
fn split_get_manifest_ignores_a_decoy_manifest_in_the_objects_pool() {
    // SPEC routing: get_manifest is answered ONLY by the manifests side. A
    // manifest of the SAME id sitting in the OBJECTS pool's `.manifests` must
    // NOT satisfy get_manifest — the pool is never consulted for manifests.
    // Pins `self.manifests.get_manifest(id)` in src/split.rs against being
    // fooled by a pool-side decoy.
    let objects = TempDir::new("decoym-obj");
    let manifests = TempDir::new("decoym-man");
    let src = TempDir::new("decoym-src");
    let (manifest, id) = build_tree(src.path(), &[("only", b"only bytes\n")]);

    // Plant the manifest ONLY in the objects-pool store, NOT the manifests side.
    FileStore::from_root(objects.path().to_path_buf())
        .put_manifest(&id, &manifest)
        .expect("plant decoy manifest in pool");

    let store = split_over(objects.path(), manifests.path());
    assert!(
        matches!(
            store.get_manifest(&id),
            Err(StoreError::ManifestNotFound { .. })
        ),
        "get_manifest must ignore a manifest present only in the objects pool"
    );

    // And the skip-if-present push fast path must likewise NOT be satisfied by
    // the pool-side decoy: a real push must still write the manifest to the
    // manifests side.
    store.push(&manifest, src.path()).expect("push");
    assert!(
        manifest_disk(manifests.path(), &id).is_file(),
        "push must write the manifest to the manifests side despite the pool decoy"
    );
}

#[test]
fn split_fetch_corrupted_blob_retries_then_surfaces_integrity() {
    // SPEC: fetch_files inherits the objects backend's verify-retry discipline
    // (copy -> BLAKE3-verify -> retry up to N -> error), reading blobs from the
    // POOL. A blob corrupted in the shared pool must NOT silently materialize
    // through the split's fetch delegation; it must surface an Integrity/Io
    // error and leave no good file at dest. Complements the black-box corruption
    // test by pinning that the retry/verify path runs on the POOL side via the
    // split delegation, with the manifest sourced from the manifests side.
    let objects = TempDir::new("fretry-obj");
    let manifests = TempDir::new("fretry-man");
    let src = TempDir::new("fretry-src");
    let dest = TempDir::new("fretry-dest");
    let (manifest, id) = build_tree(src.path(), &[("payload", b"authentic bytes\n")]);

    let store = split_over(objects.path(), manifests.path());
    store.push(&manifest, src.path()).expect("push");
    // Confirm the manifest is genuinely on the manifests side (decoupled source).
    assert!(manifest_disk(manifests.path(), &id).is_file());

    // Corrupt the blob in the POOL after a valid push.
    let sum = Blake3Hasher::new().hash_hex(b"authentic bytes\n");
    fs::write(object_disk(objects.path(), &sum), b"corrupted!\n").expect("corrupt pool blob");

    let err = store
        .fetch_files(&manifest, dest.path())
        .expect_err("a corrupt pool blob must fail fetch, not materialize");
    assert!(
        matches!(err, StoreError::Integrity { .. } | StoreError::Io(_)),
        "expected Integrity/Io from the pool verify-retry, got {err:?}"
    );
    // The corrupted bytes must NEVER be passed off as the snapshot file.
    if let Ok(got) = fs::read(dest.path().join("payload")) {
        assert_ne!(
            got, b"corrupted!\n",
            "corrupted pool bytes must never be materialized at dest"
        );
    }
}
