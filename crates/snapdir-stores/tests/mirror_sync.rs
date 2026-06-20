//! Adversarial integration suite for `sync --delete` — the manifest-set MIRROR
//! (phase 32, gate `mirror-sync-spec-tests`).
//!
//! BLACK-BOX: authored from the gate SPEC + the operator-approved plan ALONE,
//! with NO visibility into any sync-mirror/`--delete` implementation (none
//! exists yet). The ONLY existing code consulted was the public sync /
//! `StreamStore` API (`sync_snapshot`, `SyncReport`, `StreamStore`'s object /
//! manifest / listing primitives) for types and harness patterns. It will NOT
//! compile/pass until the stores-impl teammate moves this file into
//! `crates/snapdir-stores/tests/mirror_sync.rs` and wires the assumed symbols.
//! Do NOT weaken any assertion to make it green — if a behavior here fails
//! against the landed impl, that is a real bug in the impl, not in this test.
//!
//! ---------------------------------------------------------------------------
//! SPEC under test — `sync --delete` = a MANIFEST-SET MIRROR
//! ---------------------------------------------------------------------------
//! Today `sync_snapshot` is ADDITIVE: it copies ONE snapshot (its manifest +
//! every referenced object) from a source store to a dest store, objects-then-
//! manifest (manifest-LAST), skipping objects the dest already has. Phase 32
//! adds `--delete`: AFTER copying the snapshot in, DELETE the destination's
//! manifests that are NOT present in the SOURCE store's manifest set — making
//! the dest manifest set a mirror of the source's.
//!
//! HARD-SCOPED INVARIANTS (the failure modes this suite targets):
//!   1. MANIFEST-SET MIRROR: dest manifests absent from the source set are
//!      DELETED; manifests present in the source set are KEPT; the synced id is
//!      present after.
//!   2. NEVER DELETE AN OBJECT (shared-pool safety): GC is OUT OF SCOPE. No
//!      object is ever deleted — not a shared object a retained manifest still
//!      references, and not even an ORPHAN object referenced only by a pruned
//!      manifest. Only manifests are pruned.
//!   3. COPY-IN BEFORE DELETE: the synced snapshot (objects + manifest) is fully
//!      present after `--delete`; a healthy mirror is byte-identical to a plain
//!      sync followed by the prune (manifest-last preserved, no torn state).
//!   4. UNSUPPORTED DEST = HARD ERROR: `--delete` to a `to` store whose
//!      `supports_mirror()` is false (object/remote: s3/gcs/b2/ssh/sftp/external)
//!      returns a typed, non-panic error and DELETES/CHANGES NOTHING.
//!   5. `--dryrun` + `--delete`: reports what WOULD be pruned, deletes nothing.
//!   6. IDEMPOTENCY / NO-OP: `--delete` when the dest set already equals the
//!      source set performs no deletions; a healthy round-trip is unchanged.
//!   7. NEVER DELETE THE JUST-SYNCED ID, even if some edge would classify it.
//!
//! ---------------------------------------------------------------------------
//! ASSUMED API (the impl may re-point NAMES only; behavior is the contract)
//! ---------------------------------------------------------------------------
//! * Mirror entry point — a NEW variant of `sync_snapshot` that, in addition to
//!   the additive copy-in, prunes dest manifests absent from the source set:
//!
//!       pub fn sync_snapshot_mirror(
//!           from: &(dyn StreamStore + Sync),
//!           to:   &(dyn StreamStore + Sync),
//!           id:   &str,
//!           config: &TransferConfig,
//!           dry_run: bool,
//!           meter: Option<&Meter>,
//!       ) -> Result<MirrorReport, StoreError>;
//!
//!   The impl MAY instead express this as a `delete: bool` (or `mirror: bool`)
//!   parameter added to the existing `sync_snapshot`, and/or fold the pruned-id
//!   accounting into the existing `SyncReport`. If so, fix ONLY the call shape +
//!   the report field names below; NEVER the asserted behavior. `MirrorReport`
//!   is assumed to expose at least: the underlying sync counters (objects copied/
//!   skipped) AND `manifests_pruned: usize` plus the concrete pruned id set
//!   `pruned_ids: Vec<String>` (so `--dryrun` can be asserted exactly). If the
//!   report only exposes a count, drop to count-only assertions but keep them.
//!
//! * `StreamStore::supports_mirror(&self) -> bool` — DEFAULT `false`; overridden
//!   to `true` ONLY by `FileStore` (a local `file://` dest that can delete a
//!   manifest atomically/efficiently). S3/GCS/B2/SSH/external stay `false`.
//!
//! * `StreamStore::delete_manifest(&self, id: &str) -> Result<(), StoreError>` —
//!   deletes the manifest filed under `id` (NO object touched). Deleting an
//!   absent id is assumed idempotent (`Ok(())`), matching the listing/dedup
//!   discipline elsewhere; if the impl makes absent-delete an error instead, that
//!   is a behavior choice the review can pin — this suite does not depend on it.
//!
//! The unsupported-dest refusal is asserted at the STORES layer via
//! `supports_mirror()` returning false (a non-`FileStore` double). Whether the
//! refusal is *also* enforced at the CLI/router layer for a literal
//! `s3://`/`gs://` URI is FLAGGED in the handoff — this file encodes the
//! stores-level contract.

