//! KEYSTONE DURABILITY SUITE for the exact-mirror (`--delete`) + materialization
//! modes feature (Phase 32, gate `mirror-durability-review`).
//!
//! This is a CONSOLIDATED keystone gate, NOT a spec-tests/impl/review triple: the
//! whole feature is BUILT and per-cluster verified. This suite pins the single
//! durability promise the operator named — a long-running process that is holding
//! a destination file OPEN must keep working through a mirror — across EVERY
//! materialization mode, plus the no-shared-store-corruption invariants. Every
//! test here is EXPECTED TO PASS against the shipped code. If a keystone assertion
//! fails, that is a REAL BUG: it is left failing and reported for the PM to reopen
//! the offending lane. No assertion is ever weakened to go green.
//!
//! ## The durability promise, restated
//! POSIX binds an open fd to the underlying INODE, so the original bytes survive
//! an unlink / rename / atomic swap of the path UNTIL the fd closes. The five
//! invariants pinned below:
//!
//!   1. HELD-OPEN FD SURVIVES `--delete` REMOVAL (in-place path), DEFAULT + LINKED.
//!      A dest file is opened, then a `checkout --delete <OTHER snapshot>` prunes
//!      or replaces it in place; the still-open fd keeps reading the ORIGINAL
//!      bytes (inode retention). Driven through the CLI (`assert_cmd`), which uses
//!      the in-place prune path.
//!   2. HELD-OPEN FD SURVIVES THE ATOMIC STAGED SWAP, ALL ZERO-COPY MODES. The
//!      atomic swap is a stores primitive (`FileStore::fetch_files_atomic`); the
//!      CLI does not route through it yet, so this calls the stores API directly:
//!      open a dest file, swap a DIFFERENT snapshot into the same dest (Auto on a
//!      CoW host + Linked on any FS); the held fd reads ORIGINAL bytes, a fresh
//!      open of the path reads NEW content.
//!   3. SYMLINK-MODE WRITE IS BLOCKED (no shared-store corruption). A `--linked`
//!      dest file is a symlink to a `0444` store object; writing THROUGH it fails
//!      (PermissionDenied) and the store object's bytes stay byte-identical and
//!      still BLAKE3-verify.
//!   4. REFLINK-MODE WRITE LEAVES THE SOURCE OBJECT BYTE-IDENTICAL (CoW
//!      independence). In default mode, editing a materialized dest file does NOT
//!      change the source store/cache object (the write breaks CoW). Gated on CoW
//!      capability; the no-corruption invariant is asserted on EVERY host.
//!   5. ZERO EXTRA BYTE COPIES on the zero-copy paths. reflink (CoW) copies no
//!      file bytes — `clonefile_hits()` advances; symlinks are links, not copies.
//!
//! ## CoW gating
//! macOS dev is APFS (clone fires); Linux CI runs an ext4 leg (NO reflink) and a
//! Btrfs leg (real FICLONE). The "reflink actually fired" assertions are GATED on
//! a clone-capable host (macOS always; Linux only under `SNAPDIR_REFLINK_TEST_DIR`,
//! mirroring `crates/snapdir-stores/tests/reflink.rs`). The held-fd survival, the
//! symlink-write-blocked, and the no-corruption invariants are asserted on EVERY
//! host — symlink (`Linked`) staging is zero-copy on any filesystem, so those legs
//! run unconditionally.
//!
//! ## Mixed harness
//! Invariants 1, 3, 4, 5(symlink) drive the shipped CLI via `assert_cmd` (the
//! in-place `--delete` / `--linked` paths). Invariant 2 and 5(reflink) call
//! `snapdir_stores` directly (`fetch_files_atomic` / `clonefile_hits`). Both live
//! in this one file (the test crate depends on `snapdir-stores` + `snapdir-core`).

#![cfg(unix)]
// Wiring (shape only, no assertion change): silence the workspace `-D warnings`
// pedantic clippy lints on this adversary-authored suite, exactly as the sibling
// `mirror_*` suites do. `doc_markdown` fires on the prose invariant docstrings
// (method/path names without backticks); `cast_possible_truncation` on the
// `len as u32` deterministic-content generators. Pure shape; no test logic
// affected.
#![allow(clippy::doc_markdown, clippy::cast_possible_truncation)]

use std::collections::HashSet;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use assert_cmd::prelude::*;

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{object_path, Store};
use snapdir_stores::{clonefile_hits, FileStore, MaterializeMode, StreamStore};

// ===========================================================================
// Shared scaffolding (no dev-deps beyond assert_cmd; mirrors the sibling CLI +
// stores suites so the harness is familiar and the cleanup is hardened-tree-safe).
// ===========================================================================

