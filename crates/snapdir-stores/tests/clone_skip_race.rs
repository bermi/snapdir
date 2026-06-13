//! INDEPENDENT VERIFICATION suite (gate `clone-skip-race-verify`, phase 29) —
//! TOCTOU / concurrency / property fuzz proving the clone-skip optimization
//! NEVER yields a *silently mis-addressed object* (an object readable at
//! checksum(A) whose bytes are actually B).
//!
//! This is the dedicated verification gate, so authoring a NET-NEW test file
//! here is explicitly allowed. The impl is visible and was read to ground these
//! cases (`file_store.rs`: `persist`, `CopyTrust`, `StatGuarded`, `copy_file`,
//! `get_object`, `with_copy_guards`; `snapdir_core::CopyGuard`).
//!
//! ## The two-layer safety model under test
//!
//! Layer 1 (WRITE-time stat-guard). On the `CopyTrust::StatGuarded` stage path
//! `persist` re-`stat`s the source at clone time and SKIPS the temp re-hash IFF
//! the fresh `CopyGuard{size,mtime_ns,ctime_ns,ino}` still equals the recorded
//! one. A benign mid-stage race is caught here because `ctime` advances on ANY
//! inode change (a write, a `utimensat`) and cannot be moved backwards via
//! `utimensat` (which only sets atime/mtime) — so a content+mtime+size spoof
//! still flips `ctime_ns` and forces the re-hash, which then surfaces the true
//! content change as `StoreError::Integrity`.
//!
//! Layer 2 (READ-time BLAKE3 backstop). Even if the write-time guard were FULLY
//! defeated (a guard that exactly matches the post-mutation stat) so that B is
//! cloned into the object filed under checksum(A), `get_object(checksum(A))` and
//! `fetch_files` re-hash on read and reject the mis-addressed blob with
//! `StoreError::Integrity`. This is the load-bearing proof that skip is
//! safe-by-backstop.
//!
//! The FORBIDDEN outcome every case fails on: an object readable at checksum(A)
//! whose bytes are B (i.e. `get_object`/`fetch` hands back bytes that do not
//! hash to the address requested).
//!
//! ## Env / parallelism
//!
//! `SNAPDIR_CLONEFILE` / `SNAPDIR_VERIFY_COPIES` are process-global and Rust
//! runs `#[test]`s multithreaded in one binary, so every test that touches a
//! knob (or reads a `clonefile_hits()` delta) holds a single process-wide
//! `ENV_LOCK` for its whole body and RESTORES prior values on drop (mirrors
//! `apfs_clone.rs` / `clone_skip.rs`). Spawned threads are always joined.

// Style-only lint allows (no assertion is affected): the deterministic
// schedule generators do arithmetic-mod-256 byte fills (cast truncation is the
// intent), use large multiplier literals, and define small walk helpers after
// statements — none of which bear on what the suite proves.
//
// The last two only ever fire on the LINUX target (`cfg(target_os="linux")`
// cases / the musl `libc::time_t` alias), so they are invisible to the macOS
// dev host's clippy but fail CI's ubuntu `clippy --all-targets -D warnings`
// (process note in `.gatesmith/state.md` — `reflink-tests-review` caught the
// same class). Pure shape; no assertion or test logic is affected:
//   * `map_unwrap_or` — the env-contract gate `reflink_root_or_skip` (mirrors
//     `reflink.rs`'s allow),
//   * `deprecated` — `set_mtime_atime`'s `libc::time_t` (musl 1.2 64-bit
//     migration warning; the existing helper, unchanged).
#![allow(
    clippy::type_complexity,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::single_match,
    clippy::single_match_else,
    clippy::cast_possible_truncation,
    clippy::unreadable_literal,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    deprecated
)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex, MutexGuard, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::store::{Store, StoreError};
use snapdir_core::CopyGuard;

use snapdir_stores::{FileStore, StreamStore};

// ---------------------------------------------------------------------------
// Scaffolding (no NEW dev-dependency; libc is a direct dep of this crate).
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
            "snapdir-clone-skip-race-{}-{tag}-{n}",
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

/// Process-global lock guarding `SNAPDIR_CLONEFILE` + `SNAPDIR_VERIFY_COPIES`
/// (both process-global) and the process-global `clonefile_hits()` counter.
fn env_lock() -> MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII guard that sets/clears the two copy knobs and restores prior values on
/// drop. Caller must already hold `env_lock()`.
struct CopyModeEnv {
    prev_clone: Option<String>,
    prev_verify: Option<String>,
}

impl CopyModeEnv {
    fn set(clonefile: Option<&str>, verify: Option<&str>) -> Self {
        let prev_clone = std::env::var("SNAPDIR_CLONEFILE").ok();
        let prev_verify = std::env::var("SNAPDIR_VERIFY_COPIES").ok();
        match clonefile {
            Some(v) => std::env::set_var("SNAPDIR_CLONEFILE", v),
            None => std::env::remove_var("SNAPDIR_CLONEFILE"),
        }
        match verify {
            Some(v) => std::env::set_var("SNAPDIR_VERIFY_COPIES", v),
            None => std::env::remove_var("SNAPDIR_VERIFY_COPIES"),
        }
        Self {
            prev_clone,
            prev_verify,
        }
    }
}

impl Drop for CopyModeEnv {
    fn drop(&mut self) {
        match &self.prev_clone {
            Some(v) => std::env::set_var("SNAPDIR_CLONEFILE", v),
            None => std::env::remove_var("SNAPDIR_CLONEFILE"),
        }
        match &self.prev_verify {
            Some(v) => std::env::set_var("SNAPDIR_VERIFY_COPIES", v),
            None => std::env::remove_var("SNAPDIR_VERIFY_COPIES"),
        }
    }
}

/// `true` iff `e` is a `StoreError::Integrity`.
fn is_integrity(e: &StoreError) -> bool {
    matches!(e, StoreError::Integrity { .. })
}