// Mirror the style allowances used by the sibling adversarial suites so every
// assertion stays byte-for-byte as authored under the crate's `-D warnings`.
#![allow(
    clippy::manual_contains,
    clippy::explicit_auto_deref,
    clippy::cloned_ref_to_slice_refs,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};

use snapdir_stores::{FileStore, StreamStore, TransferConfig};

// The mirror entry point + its report. If the impl folds the mirror into
// `sync_snapshot` with a `delete: bool` (and reuses `SyncReport` with added
// `manifests_pruned`/`pruned_ids` fields), re-point these `use`s (and the call
// sites) to that shape ONLY — never the asserted behavior.
use snapdir_stores::{sync_snapshot_mirror, MirrorReport};

// ---------------------------------------------------------------------------
// Test scaffolding (no dev-dependencies; mirrors the existing sync/split tests).
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
            "snapdir-mirror-sync-test-{}-{tag}-{n}",
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

fn cfg() -> TransferConfig {
    TransferConfig::new(4, None)
}

/// Builds a real source tree under `src` and returns the matching `Manifest`
/// plus its snapshot id. Distinct `files` content => distinct snapshot id, so
/// callers can mint as many independent snapshots as they need. Mirrors the
/// `manifest_list.rs` fixture: NON-keyed `Blake3Hasher` addressing + a `D ./`
/// root entry + `snapshot_id` over the sorted manifest.
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

/// Stages a real source tree into a store via the full `Store::push` path
/// (objects + manifest land on disk), returning the `(manifest, id)`.
fn push_tree(store: &FileStore, tag: &str, files: &[(&str, &[u8])]) -> (Manifest, String) {
    let src = TempDir::new(tag);
    let (manifest, id) = build_tree(src.path(), files);
    store.push(&manifest, src.path()).expect("push tree");
    (manifest, id)
}

/// The File-object checksums referenced by a manifest (deduped, sorted).
fn object_checksums(manifest: &Manifest) -> Vec<String> {
    let mut v: Vec<String> = manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .map(|e| e.checksum.clone())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Sorts + dedups a list of ids so order-unspecified results compare as a set.
fn sorted_set(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// Asserts the store's `list_manifest_ids` SET equals `expected` exactly.
fn assert_manifest_set(store: &FileStore, expected: &[String]) {
    let got = sorted_set(store.list_manifest_ids().expect("list_manifest_ids"));
    let want = sorted_set(expected.to_vec());
    assert_eq!(
        got, want,
        "dest manifest set must equal the expected mirror set"
    );
}

/// Absolute on-disk path of a manifest under a FileStore root.
fn manifest_disk_path(root: &Path, id: &str) -> PathBuf {
    root.join(manifest_path(id))
}

/// Absolute on-disk path of an object under a FileStore root.
fn object_disk_path(root: &Path, checksum: &str) -> PathBuf {
    root.join(object_path(checksum))
}

// ===========================================================================
// 1. MANIFEST-SET MIRROR — dest manifests absent from the source are pruned
// ===========================================================================

#[test]
fn mirror_prunes_dest_manifests_absent_from_source_and_keeps_the_synced_id() {
    // SPEC invariant 1 (manifest-set mirror): seed the DEST with extra manifests
    // {A,B} NOT in the source; the SOURCE set is {X,Y}; sync --id X with
    // --delete. Afterwards the dest manifest set == the SOURCE set {X,Y}: A,B
    // pruned, X copied-in, Y... — the mirror prunes dest manifests absent from
    // the source set, so Y (in source, never in dest) is NOT magically created
    // by a single-id sync, but A,B (in dest, absent from source) MUST be gone
    // and X present. So the dest ends as: source ∩ (dest ∪ {X}) with absent-from-
    // source dropped => exactly {X}. We pin the two HARD facts the spec names:
    // (a) A,B are DELETED; (b) the synced id X is PRESENT.
    let src_store_dir = TempDir::new("mirror-src-store");
    let dst_store_dir = TempDir::new("mirror-dst-store");
    let source = FileStore::from_root(src_store_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_store_dir.path().to_path_buf());

    // SOURCE manifest set {X, Y}.
    let (_mx, id_x) = push_tree(&source, "src-x", &[("x", b"snapshot X content\n")]);
    let (_my, id_y) = push_tree(&source, "src-y", &[("y", b"snapshot Y content\n")]);
    assert_ne!(id_x, id_y);

    // DEST seeded with EXTRA manifests {A, B} absent from the source.
    let (_ma, id_a) = push_tree(&dest, "dst-a", &[("a", b"extra A only in dest\n")]);
    let (_mb, id_b) = push_tree(&dest, "dst-b", &[("b", b"extra B only in dest\n")]);
    assert_manifest_set(&dest, &[id_a.clone(), id_b.clone()]);

    // Mirror-sync ONLY id X from source into dest.
    sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror sync ok");

    // (a) A and B (absent from the source set) are DELETED.
    assert!(
        !dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_a),
        "dest manifest A (absent from source) must be pruned"
    );
    assert!(
        !dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_b),
        "dest manifest B (absent from source) must be pruned"
    );
    // (b) The synced id X is PRESENT.
    assert!(
        dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_x),
        "the just-synced id X must be present in the dest"
    );
    dest.get_manifest(&id_x).expect("dest has manifest X");
    // The dest set is now exactly {X} (Y is in source but was never synced; a
    // single-id mirror copies X in and prunes everything absent from source).
    assert_manifest_set(&dest, &[id_x]);
    // Y was never in the dest and a single-id sync does not fabricate it.
    assert!(
        !dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_y),
        "an un-synced source id Y is not created in the dest by a single-id sync"
    );
}