/// Unique temp dir under `parent`, removed (with the 0444 tree un-hardened first)
/// by the caller via [`cleanup`]. A global counter keeps names unique across the
/// process so parallel `#[test]`s never collide.
fn temp_under(parent: &Path, tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut dir = parent.to_path_buf();
    dir.push(format!(
        "snapdir-durability-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Unique temp dir under the OS temp root.
fn temp_dir(tag: &str) -> PathBuf {
    temp_under(&std::env::temp_dir(), tag)
}

/// Recursively restore `u+w` so a hardened (`0444`) linked tree / store can be
/// removed; never chases a symlink during teardown.
fn restore_writable(root: &Path) {
    if let Ok(md) = fs::symlink_metadata(root) {
        if md.file_type().is_symlink() {
            return;
        }
        let mut perms = md.permissions();
        let mode = perms.mode();
        perms.set_mode(mode | 0o700);
        let _ = fs::set_permissions(root, perms);
        if md.is_dir() {
            if let Ok(rd) = fs::read_dir(root) {
                for e in rd.flatten() {
                    restore_writable(&e.path());
                }
            }
        }
    }
}

/// Best-effort recursive cleanup of test scratch dirs (un-hardening first so a
/// `0444` linked store / dest tears down cleanly).
fn cleanup(dirs: &[&Path]) {
    for d in dirs {
        restore_writable(d);
        let _ = fs::remove_dir_all(d);
    }
}

/// Process-global lock guarding `SNAPDIR_CLONEFILE` + the process-global
/// `clonefile_hits()` counter. Any test that toggles the knob or compares a
/// counter delta MUST hold this for its whole body. Mirrors the stores suites.
fn env_lock() -> MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII guard that sets/clears `SNAPDIR_CLONEFILE` and restores the prior value
/// on drop. The caller must already hold [`env_lock`].
struct CloneEnv {
    prev: Option<String>,
}

impl CloneEnv {
    fn set(value: Option<&str>) -> Self {
        let prev = std::env::var("SNAPDIR_CLONEFILE").ok();
        match value {
            Some(v) => std::env::set_var("SNAPDIR_CLONEFILE", v),
            None => std::env::remove_var("SNAPDIR_CLONEFILE"),
        }
        Self { prev }
    }
}

impl Drop for CloneEnv {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("SNAPDIR_CLONEFILE", v),
            None => std::env::remove_var("SNAPDIR_CLONEFILE"),
        }
    }
}

// --- CLI harness (assert_cmd) ----------------------------------------------

/// A fresh `snapdir` command with the cache pinned and the dev env scrubbed:
/// `SNAPDIR_STORE`/`SNAPDIR_OBJECTS_STORE` removed so leakage can't mask a bug,
/// `HOME`/`XDG_CACHE_HOME` redirected inside the sandbox. Mirrors the sibling
/// CLI suites exactly.
fn snapdir(cache: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env("HOME", home);
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
    cmd.env_remove("SNAPDIR_STORE");
    cmd.env_remove("SNAPDIR_OBJECTS_STORE");
    cmd.env_remove("SNAPDIR_VERIFY_COPIES");
    cmd.env_remove("SNAPDIR_MANIFEST_CONTEXT");
    cmd
}

/// Runs `snapdir <args>`, asserts success, returns trimmed stdout.
fn ok_stdout(mut cmd: Command, args: &[&str]) -> String {
    let out = cmd.args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// Pushes a directory tree into a fresh `file://` store via the CLI, returning
/// `(store_url, id)`. Used by the in-place `--delete` / `--linked` legs so the
/// path under test is the SHIPPED router, not a stores back door.
fn cli_push(src: &Path, cache: &Path, home: &Path, store: &Path) -> (String, String) {
    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let id = ok_stdout(
        snapdir(cache, home),
        &["push", "--store", &store_url, &src_str],
    );
    assert_eq!(id.len(), 64, "push must print a 64-hex id");
    (store_url, id)
}

/// Writes one file `rel` with `content` + octal `mode` under `src` (creating
/// parents). The source dir itself is left at a stable `0755`.
fn write_file(src: &Path, rel: &str, content: &[u8], mode: u32) {
    let target = src.join(rel);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&target, content).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
}

// --- stores harness (direct API) -------------------------------------------

/// Writes a real source tree under `src` and returns the matching `Manifest`
/// plus its snapshot id, content-addressed under the NON-keyed `Blake3Hasher`
/// the file store files objects under. Mirrors the stores suites' `build_tree`.
fn build_tree(src: &Path, files: &[(&str, &[u8], &str)]) -> (Manifest, String) {
    let hasher = Blake3Hasher::new();
    let mut manifest = Manifest::new();

    let mut file_sums: Vec<String> = Vec::new();
    for (rel, content, mode) in files {
        let target = src.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&target, content).unwrap();
        let perms = fs::Permissions::from_mode(u32::from_str_radix(mode, 8).unwrap());
        fs::set_permissions(&target, perms).unwrap();
        let sum = hasher.hash_hex(content);
        file_sums.push(sum.clone());
        manifest.push(ManifestEntry::new(
            PathType::File,
            *mode,
            sum,
            content.len() as u64,
            format!("./{rel}"),
        ));
    }

    let root_sum = directory_checksum(file_sums.iter().map(String::as_str), &hasher);
    let root_size: u64 = files.iter().map(|(_, c, _)| c.len() as u64).sum();
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

/// On-disk sharded path of an object blob under `root`.
fn object_disk(root: &Path, checksum: &str) -> PathBuf {
    root.join(object_path(checksum))
}

/// Counts the regular files under `<root>/.objects`.
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

/// Honors the same env contract `reflink.rs` uses: `SNAPDIR_REFLINK_TEST_DIR`
/// set ⇒ `Some` (place src + store + dest under it so a clone can fire), unset ⇒
/// `None`.
fn reflink_root() -> Option<PathBuf> {
    match std::env::var("SNAPDIR_REFLINK_TEST_DIR") {
        Ok(dir) if !dir.is_empty() => {
            let p = PathBuf::from(dir);
            p.is_dir().then_some(p)
        }
        _ => None,
    }
}

/// macOS (APFS) is always clone-capable; Linux only under a reflink root.
fn reflink_capable() -> bool {
    cfg!(target_os = "macos") || reflink_root().is_some()
}

/// The parent under which to co-locate src + store + dest so a clone can fire.
fn coloc_parent() -> PathBuf {
    reflink_root().unwrap_or_else(std::env::temp_dir)
}

// ###########################################################################
// INVARIANT 1 — HELD-OPEN FD SURVIVES `--delete` REMOVAL (in-place path), ALL
// MODES. Driven through the SHIPPED CLI (`assert_cmd`), which prunes in place via
// `snapdir_core::mirror::prune_set` + remove. A process holds a dest file open;
// a `checkout --delete <OTHER snapshot>` then removes/replaces that path; the
// still-open fd must keep reading the ORIGINAL bytes (POSIX inode retention).
// ###########################################################################

/// Builds two single-file snapshots V1/V2 (same dest path `data.bin`, different
/// content) into one `file://` store and returns
/// `(store_url, id1, id2, v1, v2, store, cache, home)`. The caller owns the temp
/// roots and cleans them up.
#[allow(clippy::type_complexity)]
fn two_snapshots_one_path(
    tag: &str,
) -> (
    String,
    String,
    String,
    Vec<u8>,
    Vec<u8>,
    PathBuf,
    PathBuf,
    PathBuf,
) {
    let store = temp_dir(&format!("{tag}-store"));
    let cache = temp_dir(&format!("{tag}-cache"));
    let home = temp_dir(&format!("{tag}-home"));

    // >64 KiB so the read can be split across the prune (head before, tail after).
    let v1: Vec<u8> = (0..(96 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let v2: Vec<u8> = (0..(96 * 1024u32))
        .map(|i| ((i * 7 + 3) % 251) as u8)
        .collect();
    assert_ne!(
        v1, v2,
        "V1 and V2 must differ for the test to be meaningful"
    );

    let src1 = temp_dir(&format!("{tag}-src1"));
    fs::set_permissions(&src1, fs::Permissions::from_mode(0o755)).unwrap();
    write_file(&src1, "data.bin", &v1, 0o644);
    let (store_url, id1) = cli_push(&src1, &cache, &home, &store);

    let src2 = temp_dir(&format!("{tag}-src2"));
    fs::set_permissions(&src2, fs::Permissions::from_mode(0o755)).unwrap();
    // A DIFFERENT layout: data.bin replaced + an EXTRA file that --delete-into-V2
    // would prune nothing of, but the V1->V2 swap replaces data.bin's content.
    write_file(&src2, "data.bin", &v2, 0o644);
    let (_u2, id2) = cli_push(&src2, &cache, &home, &store);

    let _ = fs::remove_dir_all(&src1);
    let _ = fs::remove_dir_all(&src2);
    (store_url, id1, id2, v1, v2, store, cache, home)
}

#[test]
fn held_open_fd_survives_in_place_delete_default_mode_cli() {
    // INVARIANT 1 (default/reflink-or-copy): a dest file opened before a CLI
    // `checkout --delete <V2>` keeps reading the ORIGINAL (V1) inode bytes after
    // V2 is materialized in place over the same path. POSIX: open() binds the fd
    // to the V1 inode; the in-place replace (prune + re-materialize) unlinks the
    // old name but the inode survives until the fd closes.
    let (store_url, id1, id2, v1, v2, store, cache, home) =
        two_snapshots_one_path("held-delete-default");
    let dest = temp_dir("held-delete-default-dest");
    let dest_str = dest.to_string_lossy().into_owned();

    // Lay V1 into the dest (default/Auto materialize) via the shipped CLI.
    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id1, &dest_str],
    );
    let data_path = dest.join("data.bin");
    assert_eq!(fs::read(&data_path).unwrap(), v1, "dest must hold V1 first");

    // Open the V1 file and read the FIRST half BEFORE the mutation (binding the
    // fd to the V1 inode). Auto mode is a real independent inode, so open() lands
    // directly on it.
    let mut fd = fs::File::open(&data_path).expect("open dest data.bin before --delete");
    let mut head = vec![0u8; v1.len() / 2];
    fd.read_exact(&mut head)
        .expect("read V1 head before mutate");
    assert_eq!(
        head,
        &v1[..v1.len() / 2],
        "head read before mutate must be V1"
    );

    // Now swap V2 into the SAME dest via `pull --delete` (fetch V2 into the cache
    // then the in-place prune + re-materialize path; `checkout` alone reads only
    // the local cache and would not have V2's manifest). --delete makes the dest
    // an exact mirror of V2.
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id2, "--delete", &dest_str,
        ],
    );

    // The PATH now resolves to V2 ...
    assert_eq!(
        fs::read(&data_path).expect("read dest path after --delete"),
        v2,
        "after checkout --delete the dest PATH must resolve to V2 content"
    );

    // ... but the still-open fd keeps reading the ORIGINAL (V1) bytes.
    let mut tail = vec![0u8; v1.len() - v1.len() / 2];
    fd.read_exact(&mut tail)
        .expect("the held-open fd must keep reading V1 bytes across the in-place --delete");
    assert_eq!(
        tail,
        &v1[v1.len() / 2..],
        "KEYSTONE (default/in-place): the fd opened before `checkout --delete` MUST keep \
         reading the ORIGINAL (V1) inode bytes — long-running processes are never disrupted"
    );

    cleanup(&[&store, &cache, &home, &dest]);
}