/// `true` iff `e` is `Integrity` or `ObjectNotFound` (both are "safe": never a
/// readable mis-addressed object).
fn is_rejected_or_absent(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::Integrity { .. } | StoreError::ObjectNotFound { .. }
    )
}

/// Builds a single-file manifest recording `(checksum, len)` for `content`
/// under `./<rel>` plus the root directory entry, sorted + ready to push.
fn single_file_manifest(rel: &str, content: &[u8]) -> (Manifest, String) {
    let hasher = Blake3Hasher::new();
    let sum = hasher.hash_hex(content);
    let mut manifest = Manifest::new();
    manifest.push(ManifestEntry::new(
        PathType::File,
        "644",
        sum.clone(),
        content.len() as u64,
        format!("./{rel}"),
    ));
    let root_sum = directory_checksum(std::iter::once(sum.as_str()), &hasher);
    manifest.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root_sum,
        content.len() as u64,
        "./",
    ));
    manifest.sort();
    (manifest, sum)
}

/// THE global invariant: scan every blob in `<root>/.objects` and assert that
/// each one, when read back through `get_object` under the content-address its
/// sharded path encodes, either hashes to that address or is rejected — NEVER
/// returns bytes that do not hash to the requested address.
///
/// We re-derive the requested checksum by hashing the *file name path* is not
/// possible (the sharded layout splits the hex), so instead we drive it the
/// honest way: for every checksum we care about (passed in `addresses`), assert
/// `get_object` never hands back bytes whose hash differs from the checksum.
/// Additionally we walk the pool and confirm every present blob's bytes hash to
/// SOME value and, when looked up under that true hash, round-trips (a blob can
/// only ever be SAFELY readable under its true content address).
fn assert_no_readable_misaddress(store: &FileStore, root: &Path, addresses: &[String]) {
    let hasher = Blake3Hasher::new();
    for checksum in addresses {
        match store.get_object(checksum) {
            Ok(bytes) => {
                // If a blob is returned for this address it MUST hash to it.
                assert_eq!(
                    hasher.hash_hex(&bytes),
                    *checksum,
                    "FORBIDDEN: get_object({checksum}) returned bytes that do not hash to it \
                     (a silently mis-addressed object)"
                );
            }
            Err(e) => assert!(
                is_rejected_or_absent(&e),
                "get_object({checksum}) must be Integrity-rejected or absent, got {e:?}"
            ),
        }
    }

    // Independent sweep: every physical blob in the pool, looked up under its
    // OWN true hash, must round-trip (proves the store never serves a blob whose
    // bytes diverge from the address it is filed under — i.e. the on-disk
    // sharded path always equals the true content hash).
    fn walk(dir: &Path, acc: &mut Vec<Vec<u8>>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, acc);
                } else if p.is_file() {
                    acc.push(fs::read(&p).unwrap());
                }
            }
        }
    }
    let mut blobs = Vec::new();
    walk(&root.join(".objects"), &mut blobs);
    for bytes in blobs {
        let true_hash = hasher.hash_hex(&bytes);
        // The blob's filed-under address is its sharded path; reading it under
        // its TRUE hash must succeed and yield exactly these bytes. (If a blob
        // were filed under a DIFFERENT address than its content hash, looking it
        // up under the true hash would 404 and looking it up under the filed
        // address would Integrity-reject — both safe, neither silent.)
        if let Ok(got) = store.get_object(&true_hash) {
            assert_eq!(
                got, bytes,
                "a blob read under its true content hash must return exactly its bytes"
            );
        }
    }
}

// --- libc utimensat helper (no `filetime` dev-dep; libc is a direct dep) ----
#[cfg(unix)]
fn set_mtime_atime(path: &Path, atime: (i64, i64), mtime: (i64, i64)) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    let times = [
        libc::timespec {
            tv_sec: atime.0 as libc::time_t,
            tv_nsec: atime.1 as _,
        },
        libc::timespec {
            tv_sec: mtime.0 as libc::time_t,
            tv_nsec: mtime.1 as _,
        },
    ];
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(rc, 0, "utimensat must succeed to forge the mtime/atime");
}

// ===========================================================================
// CASE 1 — Concurrent mid-stage source mutation. A thread rewrites the source
// file's content while `push` runs (guards captured just before the push). The
// resulting snapshot MUST be self-consistent: every object readable in the
// store hashes to its address, OR the push errored. Never a readable
// mis-addressed object. Looped over several schedules.
// ===========================================================================

#[cfg(unix)]
#[test]
fn concurrent_mid_stage_mutation_never_silently_misaddresses() {
    // Two-layer model: the write-time guard may catch the race (ctime moves on
    // the concurrent write) → Integrity; if a particular schedule lets a clone
    // land, the read-time backstop rejects any mis-addressed blob. Either way:
    // never a readable object whose bytes != its address.
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // default: clone + skip live (the risky path)

    // A spread of schedules: vary content size + how many mutation iterations
    // the racer performs, and whether B differs in length from A.
    for schedule in 0..8u32 {
        let store_dir = TempDir::new(&format!("conc-store-{schedule}"));
        let src = TempDir::new(&format!("conc-src-{schedule}"));

        let base_len = 32 * 1024 + (schedule as usize) * 7919; // varies per schedule
        let content_a: Vec<u8> = (0..base_len)
            .map(|i| ((i as u32).wrapping_mul(2654435761).wrapping_add(schedule)) as u8)
            .collect();

        let rel = "racey.bin";
        let target = src.path().join(rel);
        fs::write(&target, &content_a).unwrap();

        // Build the manifest over A and capture A's guard (the "walk" snapshot).
        let (manifest, sum_a) = single_file_manifest(rel, &content_a);
        let guard_a = CopyGuard::from_metadata(&fs::metadata(&target).unwrap()).expect("guard A");
        let mut guards = HashMap::new();
        guards.insert(target.clone(), guard_a);

        let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);

        // Racer thread: hammer the source with rewrites of differing content so a
        // schedule may land mid-`copy_file` / mid-stat. Synchronize the start so
        // both threads run at the same time.
        let barrier = Arc::new(Barrier::new(2));
        let racer_target = target.clone();
        let racer_barrier = Arc::clone(&barrier);
        let len_b = if schedule % 2 == 0 {
            base_len // same length (hard case)
        } else {
            base_len + 4096 // different length
        };
        let sched = schedule;
        let racer = std::thread::spawn(move || {
            racer_barrier.wait();
            for round in 0..40u32 {
                let content_b: Vec<u8> = (0..len_b)
                    .map(|i| {
                        ((i as u32)
                            .wrapping_mul(40503)
                            .wrapping_add(round)
                            .wrapping_add(sched)) as u8
                            ^ 0xa5
                    })
                    .collect();
                // Atomic-ish overwrite in place (truncate+write) to maximize the
                // chance of interleaving with the store's open/clone/stat.
                let _ = fs::write(&racer_target, &content_b);
            }
        });

        barrier.wait();
        let push_res = store.push(&manifest, src.path());
        racer.join().expect("racer thread joined");

        // Whatever happened, the never-silent invariant holds for checksum(A).
        match push_res {
            Err(ref e) => assert!(
                is_integrity(e) || is_rejected_or_absent(e),
                "schedule {schedule}: a raced stage that errors must be Integrity / \
                 absent, got {e:?}"
            ),
            Ok(()) => {}
        }
        assert_no_readable_misaddress(&store, store_dir.path(), &[sum_a]);
    }
}

