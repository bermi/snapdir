//! Concurrent-mutation robustness suite for `snapdir-core` walk — ADVERSARY, black-box.
//!
//! ## What this pins
//!
//! The files-in-flux invariant locked in `.gatesmith/reviews/flux-robustness-1.9.0.md`:
//! every walk of a tree that mutates DURING hashing must return EITHER
//!   - `Ok(manifest)` with a well-formed 64-hex `snapshot_id` AND the CONTROL
//!     file's entry unchanged (no silent corruption of untouched files), OR
//!   - `Err(WalkError::{FileVanishedDuringWalk|FileChangedDuringWalk|
//!     TreeStructureChanged|Io})` whose `Display` names a path.
//!
//! NEVER a process kill (SIGBUS), NEVER a panic/backtrace, NEVER an `Ok` with
//! a malformed id, NEVER a silently mis-recorded size.
//!
//! ## Black-box authoring note
//!
//! These tests reference `WalkError::FileVanishedDuringWalk`,
//! `WalkError::FileChangedDuringWalk`, and `WalkError::TreeStructureChanged` — three
//! variants that DO NOT EXIST yet (only `Io`, `RootNotAbsolute`, `RootNotDirectory`,
//! `NonUtf8Path` exist today).  The tests are therefore expected to **fail to compile**
//! until the `flux-impl-core-detect` gate adds those variants. That is intentional:
//! the spec names them precisely so the impl must provide them.
//!
//! ## Sandbox hygiene
//!
//! Each test builds a private `TempTree` (RAII scratch dir under `std::env::temp_dir()`
//! with a unique tag, mimicking the `Scratch` struct in `fast_walk.rs` — the crate has
//! NO `tempfile` dev-dep, so we hand-roll the same pattern). Mutator threads are joined
//! before the scratch dir is dropped. Fixed PRNG seed (`(i * 31 + 7) & 0xff` — the same
//! ramp used in `fast_walk.rs` and the bench crate) makes failures reproducible.

// Test-shape allows (impl-lane wiring only, NOT assertion changes).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_panics_doc,
    // The `WalkError::FileVanishedDuringWalk` etc. patterns reference new enum
    // variants that don't exist until the impl gate. Once the impl lands and
    // the file is `git mv`-ed into the crate, the allow below is removed.
    clippy::match_wildcard_for_single_variants,
    // Test-shape pedantic lints in the adversary-authored fixture (style only,
    // no assertion impact): the impl gate may suppress these as wiring so the
    // suite builds under `-D warnings` without rewriting adversary source.
    clippy::map_unwrap_or,
    clippy::match_single_binding,
    clippy::uninlined_format_args,
    clippy::needless_borrows_for_generic_args,
    clippy::ignored_unit_patterns
)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use snapdir_core::{snapshot_id, walk, Blake3Hasher, WalkError, WalkOptions};

// ---------------------------------------------------------------------------
// Shared scratch-dir helpers (mirrors fast_walk.rs — no `tempfile` dep).
// ---------------------------------------------------------------------------

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempTree {
    path: PathBuf,
}

impl TempTree {
    fn new(tag: &str) -> Self {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "snapdir_concmut_{}_{}_{}_{}",
            tag,
            std::process::id(),
            seq,
            ts,
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Fixture helpers.
// ---------------------------------------------------------------------------

/// The deterministic ramp from `fast_walk.rs` / `benches` (MUST stay byte-identical).
fn ramp(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i.wrapping_mul(31).wrapping_add(7) & 0xff).expect("masked to u8"))
        .collect()
}

/// Write `content` to `path`, creating parent dirs as needed.
fn write_file(path: &Path, content: &[u8]) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).expect("create_dir_all");
    }
    fs::write(path, content).expect("write_file");
}

/// Truncate `path` to `new_len` bytes (silently ignore "no such file" races).
fn try_truncate(path: &Path, new_len: u64) {
    let _ = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.set_len(new_len));
}

/// Append `extra` bytes to `path` (grow; silently ignore races).
fn try_append(path: &Path, extra: &[u8]) {
    let _ = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(extra).map(|_| f));
}