#[test]
fn held_open_fd_survives_in_place_delete_linked_mode_cli() {
    // INVARIANT 1 (--linked/symlink): a dest file opened before a CLI
    // `checkout --linked --delete <V2>` keeps reading the ORIGINAL (V1) bytes.
    // The V1 dest entry is a symlink to the V1 0444 object; open() follows it and
    // binds the fd to the V1 OBJECT inode. The in-place replace re-points the
    // dest symlink at the V2 object, but the V1 object inode survives behind the
    // held fd.
    let (store_url, id1, id2, v1, v2, store, cache, home) =
        two_snapshots_one_path("held-delete-linked");
    let dest = temp_dir("held-delete-linked-dest");
    let dest_str = dest.to_string_lossy().into_owned();

    // Linked pull = fetch (prime the cache with manifest + objects so the symlinks
    // resolve) + linked checkout.
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id1, "--linked", &dest_str,
        ],
    );
    let data_path = dest.join("data.bin");
    assert!(
        data_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "linked V1 dest entry must be a symlink"
    );
    assert_eq!(
        fs::read(&data_path).unwrap(),
        v1,
        "linked dest must read V1 first"
    );

    // Open the V1 link (open() follows the symlink to the V1 object inode) and
    // read the first half before the mutation.
    let mut fd = fs::File::open(&data_path).expect("open linked dest before --delete");
    let mut head = vec![0u8; v1.len() / 2];
    fd.read_exact(&mut head)
        .expect("read V1 head before mutate");
    assert_eq!(
        head,
        &v1[..v1.len() / 2],
        "linked head before mutate must be V1"
    );

    // Swap V2 in via `pull --linked --delete` (fetch V2 into the cache, then
    // re-point the dest symlink at the V2 object; `checkout` alone reads only the
    // local cache and would not have V2's manifest).
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id2, "--linked", "--delete", &dest_str,
        ],
    );

    // The PATH now resolves to the V2 object ...
    assert!(
        data_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "after the linked --delete the dest entry must still be a symlink"
    );
    assert_eq!(
        fs::read(&data_path).expect("read linked dest path after --delete"),
        v2,
        "after checkout --linked --delete the dest PATH must resolve to V2 content"
    );

    // ... but the still-open fd keeps reading the ORIGINAL (V1) object bytes.
    let mut tail = vec![0u8; v1.len() - v1.len() / 2];
    fd.read_exact(&mut tail)
        .expect("the held-open fd must keep reading V1 object bytes across the linked --delete");
    assert_eq!(
        tail,
        &v1[v1.len() / 2..],
        "KEYSTONE (linked/in-place): the fd opened before `checkout --linked --delete` MUST keep \
         reading the ORIGINAL (V1) OBJECT inode bytes even as the dest symlink is re-pointed at V2"
    );

    cleanup(&[&store, &cache, &home, &dest]);
}