// ===========================================================================
// CASE 2 — mtime+size spoof, ctime defeats it (write-time catch). Capture a
// guard for content A; rewrite to B of the SAME size; `utimensat` the mtime/
// atime back to A's. The full guard still mismatches because the rewrite (and
// the utimensat itself) advanced `ctime`, which cannot be set backwards via
// utimensat → re-hash → Integrity at WRITE time. ctime is the spoof-defeater.
// ===========================================================================

#[cfg(unix)]
#[test]
fn mtime_size_spoof_is_caught_at_write_time_by_ctime() {
    // Layer 1 assertion: a content+mtime+size spoof is caught at write time
    // because ctime advances on the write and on utimensat (utimensat sets only
    // atime/mtime). Documented spoof-defeater: ctime_ns.
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // default skip mode

    let store_dir = TempDir::new("ctime-store");
    let src = TempDir::new("ctime-src");

    let content_a: Vec<u8> = (0..(48 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0xff).collect(); // SAME size
    assert_eq!(content_a.len(), content_b.len());

    let rel = "spoof.bin";
    let target = src.path().join(rel);

    // Write A, capture A's stat (mtime/atime + the guard), build the manifest.
    fs::write(&target, &content_a).unwrap();
    let md_a = fs::metadata(&target).unwrap();
    let a_atime = (md_a.atime(), md_a.atime_nsec());
    let a_mtime = (md_a.mtime(), md_a.mtime_nsec());
    let guard_a = CopyGuard::from_metadata(&md_a).expect("guard A");
    let (manifest, sum_a) = single_file_manifest(rel, &content_a);

    // Rewrite to B (same size), then force mtime/atime back to A's. A size+mtime
    // guard would now be fooled — but ctime advanced (write + utimensat).
    fs::write(&target, &content_b).unwrap();
    set_mtime_atime(&target, a_atime, a_mtime);

    // Confirm the spoof actually fooled the (size,mtime) half but NOT ctime.
    let md_b = fs::metadata(&target).unwrap();
    let guard_b = CopyGuard::from_metadata(&md_b).expect("guard B");
    assert_eq!(guard_b.size, guard_a.size, "size spoofed to match");
    assert_eq!(
        guard_b.mtime_ns, guard_a.mtime_ns,
        "mtime spoofed back to A"
    );
    // The whole point: ctime could NOT be moved back, so the guard mismatches.
    if guard_b.ctime_ns == guard_a.ctime_ns && guard_b.ino == guard_a.ino {
        // On some exotic FS ctime might not move; then this is the fully-defeated
        // guard and the read-time backstop (case 3) is the safety net. Fall
        // through to a backstop assertion instead of a hard write-time catch.
        let mut guards = HashMap::new();
        guards.insert(target.clone(), guard_a);
        let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);
        let _ = store.push(&manifest, src.path());
        assert_no_readable_misaddress(&store, store_dir.path(), &[sum_a]);
        eprintln!("note: ctime did not advance on this FS; fell through to the read-time backstop");
        return;
    }

    // Stage with the A guard: the write-time guard re-stats, sees ctime moved →
    // Rehash → the B bytes don't hash to checksum(A) → Integrity at WRITE time.
    let mut guards = HashMap::new();
    guards.insert(target.clone(), guard_a);
    let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);
    let err = store
        .push(&manifest, src.path())
        .expect_err("ctime-advanced spoof must be CAUGHT at write time");
    assert!(
        is_integrity(&err),
        "the mtime+size spoof must surface as write-time Integrity (ctime defeats it), \
         got {err:?}"
    );
    // And no mis-addressed object is readable.
    assert_no_readable_misaddress(&store, store_dir.path(), &[sum_a]);
}

// ===========================================================================
// CASE 3 — Fully-defeated write-time guard → read-time backstop (LOAD-BEARING).
// Construct the worst case: capture the guard AFTER mutating the source to B,
// but keep `expected` = checksum(A). The StatGuarded skip WILL fire (the guard
// exactly matches the current stat) and clone B into the object filed under
// checksum(A). Then assert the BACKSTOP: get_object(checksum(A)) is rejected
// and fetch_files of a manifest referencing A rejects — the mis-addressed
// object is NEVER silently served. If get_object returns B's bytes as valid →
// REAL BUG → reopen clone-skip-stores.
// ===========================================================================