#[test]
fn mirror_keeps_a_dest_manifest_that_is_also_in_the_source_set() {
    // SPEC invariant 1: a dest manifest that IS present in the source set must be
    // KEPT (not pruned). Seed dest with {X, B}; source set {X}; sync X --delete.
    // X is in source -> kept; B absent from source -> pruned. Result == {X}.
    let src_dir = TempDir::new("keep-src");
    let dst_dir = TempDir::new("keep-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    let (mx, id_x) = push_tree(&source, "keep-x", &[("x", b"kept snapshot\n")]);
    // Put the SAME X into the dest already (so it is a dest manifest that is also
    // in the source set), plus an extra B.
    let s = TempDir::new("keep-x-redo");
    let (_m2, id2) = build_tree(s.path(), &[("x", b"kept snapshot\n")]);
    assert_eq!(id2, id_x, "rebuilt X must hash to the same id");
    dest.push(&mx, s.path()).expect("seed dest with X");
    let (_mb, id_b) = push_tree(&dest, "keep-b", &[("b", b"prune me\n")]);
    assert_manifest_set(&dest, &[id_x.clone(), id_b.clone()]);

    sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror ok");

    // X kept (in source set), B pruned (absent from source set).
    assert_manifest_set(&dest, &[id_x.clone()]);
    assert!(
        !dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_b),
        "B absent from source must be pruned"
    );
    dest.get_manifest(&id_x).expect("X retained");
}

// ===========================================================================
// 2. NEVER DELETE AN OBJECT — shared-pool safety + orphan survival
// ===========================================================================

#[test]
fn mirror_never_deletes_a_shared_object_a_retained_manifest_still_references() {
    // SPEC invariant 2 (shared-object safety, the KEYSTONE): a to-be-PRUNED dest
    // manifest references an object that a RETAINED manifest ALSO references.
    // After mirror-sync, that shared object MUST still exist AND still verify —
    // deleting it would corrupt the retained snapshot. GC is out of scope; NO
    // object is ever deleted.
    let src_dir = TempDir::new("shared-src");
    let dst_dir = TempDir::new("shared-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    // The shared blob, referenced by BOTH a pruned and a retained dest manifest.
    let shared_bytes: &[u8] = b"shared-object-bytes-referenced-by-two-snapshots\n";
    let shared_sum = Blake3Hasher::new().hash_hex(shared_bytes);

    // Source set = {X} where X references the shared blob (so X is retained and
    // its object must survive).
    let (_mx, id_x) = push_tree(
        &source,
        "shared-x",
        &[("shared", shared_bytes), ("xonly", b"x distinct\n")],
    );

    // Dest seeded with PRUNED manifest P that ALSO references the shared blob,
    // plus the eventual X (we'll sync X in). P is absent from the source -> it
    // will be pruned, but the shared object it shares with X must survive.
    let (_mp, id_p) = push_tree(
        &dest,
        "shared-p",
        &[("shared", shared_bytes), ("ponly", b"p distinct\n")],
    );
    assert!(
        dest.has_object(&shared_sum).expect("has shared"),
        "precondition: dest holds the shared object via P"
    );

    sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror ok");

    // P pruned, X present.
    assert_manifest_set(&dest, &[id_x.clone()]);
    assert!(
        !dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_p),
        "P (absent from source) is pruned"
    );
    // The SHARED object MUST still exist AND still verify (no object deletion).
    assert!(
        dest.has_object(&shared_sum)
            .expect("has_object after mirror"),
        "the shared object referenced by retained X must NOT be deleted"
    );
    assert!(
        object_disk_path(dst_dir.path(), &shared_sum).exists(),
        "the shared object file must still be on disk"
    );
    let blob = dest
        .get_object(&shared_sum)
        .expect("shared object must still read + BLAKE3-verify after mirror");
    assert_eq!(blob, shared_bytes, "shared object bytes intact");
}

#[test]
fn mirror_does_not_delete_an_orphan_object_referenced_only_by_a_pruned_manifest() {
    // SPEC invariant 2 (GC out of scope, orphan survival): an object referenced
    // ONLY by a PRUNED dest manifest becomes an ORPHAN after the prune — and it
    // MUST STILL EXIST afterward. Reclaiming orphans is a FUTURE `snapdir gc`
    // job, never `sync --delete`. Pin: the orphan object is NOT deleted.
    let src_dir = TempDir::new("orphan-src");
    let dst_dir = TempDir::new("orphan-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    // Source set = {X} (its own objects, NOT shared with the orphan).
    let (_mx, id_x) = push_tree(&source, "orphan-x", &[("x", b"x snapshot only\n")]);

    // Dest seeded with a pruned manifest P whose object is UNIQUE to P (an
    // orphan-to-be). After P is pruned, this object is referenced by nothing.
    let orphan_bytes: &[u8] = b"object referenced only by the pruned manifest P\n";
    let orphan_sum = Blake3Hasher::new().hash_hex(orphan_bytes);
    let (_mp, id_p) = push_tree(&dest, "orphan-p", &[("orphan", orphan_bytes)]);
    assert!(
        dest.has_object(&orphan_sum)
            .expect("has orphan precondition"),
        "precondition: dest holds the orphan object via P"
    );

    sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror ok");

    // P pruned.
    assert!(
        !dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_p),
        "P pruned"
    );
    // The now-orphan object MUST still exist (GC out of scope).
    assert!(
        dest.has_object(&orphan_sum)
            .expect("has_object after mirror"),
        "an orphan object (only referenced by a pruned manifest) must NOT be deleted"
    );
    assert!(
        object_disk_path(dst_dir.path(), &orphan_sum).exists(),
        "the orphan object file must still be on disk after the mirror prune"
    );
    // STRONGER (review): the now-orphan object must still READ + BLAKE3-verify
    // through the store API (a corrupted-but-present file would still satisfy the
    // on-disk/has_object checks; get_object proves the bytes are intact).
    let orphan_blob = dest
        .get_object(&orphan_sum)
        .expect("orphan object must still read + BLAKE3-verify after the prune");
    assert_eq!(
        orphan_blob, orphan_bytes,
        "the orphan object bytes must be intact after the prune"
    );
    let _ = id_x;
}