// ###########################################################################
// INVARIANT 2 — HELD-OPEN FD SURVIVES THE ATOMIC STAGED SWAP, ALL ZERO-COPY
// MODES. The atomic swap is the stores primitive `fetch_files_atomic` (the CLI
// does not route through it yet); we call it directly. Open a dest file, swap a
// DIFFERENT snapshot into the same dest; the held fd reads ORIGINAL bytes, a
// fresh open reads NEW content. Linked runs on ANY FS; Auto is gated on CoW.
// ###########################################################################

/// Builds V1/V2 single-file snapshots (path `data.bin`, differing content) into
/// ONE `FileStore` under `parent`, returning `(store, store_dir, m1, v1, m2, v2)`.
#[allow(clippy::type_complexity)]
fn store_two_snapshots(
    parent: &Path,
    tag: &str,
    len: usize,
) -> (FileStore, PathBuf, Manifest, Vec<u8>, Manifest, Vec<u8>) {
    let v1: Vec<u8> = (0..len as u32).map(|i| (i % 251) as u8).collect();
    let v2: Vec<u8> = (0..len as u32)
        .map(|i| ((i * 11 + 5) % 251) as u8)
        .collect();
    assert_ne!(v1, v2);

    let store_dir = temp_under(parent, &format!("{tag}-store"));
    let store = FileStore::from_root(store_dir.clone());

    let src1 = temp_under(parent, &format!("{tag}-src1"));
    let (m1, _i1) = build_tree(&src1, &[("data.bin", v1.as_slice(), "644")]);
    store.push(&m1, &src1).expect("push v1");

    let src2 = temp_under(parent, &format!("{tag}-src2"));
    let (m2, _i2) = build_tree(&src2, &[("data.bin", v2.as_slice(), "644")]);
    store.push(&m2, &src2).expect("push v2");

    let _ = fs::remove_dir_all(&src1);
    let _ = fs::remove_dir_all(&src2);
    (store, store_dir, m1, v1, m2, v2)
}