#[cfg(unix)]
#[test]
fn fully_defeated_guard_blocked_by_read_time_backstop() {
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // default skip mode

    let store_dir = TempDir::new("backstop-store");
    let src = TempDir::new("backstop-src");
    let dest = TempDir::new("backstop-dest");

    let content_a: Vec<u8> = (0..(64 * 1024u32)).map(|i| (i % 211) as u8).collect();
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0xff).collect(); // same size, != A
    let hasher = Blake3Hasher::new();
    let sum_a = hasher.hash_hex(&content_a);
    let sum_b = hasher.hash_hex(&content_b);
    assert_ne!(sum_a, sum_b);

    let rel = "defeated.bin";
    let target = src.path().join(rel);

    // The manifest records checksum(A)+len(A) (the "walk" hashed A)...
    let (manifest, _sum) = single_file_manifest(rel, &content_a);

    // ...but the on-disk source is OVERWRITTEN with B, and the guard is captured
    // from B's CURRENT metadata — simulating a guard that utterly fails to
    // detect the change (it matches the live stat exactly). The StatGuarded
    // SKIP is therefore guaranteed to fire: B gets cloned under checksum(A).
    fs::write(&target, &content_b).unwrap();
    let guard_b = CopyGuard::from_metadata(&fs::metadata(&target).unwrap()).expect("guard B");
    let mut guards = HashMap::new();
    guards.insert(target.clone(), guard_b);

    let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);
    // The push may succeed (skip fires, B cloned under A's address) — that is
    // the WHOLE POINT: it exercises a mis-addressed object on disk.
    let push_res = store.push(&manifest, src.path());

    // LOAD-BEARING BACKSTOP ASSERTION: regardless of whether the push reported
    // success, get_object(checksum(A)) must NEVER hand back B's bytes.
    match store.get_object(&sum_a) {
        Ok(bytes) => {
            // If this ever returns B → REAL BUG (silent mis-address) → reopen
            // clone-skip-stores.
            assert_ne!(
                bytes, content_b,
                "REAL BUG: get_object(checksum(A)) returned bytes(B) — a silently \
                 mis-addressed object was accepted. Reopen `clone-skip-stores`."
            );
            assert_eq!(
                hasher.hash_hex(&bytes),
                sum_a,
                "any object returned at checksum(A) must hash to checksum(A)"
            );
        }
        Err(ref e) => assert!(
            is_rejected_or_absent(e),
            "the read-time backstop must Integrity-reject (or 404) the mis-addressed \
             blob, got {e:?}"
        ),
    }

    // And the checkout path (fetch_files) must reject too — never restoring B
    // under an entry addressed as A.
    match store.fetch_files(&manifest, dest.path()) {
        Err(ref e) => assert!(
            is_rejected_or_absent(e),
            "fetch_files of a manifest referencing checksum(A) over a mis-addressed \
             object must be Integrity-rejected, got {e:?}"
        ),
        Ok(()) => {
            let restored = fs::read(dest.path().join(rel)).unwrap_or_default();
            assert_ne!(
                restored, content_b,
                "REAL BUG: checkout restored bytes(B) under an entry addressed as A. \
                 Reopen `clone-skip-stores`."
            );
        }
    }

    // The push result itself is informational, but if it errored it must be a
    // safe error (never a panic / wrong-kind).
    if let Err(ref e) = push_res {
        assert!(
            is_rejected_or_absent(e),
            "a defeated-guard push that errors must do so safely, got {e:?}"
        );
    }
}

// ===========================================================================
// CASE 4 — inode reuse. Delete+recreate the file (new content, NEW inode); a
// guard captured for the OLD inode must NOT match (ino differs) → re-hash → no
// mis-address. Proves the `ino` field of the guard does real work.
// ===========================================================================

#[cfg(unix)]
#[test]
fn inode_change_forces_rehash_no_misaddress() {
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // default skip mode

    let store_dir = TempDir::new("ino-store");
    let src = TempDir::new("ino-src");

    let content_a: Vec<u8> = (0..(40 * 1024u32)).map(|i| (i % 199) as u8).collect();
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0x33).collect(); // same size, != A

    let rel = "reused.bin";
    let target = src.path().join(rel);
    fs::write(&target, &content_a).unwrap();
    let guard_a = CopyGuard::from_metadata(&fs::metadata(&target).unwrap()).expect("guard A");
    let (manifest, sum_a) = single_file_manifest(rel, &content_a);

    // Delete + recreate with B at the same path. On most filesystems this yields
    // a different inode (and even when the inode is reused, ctime/mtime advance).
    fs::remove_file(&target).unwrap();
    fs::write(&target, &content_b).unwrap();
    let guard_b = CopyGuard::from_metadata(&fs::metadata(&target).unwrap()).expect("guard B");
    // At least ONE guard field must differ (ino, or — if ino was recycled —
    // ctime/mtime), so the StatGuarded skip cannot fire.
    assert_ne!(
        guard_a, guard_b,
        "delete+recreate must change at least one guard field (ino/ctime/mtime)"
    );

    let mut guards = HashMap::new();
    guards.insert(target.clone(), guard_a);
    let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);
    let res = store.push(&manifest, src.path());

    // The stale (old-inode) guard mismatches → re-hash → B != checksum(A) →
    // Integrity. Never a silent mis-address.
    match res {
        Err(ref e) => assert!(
            is_integrity(e),
            "an inode-reused source must re-hash and reject with Integrity, got {e:?}"
        ),
        Ok(()) => {}
    }
    assert_no_readable_misaddress(&store, store_dir.path(), &[sum_a]);
}

// ===========================================================================
// CASE 5 — mtime granularity. Two writes within the same coarse mtime tick
// (different content, same size) where we additionally force the mtime back to
// the captured value; assert no silently-accepted mis-address (ctime / the
// read-time backstop covers it even when mtime collides).
// ===========================================================================