#[test]
fn mirror_deletes_no_object_at_all_object_pool_byte_identical() {
    // SPEC invariant 2 (strongest form): across a mirror that PRUNES several dest
    // manifests, the dest's `.objects/` pool is BYTE-IDENTICAL before vs after
    // EXCEPT for objects newly copied-in by the synced snapshot. Nothing is ever
    // removed from the object pool. We snapshot the full set of object files on
    // disk before and after and assert before ⊆ after (no deletions; only
    // additions from the copy-in).
    let src_dir = TempDir::new("pool-src");
    let dst_dir = TempDir::new("pool-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    let (_mx, id_x) = push_tree(&source, "pool-x", &[("x", b"X new content\n")]);

    // Several extra dest manifests, each with unique objects -> all become
    // orphans after prune; none may be deleted.
    let (_p1, _) = push_tree(&dest, "pool-p1", &[("p1", b"p1 unique\n")]);
    let (_p2, _) = push_tree(&dest, "pool-p2", &[("p2", b"p2 unique\n")]);
    let (_p3, _) = push_tree(&dest, "pool-p3", &[("p3", b"p3 unique\n")]);

    let before = object_files_on_disk(dst_dir.path());
    assert!(!before.is_empty(), "precondition: dest has objects");

    sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror ok");

    let after = object_files_on_disk(dst_dir.path());
    // EVERY object present before MUST still be present after (no deletion).
    for o in &before {
        assert!(
            after.contains(o),
            "object file {o:?} was deleted by the mirror prune — objects must NEVER be deleted"
        );
    }
    // STRONGER (review): `before` is a strict SUBSET of `after` and the pool only
    // ever GREW — a single new object (X's `x` blob) was copied in, the three
    // orphaned p1/p2/p3 blobs survived. So `after.len() == before.len() + 1` and
    // nothing was removed. This rules out a "delete one, copy one" wash that the
    // subset check alone would miss.
    assert!(
        before.is_subset(&after),
        "the whole .objects/ pool before must be a subset of after (no object removed)"
    );
    assert_eq!(
        after.len(),
        before.len() + 1,
        "the pool must grow by exactly the one new copied-in object and lose none"
    );
}

/// Collects the set of object FILE leaf paths (relative to root) under
/// `.objects/`, so before/after pool comparisons can detect any deletion.
fn object_files_on_disk(root: &Path) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    let objects = root.join(".objects");
    fn walk(dir: &Path, base: &Path, out: &mut HashSet<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                out.insert(rel.to_path_buf());
            }
        }
    }
    walk(&objects, root, &mut out);
    out
}

// ===========================================================================
// 3. COPY-IN BEFORE DELETE — synced snapshot fully present; byte-identical
// ===========================================================================

#[test]
fn mirror_copies_the_snapshot_in_before_pruning() {
    // SPEC invariant 3 (ordering: copy-in BEFORE delete): after a mirror-sync the
    // synced snapshot's manifest AND every referenced object are present in the
    // dest (the copy-in fully happened), AND the prune happened (extra manifest
    // gone) — proving copy-in completed before/independent of the prune.
    let src_dir = TempDir::new("order-src");
    let dst_dir = TempDir::new("order-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    let (mx, id_x) = push_tree(
        &source,
        "order-x",
        &[("a", b"alpha\n"), ("b", b"bravo\n"), ("c", b"charlie\n")],
    );
    let (_mp, id_p) = push_tree(&dest, "order-p", &[("p", b"prune target\n")]);

    sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror ok");

    // Copy-in: manifest X + every object present.
    dest.get_manifest(&id_x).expect("dest has X manifest");
    for sum in object_checksums(&mx) {
        assert!(
            dest.has_object(&sum).expect("has_object"),
            "object {sum} of the synced snapshot must be present (copy-in before delete)"
        );
        // And it must verify (manifest-last invariant: a present manifest implies
        // present, valid objects).
        dest.get_object(&sum).expect("synced object verifies");
    }
    // Prune: P gone.
    assert!(
        !dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_p),
        "P pruned after copy-in"
    );
}