#[test]
fn held_open_fd_survives_atomic_swap_linked_mode() {
    // INVARIANT 2 (atomic swap, Linked): an fd opened on a dest file BEFORE
    // `fetch_files_atomic(.., Linked)` swaps a DIFFERENT snapshot into the same
    // dest keeps reading the ORIGINAL bytes (the old tree is renamed aside; the
    // object inode survives). Linked is zero-copy on any FS, so this runs
    // unconditionally.
    let parent = coloc_parent();
    let (store, store_dir, m1, v1, m2, v2) =
        store_two_snapshots(&parent, "atomic-linked", 80 * 1024);
    let dest = temp_under(&parent, "atomic-linked-dest");

    store
        .fetch_files_atomic(&m1, &dest, MaterializeMode::Linked)
        .expect("initial atomic swap (V1, Linked) must succeed");
    let data_path = dest.join("data.bin");

    // Open V1 + read the first half before the swap (open() follows the link to
    // the V1 object inode).
    let mut fd = fs::File::open(&data_path).expect("open dest before swap");
    let mut head = vec![0u8; v1.len() / 2];
    fd.read_exact(&mut head).expect("read V1 head before swap");
    assert_eq!(head, &v1[..v1.len() / 2]);

    store
        .fetch_files_atomic(&m2, &dest, MaterializeMode::Linked)
        .expect("atomic swap (V2, Linked) over the held-open dest must succeed");

    // A FRESH open of the path resolves to V2 ...
    assert_eq!(
        fs::read(&data_path).expect("fresh read after swap"),
        v2,
        "after the atomic swap a fresh open of the dest PATH must resolve to V2 content"
    );
    // ... the held fd keeps reading the ORIGINAL (V1) object bytes.
    let mut tail = vec![0u8; v1.len() - v1.len() / 2];
    fd.read_exact(&mut tail)
        .expect("the held-open fd must keep reading V1 bytes across the Linked atomic swap");
    assert_eq!(
        tail,
        &v1[v1.len() / 2..],
        "KEYSTONE (atomic/Linked): the fd opened before the atomic swap MUST keep reading the \
         ORIGINAL (V1) object inode bytes across the swap"
    );

    cleanup(&[&store_dir, &dest]);
}