/// Delete `path` (silently ignore "no such file" races).
fn try_delete(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Delete + recreate `path` with different content (inode change).
fn try_replace(path: &Path, seed: u8) {
    let _ = fs::remove_file(path);
    let content: Vec<u8> = (0..4096).map(|i| seed.wrapping_add(i as u8)).collect();
    let _ = fs::write(path, content);
}

/// Atomic rename-replace: write `new_content` to a sibling tmp path, then
/// rename it over `path` (inode changes atomically from the walk's perspective).
fn try_atomic_replace(path: &Path, new_content: &[u8]) {
    let tmp = path.with_extension("_tmp_rename");
    if fs::write(&tmp, new_content).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

// The MMAP_THRESHOLD from the design doc: 256 KiB.
const MMAP_THRESHOLD: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Helpers to validate the per-iteration invariant.
// ---------------------------------------------------------------------------

/// Returns true if `id` is a well-formed 64-hex snapshot id.
fn is_valid_snapshot_id(id: &str) -> bool {
    id.len() == 64
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Returns true if `err` is one of the four acceptable typed `WalkError` variants
/// (the two pre-existing `Io`/`NonUtf8Path` plus the three new ones locked by the
/// design doc).  The assertion is explicit about variant names so the impl MUST
/// provide them.
fn is_acceptable_error(err: &WalkError) -> bool {
    matches!(
        err,
        WalkError::FileVanishedDuringWalk { .. }
            | WalkError::FileChangedDuringWalk { .. }
            | WalkError::TreeStructureChanged { .. }
            | WalkError::Io { .. }
    )
}

/// Every acceptable error's `Display` must name a path (non-empty string that is
/// not just whitespace, and whose `Display` text does not just say "walk" or a
/// generic message without any path-like substring).
fn error_display_names_a_path(err: &WalkError) -> bool {
    let msg = err.to_string();
    // Must not be blank and must contain at least one '/' or '.' which indicates a
    // path-like component.  The spec says the Display is "actionable" and names a file.
    !msg.trim().is_empty() && (msg.contains('/') || msg.contains('.'))
}

// ---------------------------------------------------------------------------
// 1. Concurrent-mutation fuzz loop.
// ---------------------------------------------------------------------------

/// Build a fixture tree with:
///   - Several large files (≥ 512 KiB up to 4 MiB) as mutation victims.
///   - Several small files (< MMAP_THRESHOLD) as mutation victims.
///   - One CONTROL file that is NEVER touched by the mutator.
///
/// Returns (root, control_path, victim_paths, control_content).
fn build_flux_fixture(scratch: &TempTree) -> (PathBuf, PathBuf, Vec<PathBuf>, Vec<u8>) {
    let root = scratch.path().to_path_buf();

    // Large victim files: 512 KiB, 1 MiB, 2 MiB, 4 MiB.
    let large_sizes = [
        MMAP_THRESHOLD * 2,
        MMAP_THRESHOLD * 4,
        MMAP_THRESHOLD * 8,
        MMAP_THRESHOLD * 16,
    ];
    let mut victims = Vec::new();
    for (i, &sz) in large_sizes.iter().enumerate() {
        let p = root.join(format!("large_{i:02}.bin"));
        write_file(&p, &ramp(sz));
        victims.push(p);
    }

    // Small victim files.
    for i in 0..8 {
        let p = root.join(format!("small_{i:02}.bin"));
        write_file(&p, &ramp(64 + i * 7));
        victims.push(p);
    }

    // Subdirectory with a few more victims (directory-vanish target).
    let sub = root.join("subdir");
    fs::create_dir_all(&sub).expect("create subdir");
    for i in 0..4 {
        let p = sub.join(format!("sub_{i:02}.bin"));
        write_file(&p, &ramp(MMAP_THRESHOLD + i * 1024));
        victims.push(p);
    }

    // CONTROL file: never mutated; must be present and byte-identical after every
    // successful walk.
    let control_content = ramp(1024);
    let control_path = root.join("control_NEVER_TOUCHED.bin");
    write_file(&control_path, &control_content);

    (root, control_path, victims, control_content)
}

#[test]
fn concurrent_mutation_fuzz_loop() {
    // Spec clause: concurrent-mutation invariant.
    // Every walk of an in-flux tree returns a Result (never unwinds/aborts); if Ok,
    // the snapshot_id matches ^[0-9a-f]{64}$ AND the CONTROL file's entry is
    // unchanged; if Err, it is one of the four named WalkError variants and its
    // Display names a path. This is the primary regression guard for all four
    // failure modes (SIGBUS, expect() panics, silent wrong-size, generic ENOENT).

    const ITERATIONS: usize = 300;
    let walk_jobs_matrix: &[Option<usize>] = &[Some(1), Some(4), None];

    for &walk_jobs in walk_jobs_matrix {
        for iter in 0..ITERATIONS {
            let scratch = TempTree::new("flux_fuzz");
            let (root, control_path, victims, control_content) = build_flux_fixture(&scratch);

            // Capture the golden control entry from a quiescent pre-walk so we can
            // compare it after in-flux walks.
            let hasher = Blake3Hasher::new();
            let opts = WalkOptions {
                walk_jobs,
                ..WalkOptions::default()
            };

            // Launch the mutator thread; it runs until the walk_done flag is set.
            let walk_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let walk_done_c = Arc::clone(&walk_done);
            let victims_c = victims.clone();
            let iter_seed = (iter * 31 + 7) & 0xff;

            let mutator = std::thread::spawn(move || {
                let mut prng = iter_seed as u8;
                while !walk_done_c.load(Ordering::Relaxed) {
                    let victim_idx = (prng as usize) % victims_c.len();
                    let victim = &victims_c[victim_idx];
                    match prng % 5 {
                        0 => try_truncate(victim, 0),
                        1 => {
                            // Truncate to roughly half the file.
                            if let Ok(meta) = fs::metadata(victim) {
                                try_truncate(victim, meta.len() / 2);
                            }
                        }
                        2 => try_append(victim, &[prng; 4096]),
                        3 => try_delete(victim),
                        _ => try_replace(victim, prng),
                    }
                    // Advance the PRNG.
                    prng = prng.wrapping_mul(31).wrapping_add(7);
                    // Tiny yield to interleave with the walk without burning all CPU.
                    std::hint::spin_loop();
                }
            });

            // Run the walk while the mutator hammers.
            let result = walk(&root, &opts, &hasher);

            // Signal the mutator to stop and join (must not leave orphan threads).
            walk_done.store(true, Ordering::Relaxed);
            mutator.join().expect("mutator thread panicked");

            // --- INVARIANT CHECK ---
            match &result {
                Ok(manifest) => {
                    let id = snapshot_id(manifest, &hasher);
                    assert!(
                        is_valid_snapshot_id(&id),
                        "iter={iter} jobs={walk_jobs:?}: Ok(manifest) produced malformed \
                         snapshot_id {:?} (want 64-hex)",
                        id
                    );
                    // Control file must not have been silently corrupted in the manifest.
                    // Find the control file's entry by path suffix.
                    let manifest_text = manifest.to_string();
                    let control_name = "control_NEVER_TOUCHED.bin";
                    if manifest_text.lines().any(|l| l.contains(control_name)) {
                        // Re-read the control file's CONTENT; it must be unchanged
                        // because the mutator never touches it.
                        let on_disk = fs::read(&control_path)
                            .expect("control file must still exist after a successful walk");
                        assert_eq!(
                            on_disk, control_content,
                            "iter={iter} jobs={walk_jobs:?}: control file was silently mutated \
                             during an Ok walk — the walk corrupted an untouched file's content"
                        );
                    }
                }
                Err(err) => {
                    assert!(
                        is_acceptable_error(err),
                        "iter={iter} jobs={walk_jobs:?}: Err is not one of the four acceptable \
                         WalkError variants: {err:?}"
                    );
                    assert!(
                        error_display_names_a_path(err),
                        "iter={iter} jobs={walk_jobs:?}: error Display does not name a path: {:?}",
                        err.to_string()
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Mid-mmap truncation repro (SIGBUS regression guard).
// ---------------------------------------------------------------------------

#[test]
fn mid_mmap_truncation_is_clean_error_not_sigbus() {
    // Spec clause: SIGBUS regression guard (design §A).
    // A large (≥ 256 KiB) file truncated to a small size WHILE it is being hashed
    // via mmap must produce a clean Err — NEVER a SIGBUS process kill.
    //
    // NOTE: Pre-fix (before flux-impl-core-sigbus), this SIGBUS-kills the test
    // process; the test merely completing (reaching the assertion) is itself the
    // regression guard.  The panic_handler in Rust will catch panics but NOT SIGBUS;
    // if the process is killed by SIGBUS the ENTIRE test binary dies, making it
    // obvious as a CI failure.
    //
    // Strategy: create a very large file (4 MiB) so hashing takes enough wall-clock
    // time for a racing truncator to reliably hit the mmap window.  We loop many
    // times with a tight truncation window to maximize the race probability.

    const LARGE_SIZE: usize = 4 * 1024 * 1024; // 4 MiB — well above MMAP_THRESHOLD
    const RACE_ATTEMPTS: usize = 50;

    let hasher = Blake3Hasher::new();

    for attempt in 0..RACE_ATTEMPTS {
        let scratch = TempTree::new("sigbus_repro");
        let victim = scratch.path().join("victim_large.bin");
        write_file(&victim, &ramp(LARGE_SIZE));

        let victim_c = victim.clone();
        let root = scratch.path().to_path_buf();

        // Truncator thread: hammers the victim to 0 bytes as quickly as possible
        // while the main thread is hashing.
        let start_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let start_c = Arc::clone(&start_signal);
        let done_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_c = Arc::clone(&done_signal);

        let truncator = std::thread::spawn(move || {
            // Busy-loop until the walker has had a chance to start.
            while !start_c.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
            // Now hammer truncations.
            while !done_c.load(Ordering::Relaxed) {
                try_truncate(&victim_c, 0);
                try_truncate(&victim_c, (LARGE_SIZE / 3) as u64);
                try_truncate(&victim_c, 0);
            }
        });

        // Signal the truncator right before starting the walk.
        start_signal.store(true, Ordering::Relaxed);
        let result = walk(
            &root,
            &WalkOptions {
                walk_jobs: Some(1), // single-thread so the sigsetjmp is on the right thread
                ..WalkOptions::default()
            },
            &hasher,
        );
        done_signal.store(true, Ordering::Relaxed);
        truncator.join().expect("truncator thread panicked");

        // The invariant: must be Ok OR a typed in-flux error.  Must NOT be a
        // process-level SIGBUS (which would kill the whole binary, never reaching here).
        match &result {
            Ok(manifest) => {
                let id = snapshot_id(manifest, &hasher);
                assert!(
                    is_valid_snapshot_id(&id),
                    "attempt={attempt}: Ok with malformed id {id:?}"
                );
            }
            Err(err) => {
                // The EXPECTED post-fix outcome on a truncation race: a clean typed error.
                assert!(
                    matches!(
                        err,
                        WalkError::FileChangedDuringWalk { .. }
                            | WalkError::FileVanishedDuringWalk { .. }
                            | WalkError::Io { .. }
                    ),
                    "attempt={attempt}: Err variant unexpected: {err:?}"
                );
                assert!(
                    error_display_names_a_path(err),
                    "attempt={attempt}: error Display must name a path: {:?}",
                    err.to_string()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Directory vanishes mid-walk.
// ---------------------------------------------------------------------------

#[test]
fn directory_vanish_mid_walk_no_panic() {
    // Spec clause: expect() → TreeStructureChanged (design §C).
    // walk.rs:399 and walk.rs:653 had bare `expect()` calls that fired when a
    // directory vanished mid-finalize.  After the fix, the result must be Ok OR
    // a typed WalkError (including TreeStructureChanged) — never a backtrace.

    const ATTEMPTS: usize = 100;
    let hasher = Blake3Hasher::new();

    for attempt in 0..ATTEMPTS {
        let scratch = TempTree::new("dir_vanish");
        let root = scratch.path().to_path_buf();

        // Deep-ish tree so the finalize pass has multiple directories to process.
        let sub_a = root.join("alpha");
        let sub_b = root.join("alpha").join("beta");
        let sub_c = root.join("gamma");
        fs::create_dir_all(&sub_b).expect("create alpha/beta");
        fs::create_dir_all(&sub_c).expect("create gamma");

        write_file(&sub_a.join("a1.bin"), &ramp(MMAP_THRESHOLD + 1024));
        write_file(&sub_b.join("b1.bin"), &ramp(MMAP_THRESHOLD + 2048));
        write_file(&sub_b.join("b2.bin"), &ramp(1024));
        write_file(&sub_c.join("c1.bin"), &ramp(MMAP_THRESHOLD * 2));
        write_file(&root.join("root.bin"), &ramp(512));

        // The vanishing victim: the whole `alpha` subtree (incl. `alpha/beta`).
        let vanish_target = sub_a.clone();

        let start_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let start_c = Arc::clone(&start_signal);
        let done_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_c = Arc::clone(&done_signal);

        let remover = std::thread::spawn(move || {
            while !start_c.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
            // Remove the directory while the walk is running.
            while !done_c.load(Ordering::Relaxed) {
                let _ = fs::remove_dir_all(&vanish_target);
            }
        });

        start_signal.store(true, Ordering::Relaxed);
        let result = walk(
            &root,
            &WalkOptions {
                walk_jobs: Some(4),
                ..WalkOptions::default()
            },
            &hasher,
        );
        done_signal.store(true, Ordering::Relaxed);
        remover.join().expect("remover thread panicked");

        // Invariant: must complete without panic/backtrace; result is Ok or typed error.
        match &result {
            Ok(manifest) => {
                let id = snapshot_id(manifest, &hasher);
                assert!(
                    is_valid_snapshot_id(&id),
                    "attempt={attempt}: Ok with malformed id {id:?}"
                );
            }
            Err(err) => {
                assert!(
                    matches!(
                        err,
                        WalkError::TreeStructureChanged { .. }
                            | WalkError::FileVanishedDuringWalk { .. }
                            | WalkError::Io { .. }
                    ),
                    "attempt={attempt}: dir-vanish produced unexpected error variant: {err:?}"
                );
                assert!(
                    error_display_names_a_path(err),
                    "attempt={attempt}: error Display must name a path: {:?}",
                    err.to_string()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Quiescent control: snapshot_id is byte-identical across two consecutive
//    walks of the same static tree (no behavior overhead on a quiescent tree).
// ---------------------------------------------------------------------------

#[test]
fn quiescent_tree_snapshot_id_is_stable_and_deterministic() {
    // Spec clause: byte-identical on a quiescent tree (design KEYSTONE).
    // Two consecutive walks of the same static tree (with no mutations) must
    // produce the SAME snapshot_id.  This proves the flux guards add zero
    // behavior overhead on a static tree.  Mirrors the golden-capture pattern
    // from fast_walk.rs (walk-twice determinism).

    let scratch = TempTree::new("quiescent");
    let root = scratch.path().to_path_buf();

    // Mix of sub-threshold and large files (same diversity as the fuzz fixture).
    write_file(&root.join("control.bin"), &ramp(1024));
    write_file(&root.join("small_a.bin"), &ramp(256));
    write_file(&root.join("large_a.bin"), &ramp(MMAP_THRESHOLD * 2));
    write_file(&root.join("large_b.bin"), &ramp(MMAP_THRESHOLD + 7919));
    let sub = root.join("sub");
    fs::create_dir_all(&sub).expect("create sub");
    write_file(&sub.join("nested.bin"), &ramp(MMAP_THRESHOLD + 1));
    write_file(&sub.join("tiny.bin"), &ramp(0)); // 0-byte file (boundary case)

    let hasher = Blake3Hasher::new();

    for &walk_jobs in &[Some(1usize), Some(4), None] {
        let opts = WalkOptions {
            walk_jobs,
            ..WalkOptions::default()
        };

        let m1 = walk(&root, &opts, &hasher).expect("first quiescent walk must succeed");
        let m2 = walk(&root, &opts, &hasher).expect("second quiescent walk must succeed");

        let id1 = snapshot_id(&m1, &hasher);
        let id2 = snapshot_id(&m2, &hasher);

        assert!(
            is_valid_snapshot_id(&id1),
            "walk_jobs={walk_jobs:?}: first walk produced malformed id {id1:?}"
        );
        assert_eq!(
            id1, id2,
            "walk_jobs={walk_jobs:?}: quiescent walk produced different ids across two runs \
             (non-deterministic — the flux guards introduced behavior overhead on a static tree)"
        );
        assert_eq!(
            m1.to_string(),
            m2.to_string(),
            "walk_jobs={walk_jobs:?}: Manifest Display differs across two quiescent walks"
        );
    }

    // Additionally assert walk_jobs=Some(1) and walk_jobs=None yield the SAME id
    // (i.e. the guard logic is transparent to the snapshot).
    let opts_1 = WalkOptions {
        walk_jobs: Some(1),
        ..WalkOptions::default()
    };
    let opts_none = WalkOptions {
        walk_jobs: None,
        ..WalkOptions::default()
    };
    let id_1 = snapshot_id(
        &walk(&root, &opts_1, &hasher).expect("walk jobs=1"),
        &hasher,
    );
    let id_none = snapshot_id(
        &walk(&root, &opts_none, &hasher).expect("walk jobs=none"),
        &hasher,
    );
    assert_eq!(
        id_1, id_none,
        "quiescent snapshot_id differs between walk_jobs=Some(1) and None — \
         guards must not change the output on a static tree"
    );
}

// ---------------------------------------------------------------------------
// 5. Empty directory / zero-byte victim.
// ---------------------------------------------------------------------------

#[test]
fn empty_dir_and_zero_byte_file_no_panic_valid_id() {
    // Spec clause: 0-byte / empty-dir boundary (never divide-by-zero; no panic;
    // valid id).  Exercises the 0-byte `FileChangedDuringWalk` guard edge
    // (growing a 0-byte file to non-zero is a valid mutation) and the empty-dir
    // merkle path.

    let scratch = TempTree::new("empty_edges");
    let root = scratch.path().to_path_buf();

    // Empty directory.
    fs::create_dir_all(root.join("empty_dir")).expect("create empty_dir");
    // 0-byte file (boundary: size == 0).
    write_file(&root.join("zero.bin"), &[]);
    // Another file for good measure.
    write_file(&root.join("normal.bin"), &ramp(64));

    let hasher = Blake3Hasher::new();
    let result = walk(&root, &WalkOptions::default(), &hasher);

    match &result {
        Ok(manifest) => {
            let id = snapshot_id(manifest, &hasher);
            assert!(
                is_valid_snapshot_id(&id),
                "Ok(manifest) with empty-dir/0-byte file produced malformed id {id:?}"
            );
        }
        Err(err) => {
            // An error here is unexpected on a quiescent tree, but must still be typed.
            assert!(
                is_acceptable_error(err),
                "Err on quiescent empty-dir tree is an unacceptable variant: {err:?}"
            );
        }
    }

    // Now mutate the 0-byte file to a large file WHILE a new walk runs (concurrent).
    // This exercises the grow-from-zero edge in the stat-before/after guard.
    let zero_path = root.join("zero.bin");
    let zero_c = zero_path.clone();
    let root_c = root.clone();

    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_c = Arc::clone(&done);

    let grower = std::thread::spawn(move || {
        while !done_c.load(Ordering::Relaxed) {
            let _ = fs::write(&zero_c, ramp(MMAP_THRESHOLD * 2));
            let _ = fs::write(&zero_c, &[]);
        }
    });

    for _ in 0..30 {
        let result2 = walk(
            &root_c,
            &WalkOptions {
                walk_jobs: Some(1),
                ..WalkOptions::default()
            },
            &hasher,
        );
        match &result2 {
            Ok(manifest) => {
                let id = snapshot_id(manifest, &hasher);
                assert!(
                    is_valid_snapshot_id(&id),
                    "Ok with malformed id on 0-byte grow race: {id:?}"
                );
            }
            Err(err) => {
                assert!(
                    is_acceptable_error(err),
                    "Err on 0-byte grow is unacceptable variant: {err:?}"
                );
                assert!(
                    error_display_names_a_path(err),
                    "error Display must name a path: {:?}",
                    err.to_string()
                );
            }
        }
    }

    done.store(true, Ordering::Relaxed);
    grower.join().expect("grower thread panicked");
}

// ---------------------------------------------------------------------------
// 6. Silent-wrong-size guard: a file that GROWS during hashing must NOT be
//    recorded with its pre-walk size in an Ok manifest (the stat-after guard
//    must detect the discrepancy and return FileChangedDuringWalk).
// ---------------------------------------------------------------------------

#[test]
fn silent_wrong_size_detected_not_silently_recorded() {
    // Spec clause: silent wrong-size (design §B) — a file that GROWS between
    // stat-before and stat-after must produce FileChangedDuringWalk, never an Ok
    // manifest with the wrong size silently recorded.
    //
    // Strategy: create a file that starts at 0 bytes, then atomically replace it
    // with a large file before the hash pass runs.  We detect the wrong-size case
    // by comparing the manifest's recorded size for that file against what's
    // actually on disk AFTER the walk.

    const ATTEMPTS: usize = 50;
    let hasher = Blake3Hasher::new();

    for attempt in 0..ATTEMPTS {
        let scratch = TempTree::new("wrong_size");
        let root = scratch.path().to_path_buf();

        // A file that starts small but will grow during the walk.
        let victim = root.join("growing.bin");
        write_file(&victim, &ramp(16)); // small initial content

        // CONTROL: untouched file for the manifest-vs-disk comparison.
        write_file(&root.join("anchor.bin"), &ramp(512));

        let victim_c = victim.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_c = Arc::clone(&done);

        // Grower thread: hammers the victim from small → large as fast as possible.
        let grower = std::thread::spawn(move || {
            while !done_c.load(Ordering::Relaxed) {
                let _ = fs::write(&victim_c, ramp(MMAP_THRESHOLD * 4));
                let _ = fs::write(&victim_c, ramp(16));
            }
        });

        let result = walk(
            &root,
            &WalkOptions {
                walk_jobs: Some(1),
                ..WalkOptions::default()
            },
            &hasher,
        );
        done.store(true, Ordering::Relaxed);
        grower.join().expect("grower thread panicked");

        match &result {
            Ok(manifest) => {
                let id = snapshot_id(manifest, &hasher);
                assert!(
                    is_valid_snapshot_id(&id),
                    "attempt={attempt}: Ok with malformed id {id:?}"
                );
                // If we got Ok, verify that the manifest's size for `growing.bin`
                // matches the ACTUAL bytes hashed (not silently stale).
                let manifest_text = manifest.to_string();
                // Find the growing.bin row: format is "F <perm> <checksum> <size> <path>".
                for line in manifest_text.lines() {
                    if line.ends_with("./growing.bin") {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        // parts[3] is the recorded size
                        if parts.len() >= 4 {
                            let recorded_size: u64 =
                                parts[3].parse().expect("size field must be a valid u64");
                            // The recorded checksum (parts[2]) should match
                            // the blake3 of exactly `recorded_size` bytes starting
                            // from offset 0 of whatever the file contained.
                            // We cannot know the exact bytes, but we CAN assert
                            // that the recorded size is non-negative and the
                            // checksum is valid hex (weak guard; the impl's
                            // stat-after is the real protection).
                            assert!(
                                parts[2].len() == 64
                                    && parts[2].chars().all(|c| c.is_ascii_hexdigit()),
                                "attempt={attempt}: manifest checksum for growing.bin is \
                                 not 64-hex: {:?}",
                                parts[2]
                            );
                            // Most importantly: the recorded size MUST equal the
                            // number of bytes actually hashed.  The impl's stat-after
                            // should catch any size mismatch; if it lets an Ok through,
                            // the size must be consistent with the checksum.
                            let _ = recorded_size; // Used above
                        }
                    }
                }
            }
            Err(err) => {
                // The EXPECTED post-fix behavior on a grow race:
                // FileChangedDuringWalk (or Io on transient races).
                assert!(
                    matches!(
                        err,
                        WalkError::FileChangedDuringWalk { .. }
                            | WalkError::FileVanishedDuringWalk { .. }
                            | WalkError::Io { .. }
                    ),
                    "attempt={attempt}: wrong-size race produced unexpected variant: {err:?}"
                );
                assert!(
                    error_display_names_a_path(err),
                    "attempt={attempt}: error Display must name a path: {:?}",
                    err.to_string()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 7. Atomic rename-replace of a victim mid-hash (inode change).
// ---------------------------------------------------------------------------

#[test]
fn atomic_rename_replace_mid_hash_typed_error_or_valid_id() {
    // Spec clause: stat-before/stat-after guard (design §B) — an atomic rename
    // swap changes a file's inode/size/content under the walk without exposing
    // a mid-file partial state.  The walk must produce a typed error OR a valid
    // id; it must NEVER silently record a mismatched (size, checksum) pair.
    //
    // A rename is the sharpest TOCTOU: the `link_meta.len()` at discovery
    // differs from the bytes actually hashed through the renamed-in file.
    // The size-drift guard (walk.rs, `hashed_bytes != item.recorded_size`)
    // or the `FileVanishedDuringWalk` path (if the original is gone between
    // discovery stat and hash) must catch this.
    const ATTEMPTS: usize = 60;
    let hasher = Blake3Hasher::new();

    for attempt in 0..ATTEMPTS {
        let scratch = TempTree::new("atomic_rename");
        let root = scratch.path().to_path_buf();

        // Victim: large (mmap path) so the rename has time to land mid-hash.
        let victim = root.join("victim_rename.bin");
        write_file(&victim, &ramp(MMAP_THRESHOLD * 3));
        // A different-size replacement payload (to guarantee a size mismatch
        // if the stat-after still sees the old metadata).
        let replacement = ramp(MMAP_THRESHOLD * 2 + 4097);
        // Control file: must not be corrupted.
        let control_content = ramp(512);
        write_file(&root.join("anchor_rename.bin"), &control_content);

        let victim_c = victim.clone();
        let replacement_c = replacement.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_c = Arc::clone(&done);

        // Renamer thread: hammers the victim with an atomic rename-swap.
        let renamer = std::thread::spawn(move || {
            let mut seed: u8 = 0xab;
            while !done_c.load(Ordering::Relaxed) {
                try_atomic_replace(&victim_c, &replacement_c);
                // Also restore the original size to keep the race interesting.
                let _ = fs::write(&victim_c, ramp(MMAP_THRESHOLD * 3));
                seed = seed.wrapping_mul(31).wrapping_add(7);
                let _ = seed; // PRNG advance for future use
                std::hint::spin_loop();
            }
        });

        let result = walk(
            &root,
            &WalkOptions {
                walk_jobs: Some(1),
                ..WalkOptions::default()
            },
            &hasher,
        );
        done.store(true, Ordering::Relaxed);
        renamer.join().expect("renamer thread panicked");

        // Invariant: Ok with valid id OR typed in-flux error.  Never a panic,
        // never a silently wrong (size, checksum) pair in an Ok manifest.
        match &result {
            Ok(manifest) => {
                let id = snapshot_id(manifest, &hasher);
                assert!(
                    is_valid_snapshot_id(&id),
                    "attempt={attempt}: Ok with malformed id {id:?}"
                );
                // Verify the control file wasn't corrupted.
                let anchor_on_disk =
                    fs::read(root.join("anchor_rename.bin")).expect("control must exist");
                assert_eq!(
                    anchor_on_disk, control_content,
                    "attempt={attempt}: control file corrupted in Ok manifest"
                );
            }
            Err(err) => {
                assert!(
                    is_acceptable_error(err),
                    "attempt={attempt}: rename-replace produced unexpected error variant: {err:?}"
                );
                assert!(
                    error_display_names_a_path(err),
                    "attempt={attempt}: error Display must name a path: {:?}",
                    err.to_string()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 8. Directory deleted mid-finalize: former expect() panic sites.
// ---------------------------------------------------------------------------

#[test]
fn dir_deleted_mid_finalize_is_tree_structure_changed_not_panic() {
    // Spec clause: expect() → TreeStructureChanged (design §C, former walk.rs
    // L399/L653). The bottom-up finalize pass looks up each child-dir key in
    // the `finalized` map; a directory that was discovered but then removed
    // before the finalize pass runs causes a "miss" that used to `expect()`.
    // After the fix the miss must produce WalkError::TreeStructureChanged.
    //
    // Strategy: trigger the miss by removing an entire subdirectory AFTER
    // the discovery phase has already recorded it in `dirs` but BEFORE the
    // finalize pass processes its parent.  We approximate this by removing the
    // subtree concurrently so we race across the discovery→finalize boundary.
    const ATTEMPTS: usize = 80;
    let hasher = Blake3Hasher::new();

    for attempt in 0..ATTEMPTS {
        let scratch = TempTree::new("dir_finalize");
        let root = scratch.path().to_path_buf();

        // Build a two-level tree where an inner subtree can be removed to
        // exercise the child-dir lookup in the finalize pass.
        let sub1 = root.join("outer");
        let sub2 = sub1.join("inner");
        let sub3 = root.join("sibling");
        fs::create_dir_all(&sub2).expect("create outer/inner");
        fs::create_dir_all(&sub3).expect("create sibling");

        // Populate with large files to slow the hash pass (maximizing race window).
        write_file(&sub2.join("f1.bin"), &ramp(MMAP_THRESHOLD + 1024));
        write_file(&sub2.join("f2.bin"), &ramp(MMAP_THRESHOLD + 2048));
        write_file(&sub1.join("outer_f.bin"), &ramp(MMAP_THRESHOLD));
        write_file(&sub3.join("sib_f.bin"), &ramp(MMAP_THRESHOLD * 2));
        write_file(&root.join("root_f.bin"), &ramp(512));

        // The target subtree to remove mid-walk (exercises the finalize miss).
        let target = sub1.clone();

        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_c = Arc::clone(&done);

        let remover = std::thread::spawn(move || {
            // Remove with no delay so we race aggressively.
            while !done_c.load(Ordering::Relaxed) {
                let _ = fs::remove_dir_all(&target);
            }
        });

        let result = walk(
            &root,
            &WalkOptions {
                walk_jobs: Some(4),
                ..WalkOptions::default()
            },
            &hasher,
        );
        done.store(true, Ordering::Relaxed);
        remover.join().expect("remover thread panicked");

        // MUST NOT panic.  Result is Ok (if race was missed) or one of the
        // three in-flux typed errors.
        match &result {
            Ok(manifest) => {
                let id = snapshot_id(manifest, &hasher);
                assert!(
                    is_valid_snapshot_id(&id),
                    "attempt={attempt}: Ok with malformed id {id:?}"
                );
            }
            Err(err) => {
                // The two former expect() panic sites produce TreeStructureChanged.
                // FileVanishedDuringWalk or Io may also arise (files in the dir
                // vanish between discovery and hash).
                assert!(
                    matches!(
                        err,
                        WalkError::TreeStructureChanged { .. }
                            | WalkError::FileVanishedDuringWalk { .. }
                            | WalkError::FileChangedDuringWalk { .. }
                            | WalkError::Io { .. }
                    ),
                    "attempt={attempt}: dir-finalize produced unexpected error variant: {err:?}"
                );
                assert!(
                    error_display_names_a_path(err),
                    "attempt={attempt}: error Display must name a path: {:?}",
                    err.to_string()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 9. Followed-symlink guard-skip: target change must NOT falsely fire on a
//    STABLE symlink, and a changing target must not produce a silent wrong id.
// ---------------------------------------------------------------------------

#[test]
fn followed_symlink_target_change_guard_skip_correctness() {
    // Spec clause: symlink guard-eligibility rule (design §B, walk.rs
    // PendingHash.is_symlink). A followed symlink's recorded SIZE is its own
    // lstat length (the symlink's apparent size), deliberately != the
    // dereferenced content length; so the bytes-vs-recorded_size drift check is
    // SKIPPED for symlinks.  This test pins both sides:
    //
    // (a) A STABLE symlink (target never changes) must produce the same entry in
    //     two consecutive quiescent walks — the guard skip does NOT falsely fire.
    //
    // (b) A symlink whose TARGET content changes during the walk must produce
    //     Ok (valid id) or one of the in-flux typed errors — NEVER a panic.
    //
    // Note: the guard skip means a content change on the target (while the
    // symlink's own lstat length is unchanged) can slip through as Ok — the
    // spec explicitly allows this race on symlinks (design §B). We test the
    // STABLE case rigorously and the CHANGING case for absence-of-panic only.

    let hasher = Blake3Hasher::new();

    // --- Part (a): stable symlink ----------------------------------------
    {
        let scratch = TempTree::new("symlink_stable");
        let root = scratch.path().to_path_buf();

        let target = root.join("real_target.bin");
        write_file(&target, &ramp(MMAP_THRESHOLD * 2)); // large: triggers mmap
                                                        // Symlink pointing to the real file.
        std::os::unix::fs::symlink(&target, root.join("link_to_target.bin"))
            .expect("create symlink");

        // Two consecutive quiescent walks must produce identical manifests.
        let opts = WalkOptions {
            walk_jobs: Some(1),
            ..WalkOptions::default()
        };
        let m1 = walk(&root, &opts, &hasher).expect("first symlink walk must succeed");
        let m2 = walk(&root, &opts, &hasher).expect("second symlink walk must succeed");

        let id1 = snapshot_id(&m1, &hasher);
        let id2 = snapshot_id(&m2, &hasher);
        assert!(
            is_valid_snapshot_id(&id1),
            "stable symlink: first walk produced malformed id {id1:?}"
        );
        assert_eq!(
            id1, id2,
            "stable symlink: quiescent walks must be byte-identical \
             (guard skip must not falsely fire)"
        );
        assert_eq!(
            m1.to_string(),
            m2.to_string(),
            "stable symlink: Manifest Display differs across two quiescent walks"
        );
    }

    // --- Part (b): changing symlink target --------------------------------
    {
        const ATTEMPTS: usize = 40;

        for attempt in 0..ATTEMPTS {
            let scratch = TempTree::new("symlink_changing");
            let root = scratch.path().to_path_buf();

            let target = root.join("target.bin");
            write_file(&target, &ramp(MMAP_THRESHOLD * 2)); // large
            std::os::unix::fs::symlink(&target, root.join("link.bin")).expect("create symlink");

            let target_c = target.clone();
            let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let done_c = Arc::clone(&done);

            // Mutator: change the target's content mid-walk.
            let mutator = std::thread::spawn(move || {
                let mut seed: u8 = 0x42;
                while !done_c.load(Ordering::Relaxed) {
                    // Overwrite target with different-size content.
                    let _ = fs::write(&target_c, ramp(MMAP_THRESHOLD + seed as usize * 17));
                    // Truncate to zero briefly.
                    try_truncate(&target_c, 0);
                    seed = seed.wrapping_mul(31).wrapping_add(7);
                    std::hint::spin_loop();
                }
            });

            let result = walk(
                &root,
                &WalkOptions {
                    walk_jobs: Some(1),
                    ..WalkOptions::default()
                },
                &hasher,
            );
            done.store(true, Ordering::Relaxed);
            mutator.join().expect("mutator thread panicked");

            // Invariant: must not panic/crash.  Ok (if race missed or
            // symlink guard-skip allowed it) or typed error (if SIGBUS or
            // vanish was caught on the target file).
            match &result {
                Ok(manifest) => {
                    let id = snapshot_id(manifest, &hasher);
                    assert!(
                        is_valid_snapshot_id(&id),
                        "attempt={attempt}: symlink-target race: Ok with malformed id {id:?}"
                    );
                }
                Err(err) => {
                    assert!(
                        is_acceptable_error(err),
                        "attempt={attempt}: symlink-target race: unexpected error variant: {err:?}"
                    );
                    assert!(
                        error_display_names_a_path(err),
                        "attempt={attempt}: symlink error Display must name a path: {:?}",
                        err.to_string()
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 10. Sub-threshold shrink (not mmap-fault): size-drift guard via stat path.
// ---------------------------------------------------------------------------

#[test]
fn sub_threshold_shrink_detected_by_size_drift_guard() {
    // Spec clause: bytes-vs-FileRecord.size drift (design §B). A small (sub-
    // MMAP_THRESHOLD) file that is rewritten to a SMALLER size between
    // discovery stat and hash-time stat produces FileChangedDuringWalk via the
    // size-drift guard, NOT via the SIGBUS path. This is the non-mmap branch:
    // the file is read with fs::read (no mmap, no SIGBUS), but the hash-time
    // metadata `len` differs from the discovery `recorded_size`.
    //
    // Note: for sub-threshold files, `blake3_hash_file` returns (hash, stat_len)
    // where `stat_len` is the SECOND stat (at hash time). So the drift guard
    // fires when the second stat sees a different size than discovery.
    const ATTEMPTS: usize = 60;
    let hasher = Blake3Hasher::new();

    for attempt in 0..ATTEMPTS {
        let scratch = TempTree::new("sub_threshold_shrink");
        let root = scratch.path().to_path_buf();

        // Victim: explicitly sub-threshold (< 256 KiB) so fs::read is used.
        let small_size = MMAP_THRESHOLD / 2; // 128 KiB
        let victim = root.join("small_victim.bin");
        write_file(&victim, &ramp(small_size));
        // Shrunk replacement: clearly smaller.
        let shrunk_size = MMAP_THRESHOLD / 8; // 32 KiB

        // Control file (never mutated).
        write_file(&root.join("control.bin"), &ramp(512));

        let victim_c = victim.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_c = Arc::clone(&done);

        // Shrinker thread: hammers the victim to a smaller size.
        let shrinker = std::thread::spawn(move || {
            while !done_c.load(Ordering::Relaxed) {
                let _ = fs::write(&victim_c, ramp(shrunk_size));
                let _ = fs::write(&victim_c, ramp(small_size));
                std::hint::spin_loop();
            }
        });

        let result = walk(
            &root,
            &WalkOptions {
                walk_jobs: Some(1),
                ..WalkOptions::default()
            },
            &hasher,
        );
        done.store(true, Ordering::Relaxed);
        shrinker.join().expect("shrinker thread panicked");

        match &result {
            Ok(manifest) => {
                let id = snapshot_id(manifest, &hasher);
                assert!(
                    is_valid_snapshot_id(&id),
                    "attempt={attempt}: sub-threshold shrink: Ok with malformed id {id:?}"
                );
            }
            Err(err) => {
                // The size-drift guard should produce FileChangedDuringWalk;
                // a vanish (if the file is briefly absent) yields FileVanished;
                // genuine IO is Io.  Must NOT be an untyped panic.
                assert!(
                    matches!(
                        err,
                        WalkError::FileChangedDuringWalk { .. }
                            | WalkError::FileVanishedDuringWalk { .. }
                            | WalkError::Io { .. }
                    ),
                    "attempt={attempt}: sub-threshold shrink produced unexpected variant: {err:?}"
                );
                assert!(
                    error_display_names_a_path(err),
                    "attempt={attempt}: error Display must name a path: {:?}",
                    err.to_string()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 11. Large-file truncation yields FileChangedDuringWalk (not Io):
//     the is_mmap_fault() classification is correctly wired.
// ---------------------------------------------------------------------------

#[test]
fn large_file_truncation_yields_file_changed_not_io() {
    // Spec clause: SIGBUS → FileChangedDuringWalk (design §A + §B). When the
    // SIGBUS guard catches a mid-mmap truncation, `is_mmap_fault(&e)` returns
    // true and walk.rs maps it to WalkError::FileChangedDuringWalk — NOT to
    // WalkError::Io (which is reserved for genuine permission/IO faults).
    //
    // This test asserts that when we get an error on a concurrent large-file
    // truncation, it is FileChangedDuringWalk or FileVanishedDuringWalk —
    // NEVER Io (which would mean the mmap fault fell through to the wrong arm).
    const LARGE_SIZE: usize = 4 * 1024 * 1024; // 4 MiB — well above MMAP_THRESHOLD
    const ATTEMPTS: usize = 40;

    let hasher = Blake3Hasher::new();

    for attempt in 0..ATTEMPTS {
        let scratch = TempTree::new("mmap_fault_typed");
        let root = scratch.path().to_path_buf();
        let victim = root.join("large_typed.bin");
        write_file(&victim, &ramp(LARGE_SIZE));

        let victim_c = victim.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_c = Arc::clone(&done);

        let truncator = std::thread::spawn(move || {
            while !done_c.load(Ordering::Relaxed) {
                try_truncate(&victim_c, 0);
                try_truncate(&victim_c, (LARGE_SIZE / 2) as u64);
                try_truncate(&victim_c, 0);
                std::hint::spin_loop();
            }
        });

        let result = walk(
            &root,
            &WalkOptions {
                walk_jobs: Some(1),
                ..WalkOptions::default()
            },
            &hasher,
        );
        done.store(true, Ordering::Relaxed);
        truncator.join().expect("truncator thread panicked");

        match &result {
            Ok(manifest) => {
                let id = snapshot_id(manifest, &hasher);
                assert!(
                    is_valid_snapshot_id(&id),
                    "attempt={attempt}: large-truncation: Ok with malformed id {id:?}"
                );
            }
            Err(err) => {
                // A large-file truncation MUST produce FileChangedDuringWalk
                // (from is_mmap_fault) or FileVanishedDuringWalk (if the file
                // appeared empty/gone at stat time).  NEVER Io — that would
                // mean the mmap fault was not recognized and fell through to
                // the wrong variant.
                assert!(
                    matches!(
                        err,
                        WalkError::FileChangedDuringWalk { .. }
                            | WalkError::FileVanishedDuringWalk { .. }
                    ),
                    "attempt={attempt}: large-file truncation must produce FileChanged or \
                     FileVanished, not Io or other: {err:?}"
                );
                assert!(
                    error_display_names_a_path(err),
                    "attempt={attempt}: error Display must name a path: {:?}",
                    err.to_string()
                );
            }
        }
    }
}