#[test]
fn mirror_into_a_set_equal_dest_is_byte_identical_to_plain_sync() {
    // SPEC invariant 3 (byte-identical): a healthy mirror where the dest set
    // already == the source set ∪ {X} produces a dest byte-identical to a plain
    // (additive) `sync_snapshot` over the same inputs — same manifest bytes, same
    // object bytes, manifest-last preserved, no torn state. Compare two dests:
    // one built by plain sync, one by mirror-sync, when there's nothing to prune.
    let src_dir = TempDir::new("bi-src");
    let plain_dir = TempDir::new("bi-plain");
    let mirror_dir = TempDir::new("bi-mirror");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let plain = FileStore::from_root(plain_dir.path().to_path_buf());
    let mirrored = FileStore::from_root(mirror_dir.path().to_path_buf());

    let (mx, id_x) = push_tree(
        &source,
        "bi-x",
        &[("a", b"aaa\n"), ("b", b"bbb\n"), ("c", b"ccc\n")],
    );

    // Plain additive sync into an empty dest.
    snapdir_stores::sync_snapshot(&source, &plain, &id_x, &cfg(), false, None)
        .expect("plain sync ok");
    // Mirror sync into another empty dest (nothing to prune -> additive only).
    sync_snapshot_mirror(&source, &mirrored, &id_x, &cfg(), false, None).expect("mirror sync ok");

    // Manifest bytes identical (both stores file X; raw on-disk bytes match).
    let p_man = fs::read(manifest_disk_path(plain_dir.path(), &id_x)).expect("plain manifest");
    let m_man = fs::read(manifest_disk_path(mirror_dir.path(), &id_x)).expect("mirror manifest");
    assert_eq!(
        p_man, m_man,
        "mirror dest manifest bytes must equal plain sync"
    );
    // Every object byte-identical.
    for sum in object_checksums(&mx) {
        let p = fs::read(object_disk_path(plain_dir.path(), &sum)).expect("plain object");
        let m = fs::read(object_disk_path(mirror_dir.path(), &sum)).expect("mirror object");
        assert_eq!(p, m, "object {sum} bytes must match plain sync");
    }
    // Same manifest set.
    assert_manifest_set(&mirrored, &[id_x]);
}

// ===========================================================================
// 4. UNSUPPORTED DEST = HARD ERROR (object/remote store), changes NOTHING
// ===========================================================================

/// A `StreamStore` double whose `supports_mirror()` is FALSE, standing in for an
/// object/remote backend (S3/GCS/B2/SSH/external) that cannot atomically/
/// efficiently delete a manifest. It wraps a real `FileStore` so the copy-in
/// part *would* succeed — the point is that `--delete` must HARD-ERROR on it
/// BEFORE deleting anything, because pruning is unsupported here. It records
/// every mutating op so we can assert NOTHING was deleted.
struct RemoteLikeDest {
    inner: FileStore,
    deletes_attempted: Mutex<Vec<String>>,
}

impl RemoteLikeDest {
    fn new(root: PathBuf) -> Self {
        Self {
            inner: FileStore::from_root(root),
            deletes_attempted: Mutex::new(Vec::new()),
        }
    }
}

impl Store for RemoteLikeDest {
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

impl StreamStore for RemoteLikeDest {
    fn has_object(&self, checksum: &str) -> Result<bool, StoreError> {
        self.inner.has_object(checksum)
    }
    fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
        self.inner.get_object(checksum)
    }
    fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        self.inner.put_object(checksum, bytes)
    }
    fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        self.inner.put_manifest(id, manifest)
    }
    fn list_manifest_ids(&self) -> Result<Vec<String>, StoreError> {
        self.inner.list_manifest_ids()
    }
    // The ASSUMED capability gate: a remote/object backend cannot mirror-prune.
    fn supports_mirror(&self) -> bool {
        false
    }
    // If `--delete` ever reached here it would be a bug (the capability gate must
    // refuse first). Record the attempt so the test can prove it never happened.
    fn delete_manifest(&self, id: &str) -> Result<(), StoreError> {
        self.deletes_attempted.lock().unwrap().push(id.to_owned());
        self.inner.delete_manifest(id)
    }
}