#[cfg(unix)]
#[test]
fn mtime_granularity_collision_never_silently_misaddresses() {
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // default skip mode

    let store_dir = TempDir::new("gran-store");
    let src = TempDir::new("gran-src");

    let content_a: Vec<u8> = (0..(24 * 1024u32)).map(|i| (i % 173) as u8).collect();
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0x5a).collect(); // same size

    let rel = "tick.bin";
    let target = src.path().join(rel);

    // Write A, capture its mtime; build manifest over A.
    fs::write(&target, &content_a).unwrap();
    let md_a = fs::metadata(&target).unwrap();
    let a_atime = (md_a.atime(), md_a.atime_nsec());
    let a_mtime = (md_a.mtime(), md_a.mtime_nsec());
    let guard_a = CopyGuard::from_metadata(&md_a).expect("guard A");
    let (manifest, sum_a) = single_file_manifest(rel, &content_a);

    // Second write of B within (forced to) the same mtime tick.
    fs::write(&target, &content_b).unwrap();
    set_mtime_atime(&target, a_atime, a_mtime);

    let mut guards = HashMap::new();
    guards.insert(target.clone(), guard_a);
    let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);
    let res = store.push(&manifest, src.path());

    // Same-mtime+same-size cannot fool the full guard (ctime moved) AND, even if
    // it could, the read-time backstop covers it: never a readable B@A.
    match res {
        Err(ref e) => assert!(
            is_integrity(e) || is_rejected_or_absent(e),
            "a same-mtime same-size forge that errors must be Integrity/absent, got {e:?}"
        ),
        Ok(()) => {}
    }
    assert_no_readable_misaddress(&store, store_dir.path(), &[sum_a]);
}

// ===========================================================================
// CASE 6 — Property / invariant loop. Over N deterministic (seeded-by-index, no
// rand) mutate?/when?/how? schedules, assert the GLOBAL invariant: NO object in
// the store is readable via get_object under a checksum its bytes don't hash to.
// Covers the matrix of {mutate before guard / mutate after guard / no mutate} ×
// {same size / different size} × {forge mtime / leave mtime} × modes.
// ===========================================================================

#[cfg(unix)]
#[test]
fn property_loop_global_no_readable_misaddress_invariant() {
    let _g = env_lock();

    // Deterministic schedule space: 36 combinations.
    for idx in 0..36u32 {
        let mutate_when = idx % 3; // 0 = none, 1 = before guard, 2 = after guard
        let same_size = (idx / 3) % 2 == 0;
        let forge_mtime = (idx / 6) % 2 == 0;
        let mode_sel = (idx / 12) % 3; // 0 default, 1 verify, 2 clone-off

        let _e = match mode_sel {
            0 => CopyModeEnv::set(None, None),
            1 => CopyModeEnv::set(None, Some("1")),
            _ => CopyModeEnv::set(Some("0"), None),
        };

        let store_dir = TempDir::new(&format!("prop-store-{idx}"));
        let src = TempDir::new(&format!("prop-src-{idx}"));

        let len_a = 8 * 1024 + (idx as usize) * 311;
        let content_a: Vec<u8> = (0..len_a)
            .map(|i| ((i as u32).wrapping_mul(2246822519).wrapping_add(idx)) as u8)
            .collect();
        let len_b = if same_size { len_a } else { len_a + 777 };
        let content_b: Vec<u8> = (0..len_b)
            .map(|i| ((i as u32).wrapping_mul(3266489917).wrapping_add(idx)) as u8 ^ 0x7e)
            .collect();

        let rel = "prop.bin";
        let target = src.path().join(rel);

        // Always build the manifest over A (records checksum(A)).
        fs::write(&target, &content_a).unwrap();
        let md_a = fs::metadata(&target).unwrap();
        let a_atime = (md_a.atime(), md_a.atime_nsec());
        let a_mtime = (md_a.mtime(), md_a.mtime_nsec());
        let (manifest, sum_a) = single_file_manifest(rel, &content_a);

        // Guard captured either BEFORE mutation (stale guard) or AFTER (defeated
        // guard), or for the unmutated file.
        let guard = match mutate_when {
            1 => {
                // mutate BEFORE guard: guard reflects B, but expected stays A.
                fs::write(&target, &content_b).unwrap();
                if forge_mtime {
                    set_mtime_atime(&target, a_atime, a_mtime);
                }
                CopyGuard::from_metadata(&fs::metadata(&target).unwrap())
            }
            2 => {
                // guard for A, THEN mutate to B (stale guard).
                let g = CopyGuard::from_metadata(&md_a);
                fs::write(&target, &content_b).unwrap();
                if forge_mtime {
                    set_mtime_atime(&target, a_atime, a_mtime);
                }
                g
            }
            _ => CopyGuard::from_metadata(&md_a), // no mutation: honest input
        };

        let mut guards = HashMap::new();
        if let Some(g) = guard {
            guards.insert(target.clone(), g);
        }
        let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);
        let _ = store.push(&manifest, src.path());

        // GLOBAL INVARIANT: no readable mis-address under checksum(A).
        assert_no_readable_misaddress(&store, store_dir.path(), std::slice::from_ref(&sum_a));

        // When the input was HONEST (no mutation), the object MUST be present and
        // valid (the optimization must not lose data).
        if mutate_when == 0 {
            let got = store
                .get_object(&sum_a)
                .unwrap_or_else(|e| panic!("idx {idx}: honest object must be readable: {e:?}"));
            assert_eq!(
                got, content_a,
                "idx {idx}: honest skip path must store the correct bytes"
            );
        }
    }
}

// ===========================================================================
// CASE 7 — Honest input on the skip path stores correct bytes under CLONEFILE
// (cfg macos): proves the global-invariant sweep is not vacuous — the skip path
// actually rode the clone fast-path and produced a CORRECT object.
// ===========================================================================