#[test]
fn held_open_fd_survives_atomic_swap_auto_cow() {
    // INVARIANT 2 (atomic swap, Auto/reflink): same held-fd durability on the
    // editable Auto (reflink) path. GATED on a clone-capable host — Auto atomic
    // on a non-CoW target is a hard error (zero-copy-only), so the swap could not
    // run there anyway.
    if !reflink_capable() {
        eprintln!(
            "SKIP held_open_fd_survives_atomic_swap_auto_cow: no clone-capable FS \
             (Auto atomic requires zero-copy/CoW; set SNAPDIR_REFLINK_TEST_DIR on Linux)"
        );
        return;
    }

    let parent = coloc_parent();

    let _g = env_lock();
    let _e = CloneEnv::set(None); // reflink enabled where supported

    let (store, store_dir, m1, v1, m2, v2) = store_two_snapshots(&parent, "atomic-auto", 80 * 1024);
    let dest = temp_under(&parent, "atomic-auto-dest");

    store
        .fetch_files_atomic(&m1, &dest, MaterializeMode::Auto)
        .expect("initial atomic swap (V1, Auto) must succeed on a CoW host");
    let data_path = dest.join("data.bin");
    // Auto entries are real independent inodes (not symlinks).
    assert!(
        data_path.symlink_metadata().unwrap().file_type().is_file(),
        "atomic Auto dest entry must be a regular file (editable), not a symlink"
    );

    let mut fd = fs::File::open(&data_path).expect("open dest before swap");
    let mut head = vec![0u8; v1.len() / 2];
    fd.read_exact(&mut head).expect("read V1 head before swap");
    assert_eq!(head, &v1[..v1.len() / 2]);

    store
        .fetch_files_atomic(&m2, &dest, MaterializeMode::Auto)
        .expect("atomic swap (V2, Auto) over the held-open dest must succeed");

    assert_eq!(
        fs::read(&data_path).expect("fresh read after swap"),
        v2,
        "after the Auto atomic swap a fresh open must resolve to V2 content"
    );
    let mut tail = vec![0u8; v1.len() - v1.len() / 2];
    fd.read_exact(&mut tail)
        .expect("the held-open fd must keep reading V1 bytes across the Auto atomic swap");
    assert_eq!(
        tail,
        &v1[v1.len() / 2..],
        "KEYSTONE (atomic/Auto): the fd opened before the atomic swap MUST keep reading the \
         ORIGINAL (V1) inode bytes across the reflink swap"
    );

    cleanup(&[&store_dir, &dest]);
}

// ###########################################################################
// INVARIANT 3 — SYMLINK-MODE WRITE IS BLOCKED (no shared-store corruption).
// A `--linked` dest file is a symlink to a `0444` store object; writing THROUGH
// it fails (PermissionDenied), and the store object stays byte-identical AND
// still BLAKE3-verifies. Driven through the shipped `pull --linked` CLI.
// ###########################################################################

#[test]
fn linked_write_through_is_blocked_and_object_stays_intact_and_verifies_cli() {
    // INVARIANT 3: a `pull --linked` dest entry is a symlink to a 0444 store
    // object. Writing THROUGH the link must FAIL with PermissionDenied, and the
    // underlying store object's bytes must stay byte-identical and still verify
    // (no shared-store corruption through the link).
    let content = b"shared-object-must-not-be-corruptible-through-the-link\n".to_vec();

    let store = temp_dir("linkwrite-store");
    let cache = temp_dir("linkwrite-cache");
    let home = temp_dir("linkwrite-home");
    let dest = temp_dir("linkwrite-dest");
    let dest_str = dest.to_string_lossy().into_owned();

    let src = temp_dir("linkwrite-src");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    write_file(&src, "doc.txt", &content, 0o644);
    let (store_url, id) = cli_push(&src, &cache, &home, &store);

    // Linked pull: prime the cache (manifest + objects) so the symlinks resolve.
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--linked", &dest_str,
        ],
    );

    let link = dest.join("doc.txt");
    assert!(
        link.symlink_metadata().unwrap().file_type().is_symlink(),
        "linked dest entry must be a symlink for the write-through to target the object"
    );

    // The object the link resolves to is read-only 0444.
    let target = fs::canonicalize(&link).expect("resolve linked target");
    let target_mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        target_mode, 0o444,
        "the shared linked object must be read-only 0444 (got {target_mode:o})"
    );

    // Writing THROUGH the link must FAIL with PermissionDenied (the 0444 object).
    let write_res = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&link)
        .and_then(|mut f| {
            use std::io::Write as _;
            f.write_all(b"CORRUPTION")
        });
    assert!(
        write_res.is_err(),
        "writing through a linked (0444) dest file MUST fail — the store object must not be \
         corruptible through the link; got Ok(())"
    );
    assert_eq!(
        write_res.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "the write-through failure must be a permission error (0444 object)"
    );

    // The shared object's bytes are UNCHANGED after the blocked write ...
    assert_eq!(
        fs::read(&target).expect("object still readable"),
        content,
        "the store object bytes must be byte-identical after the blocked write-through"
    );
    // ... and it still BLAKE3-verifies through the store's read-time backstop.
    let sum = Blake3Hasher::new().hash_hex(&content);
    let store_obj = FileStore::from_root(store.clone());
    assert_eq!(
        store_obj
            .get_object(&sum)
            .expect("object verifies via get_object"),
        content,
        "the store object must still BLAKE3-verify (get_object) after the blocked write-through"
    );

    cleanup(&[&store, &cache, &home, &dest, &src]);
}