#[test]
fn mirror_to_unsupported_dest_is_a_hard_error_and_deletes_nothing() {
    // SPEC invariant 4 (unsupported dest = hard error): `--delete` to a `to`
    // store whose `supports_mirror()` is false (object/remote) returns a typed,
    // NON-PANIC error and DELETES/CHANGES NOTHING. The capability gate must fire
    // BEFORE any prune (and arguably before/without the copy-in mutating the
    // dest). We assert: (a) Err, not a panic; (b) NO delete_manifest was ever
    // attempted; (c) the pre-existing extra manifest is still present.
    let src_dir = TempDir::new("unsup-src");
    let dst_dir = TempDir::new("unsup-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = RemoteLikeDest::new(dst_dir.path().to_path_buf());

    let (mx, id_x) = push_tree(&source, "unsup-x", &[("x", b"x to sync\n")]);
    // A pre-existing extra manifest in the (file-backed) remote-like dest.
    let (_mp, id_p) = push_tree(&dest.inner, "unsup-p", &[("p", b"must survive\n")]);

    // Snapshot the FULL dest state (manifest set + object pool) before the refused
    // mirror, so we can prove the refusal changed NOTHING — not just that no delete
    // was attempted, but that no copy-in mutated the dest either.
    let manifests_before = sorted_set(dest.inner.list_manifest_ids().unwrap());
    let objects_before = object_files_on_disk(dst_dir.path());

    let err = sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None)
        .expect_err("mirror to an unsupported (non-mirror) dest must be a hard error");

    // A typed StoreError (not a panic). We don't over-constrain the exact
    // variant, but it must NOT be a silent success path. A Backend("unsupported"
    // / "mirror") message is the natural shape.
    match &err {
        StoreError::Backend { message, .. } => {
            let m = message.to_lowercase();
            assert!(
                m.contains("mirror")
                    || m.contains("unsupported")
                    || m.contains("delete")
                    || m.contains("not support"),
                "the refusal message should explain mirror/delete is unsupported here; got: {message}"
            );
        }
        // Tolerate the impl choosing a future dedicated variant; the only hard
        // requirement is that it is an Err and NOTHING was deleted.
        other => {
            let _ = other;
        }
    }

    // NOTHING was deleted: delete_manifest must never have been called.
    assert!(
        dest.deletes_attempted.lock().unwrap().is_empty(),
        "no manifest may be deleted on an unsupported dest: {:?}",
        dest.deletes_attempted.lock().unwrap()
    );
    // The pre-existing extra manifest survives.
    assert!(
        dest.inner
            .list_manifest_ids()
            .unwrap()
            .iter()
            .any(|i| *i == id_p),
        "the pre-existing dest manifest P must survive a refused mirror"
    );
    // STRONGER (review): the refusal must change NOTHING. The capability gate fires
    // BEFORE the copy-in, so the dest's manifest SET and the entire `.objects/`
    // pool are byte-identical to before — no copy-in, no delete, no torn state.
    assert_eq!(
        manifests_before,
        sorted_set(dest.inner.list_manifest_ids().unwrap()),
        "a refused mirror must leave the dest manifest set byte-identical"
    );
    assert_eq!(
        objects_before,
        object_files_on_disk(dst_dir.path()),
        "a refused mirror must leave the dest object pool byte-identical (no copy-in)"
    );
    // X's manifest was NOT copied in (the refusal preceded any copy).
    assert!(
        dest.inner.get_manifest(&id_x).is_err(),
        "a refused mirror must NOT have copied the synced manifest in"
    );
    // None of X's objects were copied in either.
    for sum in object_checksums(&mx) {
        assert!(
            !dest.inner.has_object(&sum).expect("has_object"),
            "a refused mirror must NOT have copied any object of X in"
        );
    }
}

#[test]
fn filestore_supports_mirror_is_true() {
    // SPEC invariant 4 (capability): a local `FileStore` dest DOES support the
    // mirror (it can delete a manifest locally), so its `supports_mirror()` is
    // true. This pins the FileStore side of the capability gate the refusal test
    // relies on.
    let dir = TempDir::new("cap-true");
    let store = FileStore::from_root(dir.path().to_path_buf());
    assert!(
        store.supports_mirror(),
        "FileStore (local file:// dest) must report supports_mirror() == true"
    );
}

// ===========================================================================
// 5. --dryrun + --delete — reports what WOULD be pruned, deletes nothing
// ===========================================================================

#[test]
fn mirror_dry_run_reports_pruned_set_but_deletes_nothing() {
    // SPEC invariant 5 (--dryrun + --delete): a dry-run mirror reports what WOULD
    // be pruned and copied, but writes/deletes NOTHING. Seed dest with extras
    // {A,B}; dry-run mirror id X. After: A,B still present, X NOT copied in, no
    // object written. The report (if it exposes the set) names A,B as the prune
    // candidates.
    let src_dir = TempDir::new("dry-src");
    let dst_dir = TempDir::new("dry-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    let (mx, id_x) = push_tree(&source, "dry-x", &[("x", b"x dryrun\n")]);
    let (_ma, id_a) = push_tree(&dest, "dry-a", &[("a", b"A extra\n")]);
    let (_mb, id_b) = push_tree(&dest, "dry-b", &[("b", b"B extra\n")]);
    let before = sorted_set(dest.list_manifest_ids().unwrap());
    // Also snapshot the object pool: a dry run must leave it byte-identical too.
    let objects_before = object_files_on_disk(dst_dir.path());

    let report =
        sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), true, None).expect("dry-run mirror ok");

    // NOTHING deleted: A and B still present.
    assert!(
        dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_a),
        "dry-run must not delete A"
    );
    assert!(
        dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_b),
        "dry-run must not delete B"
    );
    // The dest manifest set is UNCHANGED (X not copied in, nothing pruned).
    assert_eq!(
        before,
        sorted_set(dest.list_manifest_ids().unwrap()),
        "dry-run must leave the dest manifest set unchanged"
    );
    // No object of X written.
    for sum in object_checksums(&mx) {
        assert!(
            !dest.has_object(&sum).expect("has_object"),
            "dry-run must not copy in any object"
        );
    }
    // X manifest not written.
    assert!(
        dest.get_manifest(&id_x).is_err(),
        "dry-run must not write the synced manifest"
    );
    // STRONGER (review): the entire object pool is byte-identical — a dry run
    // neither copies a new object in nor (ever) removes one.
    assert_eq!(
        objects_before,
        object_files_on_disk(dst_dir.path()),
        "dry-run must leave the dest object pool byte-identical"
    );

    // The report names what WOULD be pruned. If the impl exposes `pruned_ids`,
    // it must list exactly {A,B}; if only a count, it must be 2. We assert via a
    // helper that tolerates either shape.
    assert_pruned_report(&report, &[id_a, id_b]);
}