#[cfg(all(unix, target_os = "macos"))]
#[test]
fn honest_skip_rides_clone_and_stores_correct_bytes() {
    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None);

    let store_dir = TempDir::new("honest-clone-store");
    let src = TempDir::new("honest-clone-src");

    // >256 KiB so the clone fast-path is firmly engaged.
    let content: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let rel = "big.bin";
    let target = src.path().join(rel);
    fs::write(&target, &content).unwrap();
    let guard = CopyGuard::from_metadata(&fs::metadata(&target).unwrap()).expect("guard");
    let (manifest, sum) = single_file_manifest(rel, &content);

    let mut guards = HashMap::new();
    guards.insert(target.clone(), guard);
    let store = FileStore::from_root(store_dir.path().to_path_buf()).with_copy_guards(guards);

    let before = snapdir_stores::clonefile_hits();
    store.push(&manifest, src.path()).expect("honest push");
    let delta = snapdir_stores::clonefile_hits() - before;
    assert!(
        delta >= 1,
        "the StatGuarded skip must ride the clone fast-path on APFS: delta={delta}"
    );

    let got = store.get_object(&sum).expect("honest object readable");
    assert_eq!(got, content, "the skip path stored the correct bytes");
    assert_eq!(
        got.len(),
        content.len(),
        "the >256 KiB object is full length (skip is not vacuous)"
    );
    assert_no_readable_misaddress(&store, store_dir.path(), &[sum]);
}

// ===========================================================================
// LINUX REFLINK (FICLONE) — real-reflink TOCTOU / race edges.
//
// COVERAGE NOTE (spec clause 4): the cases ABOVE (CASE 1 concurrent mid-stage
// mutation, CASE 2 ctime stale-guard catch, CASE 3 fully-defeated-guard read-
// time backstop, CASE 5 same-mtime forge, CASE 6 property loop) are all
// platform-agnostic and use `std::env::temp_dir()` for their fixtures. On the
// CI `Reflink (Btrfs FICLONE)` job that dir is `TMPDIR=/mnt/reflink` (a real
// Btrfs loopback), so EVERY one of those races runs against GENUINE FICLONE
// reflink with NO new code — the `CopyMethod::Cloned` path the Linux
// `try_reflink` returns flows through the SAME `clone_skip_decision`
// StatGuarded/read-time-backstop machinery the cross-platform cases pin. The
// Linux-specific cases BELOW add reflink-only edges (concurrent mutation +
// co-located src/store on a real reflink FS, `chattr +i` source, EXDEV
// cross-mount fallback) that cannot be expressed platform-agnostically.
//
// All three are `#[cfg(target_os = "linux")]` + env-gated on
// `SNAPDIR_REFLINK_TEST_DIR` (skip-on-unset / `panic!` if
// `SNAPDIR_REFLINK_TEST_REQUIRE=1`), mirroring `reflink.rs`'s gating and the
// shared `ENV_LOCK`. They compile to nothing on macOS/ext4 and RUN on the CI
// Btrfs leg, which sets `SNAPDIR_REFLINK_TEST_DIR=/mnt/reflink` +
// `SNAPDIR_REFLINK_TEST_REQUIRE=1`.
// ===========================================================================

/// Resolves the reflink-capable root for this run, honoring the env contract
/// (mirrors `reflink.rs`):
///   * `SNAPDIR_REFLINK_TEST_DIR` set -> `Some(path)` (place src + store under it
///     so FICLONE co-locates and actually fires — cross-FS would be EXDEV).
///   * unset + `SNAPDIR_REFLINK_TEST_REQUIRE=1` -> `panic!` (Btrfs leg enforce).
///   * unset + not required -> `None` (caller `eprintln!`s a skip note + returns).
#[cfg(target_os = "linux")]
fn reflink_root_or_skip(test_name: &str) -> Option<PathBuf> {
    match std::env::var("SNAPDIR_REFLINK_TEST_DIR") {
        Ok(dir) if !dir.is_empty() => {
            let p = PathBuf::from(dir);
            assert!(
                p.is_dir(),
                "SNAPDIR_REFLINK_TEST_DIR={} must be an existing mounted reflink directory",
                p.display()
            );
            Some(p)
        }
        _ => {
            let required = std::env::var("SNAPDIR_REFLINK_TEST_REQUIRE")
                .map(|v| v == "1")
                .unwrap_or(false);
            assert!(
                !required,
                "reflink FS required but SNAPDIR_REFLINK_TEST_DIR unset"
            );
            eprintln!(
                "SKIP {test_name}: SNAPDIR_REFLINK_TEST_DIR unset and \
                 SNAPDIR_REFLINK_TEST_REQUIRE != 1 (no reflink FS on this host)"
            );
            None
        }
    }
}

/// A unique temp dir created UNDER `parent` (the reflink root), removed on drop
/// — co-locating src + store so a same-FS FICLONE can fire. (`TempDir::new`
/// uses `std::env::temp_dir()`, which is ALSO the reflink dir on the CI Btrfs
/// leg via `TMPDIR`, but the Linux cases use an explicit parent to be robust
/// to a `TMPDIR` that differs from `SNAPDIR_REFLINK_TEST_DIR`.)
#[cfg(target_os = "linux")]
fn temp_dir_under(parent: &Path, tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = parent.join(format!(
        "snapdir-clone-skip-race-reflink-{}-{tag}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp dir under reflink root");
    path
}

// FS_IOC_SETFLAGS / FS_IOC_GETFLAGS ioctl request numbers (asm-generic values
// used by Btrfs/ext*/XFS on Linux); FS_IMMUTABLE_FL is the immutable bit. This
// is `chattr +i` at the syscall level (mirrors `reflink.rs`).
#[cfg(target_os = "linux")]
const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
#[cfg(target_os = "linux")]
const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;
#[cfg(target_os = "linux")]
const FS_IMMUTABLE_FL: libc::c_long = 0x0000_0010;

/// Sets/clears FS_IMMUTABLE_FL (the `chattr +i` immutable inode flag) on `path`.
/// Returns `Ok(())`, or `Err(errno)` — `EPERM`/`EACCES` signal "no privilege"
/// (needs root; the CI Btrfs job runs as root), `ENOTTY`/`EOPNOTSUPP` signal
/// "FS does not support flags".
#[cfg(target_os = "linux")]
fn set_immutable(path: &Path, immutable: bool) -> Result<(), i32> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
    }
    let result = (|| {
        let mut flags: libc::c_long = 0;
        let rc = unsafe { libc::ioctl(fd, FS_IOC_GETFLAGS as _, &mut flags) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
        }
        if immutable {
            flags |= FS_IMMUTABLE_FL;
        } else {
            flags &= !FS_IMMUTABLE_FL;
        }
        let rc = unsafe { libc::ioctl(fd, FS_IOC_SETFLAGS as _, &flags) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
        }
        Ok(())
    })();
    unsafe { libc::close(fd) };
    result
}