// ###########################################################################
// INVARIANT 4 — REFLINK-MODE WRITE LEAVES THE SOURCE OBJECT BYTE-IDENTICAL (CoW
// independence). In default mode, editing a materialized dest file does NOT
// change the source store/cache object (the write breaks CoW). The reflink-fired
// assertion is gated on CoW capability; the no-corruption invariant is asserted
// on EVERY host. Driven through the shipped `pull` (default/Auto) CLI.
// ###########################################################################

#[test]
fn default_mode_dest_edit_leaves_source_object_byte_identical_cli() {
    // INVARIANT 4: a default-mode (Auto) `pull` materializes an INDEPENDENT,
    // EDITABLE dest inode (reflink-on-CoW else copy). Editing it (a CoW break on a
    // reflink FS) must leave BOTH the store object AND the primed cache object
    // byte-identical — no shared-object corruption. A >256 KiB payload so a
    // reflink shares real extents and a CoW-break bug would surface.
    let content: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();

    // Co-locate store/cache/dest so a clone can actually fire where supported.
    let parent = coloc_parent();
    let store = temp_under(&parent, "cowedit-store");
    let cache = temp_under(&parent, "cowedit-cache");
    let home = temp_under(&parent, "cowedit-home");
    let dest = temp_under(&parent, "cowedit-dest");
    let dest_str = dest.to_string_lossy().into_owned();

    let src = temp_under(&parent, "cowedit-src");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    write_file(&src, "editable.bin", &content, 0o644);

    let _g = env_lock();
    let _e = CloneEnv::set(None); // reflink enabled where supported

    let (store_url, id) = cli_push(&src, &cache, &home, &store);

    // Default-mode pull (no --linked): the dest file is a real editable inode
    // (a reflink clone on a CoW host, else a plain copy — INVARIANT 5a proves the
    // reflink path itself fires via the stores primitive; here we pin the
    // no-corruption invariant which holds on either path).
    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );

    let dest_file = dest.join("editable.bin");
    let dmeta = dest_file.symlink_metadata().expect("dest file metadata");
    assert!(
        dmeta.file_type().is_file(),
        "default-mode dest entry must be a regular editable file, never a symlink"
    );

    let sum = Blake3Hasher::new().hash_hex(&content);
    let store_object = object_disk(&store, &sum);
    // The dest file is an INDEPENDENT inode from the store object.
    let ometa = store_object
        .symlink_metadata()
        .expect("store object metadata");
    assert_ne!(
        (dmeta.dev(), dmeta.ino()),
        (ometa.dev(), ometa.ino()),
        "default-mode dest file must be an INDEPENDENT inode, not the store object itself"
    );

    // EDIT the dest file (a CoW break on a reflink FS): writing must SUCCEED
    // (it is the editable mode, manifest 0644).
    let mut edited = content.clone();
    edited[0] ^= 0xff;
    edited[content.len() / 2] ^= 0xff;
    edited[content.len() - 1] ^= 0xff;
    fs::write(&dest_file, &edited).expect("default-mode dest file must be editable/writable");

    // KEYSTONE: the SOURCE store object's bytes are UNCHANGED after the edit ...
    assert_eq!(
        fs::read(&store_object).expect("store object still readable"),
        content,
        "editing the reflinked/copied dest file must NOT change the source STORE object bytes \
         (CoW break / independent inode)"
    );
    let store_handle = FileStore::from_root(store.clone());
    assert_eq!(
        store_handle
            .get_object(&sum)
            .expect("store object verifies"),
        content,
        "the store object must still BLAKE3-verify after the dest file was edited"
    );

    // ... and the CACHE object the pull primed is ALSO byte-identical (the cache
    // is the other shared content-addressed pool a CoW break could corrupt).
    let cache_object = object_disk(&cache, &sum);
    if cache_object.exists() {
        assert_eq!(
            fs::read(&cache_object).expect("cache object readable"),
            content,
            "editing the dest file must NOT change the primed CACHE object bytes either"
        );
    }

    // The edit actually landed on the independent dest inode.
    assert_eq!(
        fs::read(&dest_file).expect("read edited dest"),
        edited,
        "the edit must have landed on the independent dest inode"
    );

    cleanup(&[&store, &cache, &home, &dest, &src]);
}

// ###########################################################################
// INVARIANT 5 — ZERO EXTRA BYTE COPIES on the zero-copy paths.
//   (5a) reflink (CoW): `clonefile_hits()` advances — no file bytes duplicated.
//   (5b) symlink (--linked): dest entries are LINKS, not copies, and no object is
//        duplicated into the store/cache pool.
// 5a is gated on a clone-capable host; 5b runs on any FS.
// ###########################################################################