/// Asserts a mirror report's "would prune / did prune" set. Written against the
/// ASSUMED `MirrorReport { manifests_pruned: usize, pruned_ids: Vec<String>, .. }`
/// (alongside the underlying sync counters). If the landed report exposes only a
/// count (no `pruned_ids`), the impl teammate should reduce the second assertion
/// to the count check (NEVER drop it). `expected` is the exact id set.
fn assert_pruned_report(report: &MirrorReport, expected: &[String]) {
    let want = sorted_set(expected.to_vec());
    assert_eq!(
        report.manifests_pruned,
        want.len(),
        "pruned count must equal the number of dest manifests absent from source"
    );
    let got = sorted_set(report.pruned_ids.clone());
    assert_eq!(
        got, want,
        "pruned id set must be exactly the absent-from-source manifests"
    );
}

// ===========================================================================
// 6. IDEMPOTENCY / NO-OP — dest already equals source set
// ===========================================================================

#[test]
fn mirror_is_a_no_op_when_dest_set_already_equals_source_set() {
    // SPEC invariant 6 (idempotency / no-op): mirror-syncing into a dest whose
    // manifest set already == the source set (just {X}) performs NO deletions and
    // a healthy round-trip is unchanged. Run mirror once to converge, then again
    // and assert the second run prunes nothing and the object pool is unchanged.
    let src_dir = TempDir::new("noop-src");
    let dst_dir = TempDir::new("noop-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    let (mx, id_x) = push_tree(&source, "noop-x", &[("x", b"x noop\n")]);

    // First mirror: empty dest -> just copies X in (nothing to prune).
    sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("first mirror");
    assert_manifest_set(&dest, &[id_x.clone()]);
    let pool_after_first = object_files_on_disk(dst_dir.path());

    // Second mirror over the converged dest: NO-OP. Nothing pruned, set + pool
    // unchanged.
    let report =
        sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("second mirror");
    assert_manifest_set(&dest, &[id_x.clone()]);
    assert_eq!(
        pool_after_first,
        object_files_on_disk(dst_dir.path()),
        "a converged second mirror must not change the object pool"
    );
    // If the report exposes a pruned count, it must be 0 on the no-op run.
    assert_eq!(
        report.manifests_pruned, 0,
        "a converged mirror prunes nothing"
    );
    // X still verifies end-to-end.
    for sum in object_checksums(&mx) {
        dest.get_object(&sum).expect("object still verifies");
    }
}

// ===========================================================================
// 7. NEVER DELETE THE JUST-SYNCED ID
// ===========================================================================

#[test]
fn mirror_never_deletes_the_just_synced_id_even_when_it_is_new_to_the_dest() {
    // SPEC invariant 7: `--delete` must NEVER delete the just-synced id, even
    // though X did not exist in the dest before this run (an edge that a naive
    // "delete everything absent from <dest's prior set>" could misclassify). X is
    // copied in AND retained; only the unrelated extra is pruned.
    let src_dir = TempDir::new("self-src");
    let dst_dir = TempDir::new("self-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    let (_mx, id_x) = push_tree(&source, "self-x", &[("x", b"the synced id\n")]);
    let (_mq, id_q) = push_tree(&dest, "self-q", &[("q", b"unrelated extra\n")]);
    // Precondition: X is NOT yet in the dest.
    assert!(
        dest.get_manifest(&id_x).is_err(),
        "precondition: X absent from dest before sync"
    );

    sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror ok");

    // X present (never deleted), Q pruned.
    assert!(
        dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_x),
        "the just-synced id X must never be deleted"
    );
    dest.get_manifest(&id_x).expect("X readable after mirror");
    assert!(
        !dest.list_manifest_ids().unwrap().iter().any(|i| *i == id_q),
        "the unrelated extra Q is pruned"
    );
    assert_manifest_set(&dest, &[id_x]);
}

#[test]
fn mirror_with_empty_dest_only_copies_in_prunes_nothing() {
    // SPEC invariant 1 + 7 edge: an EMPTY dest has nothing to prune — the mirror
    // is purely additive (copy X in), prunes zero manifests, and the synced id is
    // present. Pins that "prune set absent from source" is empty when the dest
    // starts empty (no spurious deletion of the just-copied X).
    let src_dir = TempDir::new("empty-src");
    let dst_dir = TempDir::new("empty-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    let (_mx, id_x) = push_tree(&source, "empty-x", &[("x", b"only snapshot\n")]);
    assert!(
        dest.list_manifest_ids().unwrap().is_empty(),
        "precondition: empty dest"
    );

    let report =
        sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror ok");

    assert_manifest_set(&dest, &[id_x.clone()]);
    assert_eq!(report.manifests_pruned, 0, "empty dest prunes nothing");
    dest.get_manifest(&id_x).expect("X present");
}

// ===========================================================================
// 8. IMPL-REVEALED cases (review gate, implementation now visible)
// ===========================================================================

#[test]
fn mirror_prunes_every_one_of_many_extraneous_manifests_and_report_matches_exactly() {
    // SPEC invariant 1 + report contract (review-added): a dest seeded with MANY
    // extraneous manifests {A,B,C,D} (none in the source set) must have ALL of
    // them pruned by a single mirror, and `MirrorReport.pruned_ids` must be
    // EXACTLY that set (+ `manifests_pruned == 4`). Pins that the prune loop does
    // not stop early and that the report's id set is precise, not approximate.
    let src_dir = TempDir::new("many-src");
    let dst_dir = TempDir::new("many-dst");
    let source = FileStore::from_root(src_dir.path().to_path_buf());
    let dest = FileStore::from_root(dst_dir.path().to_path_buf());

    let (_mx, id_x) = push_tree(&source, "many-x", &[("x", b"the synced one\n")]);

    let (_ma, id_a) = push_tree(&dest, "many-a", &[("a", b"extra A\n")]);
    let (_mb, id_b) = push_tree(&dest, "many-b", &[("b", b"extra B\n")]);
    let (_mc, id_c) = push_tree(&dest, "many-c", &[("c", b"extra C\n")]);
    let (_md, id_d) = push_tree(&dest, "many-d", &[("d", b"extra D\n")]);
    assert_manifest_set(
        &dest,
        &[id_a.clone(), id_b.clone(), id_c.clone(), id_d.clone()],
    );

    let report =
        sync_snapshot_mirror(&source, &dest, &id_x, &cfg(), false, None).expect("mirror ok");

    // ALL four extraneous manifests pruned; only X remains.
    assert_manifest_set(&dest, &[id_x.clone()]);
    // The report's pruned id set is EXACTLY {A,B,C,D} (the just-synced X excluded).
    assert_eq!(report.manifests_pruned, 4, "all four extras pruned");
    assert_eq!(
        sorted_set(report.pruned_ids.clone()),
        sorted_set(vec![id_a, id_b, id_c, id_d]),
        "pruned_ids must be exactly the four absent-from-source manifests"
    );
    assert!(
        !report.pruned_ids.iter().any(|i| *i == id_x),
        "the just-synced id must never appear in pruned_ids"
    );
}

#[test]
fn filestore_delete_manifest_of_absent_id_is_idempotent() {
    // delete_manifest contract (review-added, impl-revealed): deleting a manifest
    // id that is NOT present is idempotent — returns Ok(()), matching the
    // listing/dedup discipline elsewhere. This underpins the mirror's prune loop
    // tolerating concurrent/duplicate deletes without erroring. Also confirm it
    // leaves an unrelated existing manifest + its object untouched.
    let dir = TempDir::new("idem-del");
    let store = FileStore::from_root(dir.path().to_path_buf());

    let (mx, id_x) = push_tree(&store, "idem-x", &[("x", b"keep me\n")]);
    let x_obj = object_checksums(&mx);

    // Deleting a never-existed id: Ok, no panic, nothing changed.
    store
        .delete_manifest("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
        .expect("deleting an absent manifest id must be idempotent (Ok)");
    assert_manifest_set(&store, &[id_x.clone()]);

    // Delete X, then delete X AGAIN — the second delete is still Ok (idempotent).
    store.delete_manifest(&id_x).expect("first delete ok");
    assert!(
        store.get_manifest(&id_x).is_err(),
        "X manifest gone after delete"
    );
    store
        .delete_manifest(&id_x)
        .expect("re-deleting the now-absent id must be idempotent (Ok)");

    // delete_manifest NEVER touches objects: X's object still exists on disk and
    // reads back even though its manifest is gone (it is now an orphan).
    for sum in &x_obj {
        assert!(
            object_disk_path(dir.path(), sum).exists(),
            "delete_manifest must never remove an object file"
        );
        assert!(
            store.has_object(sum).expect("has_object"),
            "the object of a deleted manifest must survive (no object GC)"
        );
    }
}

#[test]
fn default_streamstore_delete_manifest_and_supports_mirror_refuse() {
    // SPEC invariant 4 (capability default, impl-revealed): a StreamStore that does
    // NOT override the mirror capability inherits supports_mirror() == false and a
    // delete_manifest() that HARD-ERRORS (typed StoreError, no silent success).
    // Our RemoteLikeDest forwards delete to its FileStore inner, so build a pure
    // default-impl double that overrides NOTHING to pin the trait defaults.
    let dir = TempDir::new("default-cap");
    let inner = FileStore::from_root(dir.path().to_path_buf());

    struct DefaultsOnly {
        inner: FileStore,
    }
    impl Store for DefaultsOnly {
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
    impl StreamStore for DefaultsOnly {
        fn has_object(&self, checksum: &str) -> Result<bool, StoreError> {
            self.inner.has_object(checksum)
        }
        fn get_object(&self, checksum: &str) -> Result<Vec<u8>, StoreError> {
            self.inner.get_object(checksum)
        }
        fn put_object(&self, checksum: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
            self.inner.put_object(checksum, bytes)
        }
        fn put_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
            self.inner.put_manifest(id, manifest)
        }
        fn list_manifest_ids(&self) -> Result<Vec<String>, StoreError> {
            self.inner.list_manifest_ids()
        }
        // supports_mirror + delete_manifest deliberately NOT overridden -> defaults.
    }

    let store = DefaultsOnly { inner };
    // Default supports_mirror() is false (only FileStore opts in).
    assert!(
        !store.supports_mirror(),
        "the default StreamStore must NOT support mirroring"
    );
    // Default delete_manifest() hard-errors (typed, not a panic, not a silent Ok).
    let err = store
        .delete_manifest("anything")
        .expect_err("the default delete_manifest must hard-error, not silently succeed");
    match &err {
        StoreError::Backend { message, .. } => {
            let m = message.to_lowercase();
            assert!(
                m.contains("delete") || m.contains("mirror") || m.contains("unsupported"),
                "the default refusal should explain delete/mirror is unsupported; got: {message}"
            );
        }
        other => panic!("default delete_manifest should be a Backend error, got {other:?}"),
    }
}