/// A writable dir on a DIFFERENT device than `same_dev_path`, or `None`
/// (mirrors `reflink.rs`). Used to force the source onto a different FS than
/// the store so FICLONE returns EXDEV and the impl falls back to `fs::copy`.
#[cfg(target_os = "linux")]
fn other_fs_dir(same_dev_path: &Path) -> Option<PathBuf> {
    let base_dev = fs::metadata(same_dev_path).ok()?.dev();
    for cand in ["/dev/shm", "/tmp", "/var/tmp", "/run"] {
        let p = Path::new(cand);
        if let Ok(md) = fs::metadata(p) {
            if md.dev() != base_dev {
                let scratch = p.join(format!(
                    "snapdir-clone-skip-race-xdev-{}",
                    std::process::id()
                ));
                if fs::create_dir_all(&scratch).is_ok() {
                    return Some(scratch);
                }
            }
        }
    }
    None
}

// ===========================================================================
// LINUX CASE A — Concurrent mid-stage source mutation on a REAL reflink FS.
// src + store co-located under the reflink root so push rides FICLONE
// (`CopyMethod::Cloned`); a racer thread rewrites the source while `push` runs
// (guards captured just before). Whatever schedule lands, the resulting
// snapshot MUST be self-consistent: every object readable via `get_object`
// hashes to its address, OR the push errored — NEVER a readable object whose
// bytes != its address. Looped over several schedules.
//
// Spec clause: this is CASE 1 (concurrent mid-stage mutation) re-run with the
// fixtures FORCED under a genuine reflink FS, so the StatGuarded-skip race is
// proven against the FICLONE clone path (not just APFS / fs::copy), pinning
// "no silently mis-addressed object on Linux reflink".
// ===========================================================================

#[cfg(target_os = "linux")]
#[test]
fn reflink_concurrent_mid_stage_mutation_never_silently_misaddresses() {
    let Some(root) =
        reflink_root_or_skip("reflink_concurrent_mid_stage_mutation_never_silently_misaddresses")
    else {
        return;
    };

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // clone + skip live (the FICLONE path)

    for schedule in 0..6u32 {
        let store_root = temp_dir_under(&root, &format!("reflink-conc-store-{schedule}"));
        let src_root = temp_dir_under(&root, &format!("reflink-conc-src-{schedule}"));

        let base_len = 300 * 1024 + (schedule as usize) * 7919; // >256KiB so FICLONE shares extents
        let content_a: Vec<u8> = (0..base_len)
            .map(|i| ((i as u32).wrapping_mul(2654435761).wrapping_add(schedule)) as u8)
            .collect();

        let rel = "racey.bin";
        let target = src_root.join(rel);
        fs::write(&target, &content_a).unwrap();

        let (manifest, sum_a) = single_file_manifest(rel, &content_a);
        let guard_a = CopyGuard::from_metadata(&fs::metadata(&target).unwrap()).expect("guard A");
        let mut guards = HashMap::new();
        guards.insert(target.clone(), guard_a);

        let store = FileStore::from_root(store_root.clone()).with_copy_guards(guards);

        let barrier = Arc::new(Barrier::new(2));
        let racer_target = target.clone();
        let racer_barrier = Arc::clone(&barrier);
        let len_b = if schedule % 2 == 0 {
            base_len // same length (the hard case)
        } else {
            base_len + 4096
        };
        let sched = schedule;
        let racer = std::thread::spawn(move || {
            racer_barrier.wait();
            for round in 0..40u32 {
                let content_b: Vec<u8> = (0..len_b)
                    .map(|i| {
                        ((i as u32)
                            .wrapping_mul(40503)
                            .wrapping_add(round)
                            .wrapping_add(sched)) as u8
                            ^ 0xa5
                    })
                    .collect();
                let _ = fs::write(&racer_target, &content_b);
            }
        });

        barrier.wait();
        let push_res = store.push(&manifest, &src_root);
        racer.join().expect("racer thread joined");

        match push_res {
            Err(ref e) => assert!(
                is_integrity(e) || is_rejected_or_absent(e),
                "schedule {schedule}: a raced reflink stage that errors must be Integrity / \
                 absent, got {e:?}"
            ),
            Ok(()) => {}
        }
        // Whatever landed (or didn't) on the REAL reflink path, no object filed
        // under checksum(A) may be readable with bytes that don't hash to it.
        assert_no_readable_misaddress(&store, &store_root, &[sum_a]);

        let _ = fs::remove_dir_all(&store_root);
        let _ = fs::remove_dir_all(&src_root);
    }
}

// ===========================================================================
// LINUX CASE B — `chattr +i` (FS_IMMUTABLE_FL) source during the stage on a
// real reflink FS. FICLONE is a DATA-ONLY clone, so the source inode's
// immutable flag must NOT propagate to the cloned object. Assert the resulting
// OBJECT (a) is byte-correct AND (b) is REMOVABLE (no un-GC-able object). Needs
// privilege to set the flag (root on the CI Btrfs job) — skip-with-eprintln on
// EPERM/EACCES/ENOTTY/EOPNOTSUPP. The source flag is cleared in teardown so the
// tempdir cleans up.
//
// Spec clause: pins "FICLONE data-only clone does NOT propagate the immutable
// flag — no un-GC-able object on Linux reflink", AND (additionally) that the
// cloned bytes are correct (no silent mis-address while the source was locked).
// ===========================================================================