#[test]
fn reflink_materialize_copies_zero_bytes_clonefile_hits_advances() {
    // INVARIANT 5a: on a CoW host a default-mode atomic materialize REFLINKS the
    // staged files (CopyMethod::Cloned) — clonefile_hits() must advance, proving
    // ZERO bytes of the >256 KiB object were duplicated. GATED on a clone-capable
    // host (on a copy-only host there is no clone to count). Uses the stores
    // primitive directly so the counter delta is attributable to this swap alone
    // (under env_lock).
    if !reflink_capable() {
        eprintln!(
            "SKIP reflink_materialize_copies_zero_bytes_clonefile_hits_advances: \
             no clone-capable FS (set SNAPDIR_REFLINK_TEST_DIR on Linux)"
        );
        return;
    }

    let big: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let parent = coloc_parent();

    let _g = env_lock();
    let _e = CloneEnv::set(None);

    let store_dir = temp_under(&parent, "zerocopy-reflink-store");
    let store = FileStore::from_root(store_dir.clone());
    let src = temp_under(&parent, "zerocopy-reflink-src");
    let (manifest, _id) = build_tree(&src, &[("big.bin", big.as_slice(), "644")]);
    store.push(&manifest, &src).expect("push big object");

    let dest = temp_under(&parent, "zerocopy-reflink-dest");
    let before = clonefile_hits();
    store
        .fetch_files_atomic(&manifest, &dest, MaterializeMode::Auto)
        .expect("Auto atomic materialize on a CoW host must succeed");
    let after = clonefile_hits();

    assert!(
        after > before,
        "INVARIANT 5a: reflink materialization must clone (CopyMethod::Cloned), copying ZERO \
         file bytes — clonefile_hits() must advance: {before} -> {after}"
    );
    // The materialized dest entry is a real (independent) file holding the bytes.
    assert_eq!(
        fs::read(dest.join("big.bin")).expect("read materialized big.bin"),
        big,
        "the reflinked dest file must hold the source content"
    );
    // No object was duplicated into the store pool by the clone.
    assert_eq!(
        count_objects(&store_dir),
        1,
        "a reflink materialize must not duplicate objects into the store"
    );

    cleanup(&[&store_dir, &dest, &src]);
}

#[test]
fn linked_materialize_is_symlinks_not_copies_zero_byte_duplication_cli() {
    // INVARIANT 5b: a `pull --linked` materializes SYMLINKS (links, not byte
    // copies) into the local objects, on ANY filesystem. Every file entry is a
    // symlink resolving to the shared object, and NO object is duplicated into
    // the store or the primed cache (zero-copy). Driven through the shipped CLI.
    let store = temp_dir("zerocopy-linked-store");
    let cache = temp_dir("zerocopy-linked-cache");
    let home = temp_dir("zerocopy-linked-home");
    let dest = temp_dir("zerocopy-linked-dest");
    let dest_str = dest.to_string_lossy().into_owned();

    // A mixed tree: a >256 KiB object, a tiny file, a 0-byte file, a nested path.
    let big: Vec<u8> = (0..(300 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let files: Vec<(&str, Vec<u8>, u32)> = vec![
        ("big.bin", big, 0o644),
        ("tiny.txt", b"hello\n".to_vec(), 0o644),
        ("empty", Vec::new(), 0o644),
        (
            "nested/deep/leaf.bin",
            vec![0u8, 1, 2, 3, 255, 254, 0],
            0o600,
        ),
    ];

    let src = temp_dir("zerocopy-linked-src");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    for (rel, content, mode) in &files {
        write_file(&src, rel, content, *mode);
    }
    let (store_url, id) = cli_push(&src, &cache, &home, &store);

    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--linked", &dest_str,
        ],
    );

    let hasher = Blake3Hasher::new();
    let mut distinct: HashSet<String> = HashSet::new();
    for (rel, content, _mode) in &files {
        let link = dest.join(rel);
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "INVARIANT 5b: linked dest entry {rel} MUST be a symlink (zero-copy), not a fresh copy"
        );
        assert_eq!(
            &fs::read(&link).unwrap(),
            content,
            "reading through the linked entry {rel} must return the source content"
        );
        distinct.insert(hasher.hash_hex(content));
    }

    // ZERO byte duplication: the store pool holds exactly the distinct objects —
    // the links carry no object bytes of their own.
    assert_eq!(
        count_objects(&store),
        distinct.len(),
        "INVARIANT 5b: linked materialization is zero-copy — it must NOT duplicate objects into \
         the store ({} distinct expected)",
        distinct.len()
    );
    // The primed cache pool likewise holds at most the distinct objects (no
    // per-link byte copy leaked into the cache).
    assert!(
        count_objects(&cache) <= distinct.len(),
        "INVARIANT 5b: a linked pull must not duplicate object bytes into the cache pool either \
         (cache objects {} must be <= distinct {})",
        count_objects(&cache),
        distinct.len()
    );

    cleanup(&[&store, &cache, &home, &dest, &src]);
}