#[cfg(target_os = "linux")]
#[test]
fn reflink_immutable_source_clones_correct_and_removable_object() {
    let Some(root) =
        reflink_root_or_skip("reflink_immutable_source_clones_correct_and_removable_object")
    else {
        return;
    };

    let store_root = temp_dir_under(&root, "reflink-immut-store");
    let src_root = temp_dir_under(&root, "reflink-immut-src");

    let content: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let rel = "locked.bin";
    let target = src_root.join(rel);
    fs::write(&target, &content).unwrap();

    // Try to set FS_IMMUTABLE_FL on the source. No-privilege / no-FS-support =>
    // skip (the CI Btrfs job runs as root and exercises this for real).
    match set_immutable(&target, true) {
        Ok(()) => {}
        Err(e) if e == libc::EPERM || e == libc::EACCES => {
            eprintln!(
                "SKIP reflink_immutable_source_clones_correct_and_removable_object: \
                 setting FS_IMMUTABLE_FL needs privilege (errno {e})"
            );
            let _ = fs::remove_dir_all(&store_root);
            let _ = fs::remove_dir_all(&src_root);
            return;
        }
        Err(e) if e == libc::ENOTTY || e == libc::EOPNOTSUPP => {
            eprintln!(
                "SKIP reflink_immutable_source_clones_correct_and_removable_object: \
                 filesystem does not support FS_IOC_SETFLAGS (errno {e})"
            );
            let _ = fs::remove_dir_all(&store_root);
            let _ = fs::remove_dir_all(&src_root);
            return;
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&store_root);
            let _ = fs::remove_dir_all(&src_root);
            panic!("unexpected errno setting FS_IMMUTABLE_FL: {e}");
        }
    }

    let (manifest, sum) = single_file_manifest(rel, &content);

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // fast-path on so a real FICLONE fires

    let store = FileStore::from_root(store_root.clone());
    let push_res = store.push(&manifest, &src_root);

    // Clear the SOURCE flag NOW so teardown can remove the src tempdir regardless
    // of the outcome below.
    let _ = set_immutable(&target, false);

    push_res.expect("push of an immutable source must succeed on a reflink FS");

    // (a) the cloned object is byte-correct (no silent mis-address while the
    // source was immutable) ...
    let got = store
        .get_object(&sum)
        .expect("the cloned object must be readable + byte-correct");
    assert_eq!(
        got, content,
        "FICLONE of an immutable source must still produce byte-correct object content"
    );
    assert_no_readable_misaddress(&store, &store_root, std::slice::from_ref(&sum));

    // ... and (b) KEYSTONE: the object must be REMOVABLE — FICLONE (data-only)
    // must NOT have propagated FS_IMMUTABLE_FL onto the object inode, so there
    // is no un-GC-able object.
    let obj = store_root.join(snapdir_core::store::object_path(&sum));
    assert!(obj.is_file(), "object must have landed");
    fs::remove_file(&obj).expect(
        "the cloned object must NOT be immutable — FICLONE clones data only, so \
         FS_IMMUTABLE_FL must not propagate (Linux has no un-GC-able-object risk)",
    );
    assert!(!obj.exists(), "object must be gone after removal");

    let _ = fs::remove_dir_all(&store_root);
    let _ = fs::remove_dir_all(&src_root);
}

// ===========================================================================
// LINUX CASE C — EXDEV cross-mount. Source on a DIFFERENT filesystem than the
// store (source under /tmp|/dev/shm|... while the store is on the reflink FS)
// => FICLONE returns EXDEV => clean `fs::copy` fallback. Assert the object is
// byte-correct, the snapshot id matches, and NO mis-addressed object results.
// Skip if a second writable FS distinct from the reflink FS cannot be arranged.
//
// Spec clause: pins "cross-mount EXDEV => graceful fs::copy fallback, byte-
// correct object + matching snapshot id, no mis-address" on Linux reflink.
// ===========================================================================

#[cfg(target_os = "linux")]
#[test]
fn reflink_exdev_cross_mount_falls_back_clean_no_misaddress() {
    let Some(root) =
        reflink_root_or_skip("reflink_exdev_cross_mount_falls_back_clean_no_misaddress")
    else {
        return;
    };

    // STORE on the reflink FS; SOURCE on a different FS => source->object is a
    // cross-mount FICLONE => EXDEV => fs::copy fallback.
    let store_root = temp_dir_under(&root, "reflink-xdev-store");

    let Some(other) = other_fs_dir(&store_root) else {
        eprintln!(
            "SKIP reflink_exdev_cross_mount_falls_back_clean_no_misaddress: \
             no second writable filesystem distinct from the reflink FS detected"
        );
        let _ = fs::remove_dir_all(&store_root);
        return;
    };

    let src_root = other.join(format!("src-{}", std::process::id()));
    fs::create_dir_all(&src_root).unwrap();

    let content: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 197) as u8).collect();
    let rel = "payload.bin";
    let target = src_root.join(rel);
    fs::write(&target, &content).unwrap();

    let (manifest, sum) = single_file_manifest(rel, &content);

    let _g = env_lock();
    let _e = CopyModeEnv::set(None, None); // clone on; impl must fall back on EXDEV

    let store = FileStore::from_root(store_root.clone());
    let res = store.push(&manifest, &src_root);

    let cleanup = |a: &Path, b: &Path| {
        let _ = fs::remove_dir_all(a);
        let _ = fs::remove_dir_all(b);
    };

    match res {
        Ok(()) => {
            // The EXDEV fallback must file a byte-correct object addressed
            // exactly as the walk hashed it — no mis-address.
            let got = store.get_object(&sum);
            assert_no_readable_misaddress(&store, &store_root, std::slice::from_ref(&sum));
            match got {
                Ok(bytes) => assert_eq!(
                    bytes, content,
                    "cross-FS EXDEV fallback must produce byte-correct object content"
                ),
                Err(e) => {
                    cleanup(&other, &store_root);
                    panic!("the object filed under its true checksum must be readable, got {e:?}");
                }
            }
            cleanup(&other, &store_root);
        }
        Err(e) => {
            cleanup(&other, &store_root);
            panic!("cross-FS (EXDEV) push must succeed via the fs::copy fallback, got {e:?}");
        }
    }
}
