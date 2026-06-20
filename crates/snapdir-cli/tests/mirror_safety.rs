//! ADVERSARY destructive-safety KEYSTONE suite for `checkout`/`pull --delete`
//! (Phase 32). This consolidates and goes DEEPER than `mirror_checkout_cli.rs`
//! on the ONE thing that matters most: `--delete` must NEVER destroy anything it
//! is not mandated to. The exact-mirror `--delete` feature is FULLY BUILT; these
//! tests must PASS against the shipped binary. If a safety assertion FAILS, that
//! is a REAL DESTRUCTIVE BUG — the failing test is left in place and the impl
//! gate (`mirror-checkout-cli-impl`) is reopened. NONE of these may be weakened
//! to go green.
//!
//! The destructive-safety invariants pinned (each `#[test]` names its invariant):
//!   1. NEVER delete anything OUTSIDE the dest root (the cardinal rule): a dest
//!      symlink escaping to an external file/dir is removed AS A LINK; the
//!      external target + its contents survive byte-identical (canaries).
//!   2. The escaping symlink IS removed (extraneous) but UNLINKED, not followed —
//!      the prune-set walk must not descend out of the dest.
//!   3. HARD-REFUSE dangerous dests with NO `--force` bypass, deleting nothing:
//!      `/`, `$HOME`, the cache dir(s), and a store path; canonicalization
//!      aliases (trailing `/`, `/.`, symlink-to-guarded) all refuse.
//!   4. `--exclude` is honored: a matching extraneous path is protected; a
//!      non-matching sibling is still pruned.
//!   5. Nothing deleted on refusal (all-or-nothing on the guard); `--dryrun
//!      --delete` deletes nothing while listing.
//!   6. Adversarial extras: a symlink LOOP doesn't hang/panic; a dest symlink to
//!      a guarded dir is removed as a link, not followed into it; a deeply nested
//!      extraneous tree is pruned without escaping.
//!
//! SAFETY OF THE SUITE ITSELF: every dangerous-dest run env-redirects
//! `HOME`/`XDG_CACHE_HOME`/`SNAPDIR_CACHE_DIR` to per-test tempdirs, so even a
//! buggy impl that ignored the guard could only ever touch a sandbox, never the
//! real `$HOME` or cache. The `/` case asserts ONLY the refusal (it never writes
//! to or targets real `/`). Canaries are placed liberally OUTSIDE every dest and
//! their byte-for-byte survival is asserted after the prune.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;

/// Unique temp dir under the OS temp root, removed by the caller on drop.
fn temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "snapdir-safety-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A fresh `snapdir` command with the cache pinned and the developer's env
/// scrubbed. `HOME`/`XDG_CACHE_HOME` are redirected at `home` so the default
/// cache / `$HOME` computations resolve INSIDE the sandbox (never the real
/// home), and `SNAPDIR_STORE`/`SNAPDIR_OBJECTS_STORE` are removed so leakage
/// cannot mask a bug.
fn snapdir(cache: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env("HOME", home);
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
    cmd.env_remove("SNAPDIR_STORE");
    cmd.env_remove("SNAPDIR_OBJECTS_STORE");
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

/// Build a tiny deterministic source tree (stable perms so a checkout
/// re-manifests to the same id) and return it.
fn build_src(tag: &str) -> PathBuf {
    let src = temp_dir(tag);
    fs::write(src.join("a.txt"), b"hello").unwrap();
    fs::set_permissions(src.join("a.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::create_dir(src.join("sub")).unwrap();
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(src.join("sub").join("b.txt"), b"world!!").unwrap();
    fs::set_permissions(
        src.join("sub").join("b.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    src
}

/// Push `src` into a fresh `file://` store and return `(store_url, id)`.
fn push_to_store(src: &Path, cache: &Path, home: &Path, store: &Path) -> (String, String) {
    let store_url = format!("file://{}", store.display());
    let src_str = src.to_string_lossy().into_owned();
    let id = ok_stdout(
        snapdir(cache, home),
        &["push", "--store", &store_url, &src_str],
    );
    assert_eq!(id.len(), 64, "push must print a 64-hex id");
    (store_url, id)
}

/// Materialize the snapshot into `dest` (plain checkout), then return its
/// dest-string. Used to set up an exact mirror that we then perturb.
fn pull_into(dest: &Path, store_url: &str, id: &str, cache: &Path, home: &Path) -> String {
    let dest_str = dest.to_string_lossy().into_owned();
    ok_stdout(
        snapdir(cache, home),
        &["pull", "--store", store_url, "--id", id, &dest_str],
    );
    dest_str
}

/// Best-effort recursive cleanup of test scratch dirs.
fn cleanup(dirs: &[&Path]) {
    for d in dirs {
        fs::remove_dir_all(d).ok();
    }
}

/// Recursively snapshot a tree as a sorted list of `(relpath, kind, bytes)`,
/// where kind is "f" (file, with bytes), "d" (dir), "l" (symlink, bytes = the
/// link target). Used to assert an OUTSIDE-the-dest canary tree is BYTE-IDENTICAL
/// before and after a prune.
fn snapshot_tree(root: &Path) -> Vec<(String, &'static str, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, &'static str, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let meta = fs::symlink_metadata(&path).unwrap();
            let ft = meta.file_type();
            if ft.is_symlink() {
                let target = fs::read_link(&path).unwrap();
                out.push((rel, "l", target.as_os_str().as_encoded_bytes().to_vec()));
            } else if ft.is_dir() {
                out.push((rel, "d", Vec::new()));
                walk(root, &path, out);
            } else {
                out.push((rel, "f", fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

// ===========================================================================
// INVARIANT 1 + 2 — NEVER delete outside the dest root; escaping symlink is
// removed-AS-A-LINK, never followed. Canaries outside the dest prove survival.
// ===========================================================================

/// INVARIANT 1/2: an extraneous dest symlink → an EXTERNAL FILE (absolute target)
/// is unlinked, but the external file survives byte-identical. Canary proves the
/// target was not deleted-through-the-link.
#[test]
fn escaping_symlink_to_external_file_absolute_is_unlinked_target_survives() {
    let src = build_src("ext-file-abs-src");
    let store = temp_dir("ext-file-abs-store");
    let dest = temp_dir("ext-file-abs-dest");
    let cache = temp_dir("ext-file-abs-cache");
    let home = temp_dir("ext-file-abs-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = pull_into(&dest, &store_url, &id, &cache, &home);

    // A canary file OUTSIDE the dest, with known bytes.
    let outside = temp_dir("ext-file-abs-outside");
    let canary = outside.join("CANARY.txt");
    fs::write(&canary, b"external file must survive byte-identical").unwrap();
    let before = fs::read(&canary).unwrap();

    // Extraneous dest symlink escaping to that external file (ABSOLUTE target).
    let escape = dest.join("escape-file");
    symlink(&canary, &escape).unwrap();

    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );

    assert!(
        escape.symlink_metadata().is_err(),
        "the escaping symlink must be unlinked (it is extraneous)"
    );
    assert!(
        canary.exists(),
        "external canary FILE must survive --delete"
    );
    assert_eq!(
        fs::read(&canary).unwrap(),
        before,
        "external canary FILE must be byte-identical (not deleted through the link)"
    );

    cleanup(&[&src, &store, &dest, &cache, &home, &outside]);
}

/// INVARIANT 1/2: an extraneous dest symlink → an EXTERNAL DIRECTORY is unlinked;
/// `--delete` must NOT recurse INTO it and delete its contents. The whole
/// external subtree (a canary file + a nested canary) survives byte-identical.
#[test]
fn escaping_symlink_to_external_dir_does_not_recurse_or_delete_contents() {
    let src = build_src("ext-dir-src");
    let store = temp_dir("ext-dir-store");
    let dest = temp_dir("ext-dir-dest");
    let cache = temp_dir("ext-dir-cache");
    let home = temp_dir("ext-dir-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = pull_into(&dest, &store_url, &id, &cache, &home);

    // A populated external directory OUTSIDE the dest.
    let outside = temp_dir("ext-dir-outside");
    fs::write(outside.join("CANARY.txt"), b"top canary").unwrap();
    fs::create_dir(outside.join("nested")).unwrap();
    fs::write(outside.join("nested").join("DEEP.txt"), b"deep canary").unwrap();
    let before = snapshot_tree(&outside);

    // Extraneous dest symlink escaping to that external DIRECTORY.
    let escape = dest.join("escape-dir");
    symlink(&outside, &escape).unwrap();

    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );

    assert!(
        escape.symlink_metadata().is_err(),
        "the escaping dir-symlink must be unlinked"
    );
    assert!(outside.exists(), "external directory must survive");
    assert_eq!(
        snapshot_tree(&outside),
        before,
        "the external directory's ENTIRE contents must survive byte-identical \
         (--delete must NOT follow the symlink and recurse into it)"
    );

    cleanup(&[&src, &store, &dest, &cache, &home, &outside]);
}

/// INVARIANT 1/2: a RELATIVE escaping symlink (`../../<outside>`) is unlinked;
/// the external target survives. A relative-path escape must be just as safe as
/// an absolute one.
#[test]
fn escaping_symlink_relative_traversal_is_unlinked_target_survives() {
    // Use a shared parent so a `..`-relative link can address an external file.
    let parent = temp_dir("rel-parent");
    let src = build_src("rel-src");
    let store = temp_dir("rel-store");
    let dest = parent.join("dest");
    fs::create_dir(&dest).unwrap();
    let cache = temp_dir("rel-cache");
    let home = temp_dir("rel-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);

    // Materialize the mirror into the nested dest.
    let dest_str = dest.to_string_lossy().into_owned();
    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );

    // A canary sibling of `dest` (outside it), addressed via `..`.
    let canary = parent.join("OUTSIDE_CANARY.txt");
    fs::write(&canary, b"relative escape must not reach me").unwrap();
    let before = fs::read(&canary).unwrap();

    // Extraneous RELATIVE escaping symlink: dest/escape-rel -> ../OUTSIDE_CANARY.txt
    let escape = dest.join("escape-rel");
    symlink(Path::new("../OUTSIDE_CANARY.txt"), &escape).unwrap();
    // Sanity: it resolves to the canary.
    assert_eq!(fs::read(&escape).unwrap(), before);

    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );

    assert!(
        escape.symlink_metadata().is_err(),
        "the relative escaping symlink must be unlinked"
    );
    assert!(
        canary.exists(),
        "the `..`-addressed external canary must survive"
    );
    assert_eq!(
        fs::read(&canary).unwrap(),
        before,
        "a `..`-traversal escape must NOT delete the external target"
    );

    cleanup(&[&src, &store, &cache, &home, &parent]);
}

/// INVARIANT 1/2: a `..`-traversal escaping symlink to an external DIRECTORY is
/// unlinked, not followed; the external dir's contents survive. Combines the
/// `..`-traversal and dir-recursion attack vectors.
#[test]
fn escaping_symlink_dotdot_to_external_dir_does_not_recurse() {
    let parent = temp_dir("dotdot-parent");
    let src = build_src("dotdot-src");
    let store = temp_dir("dotdot-store");
    let dest = parent.join("dest");
    fs::create_dir(&dest).unwrap();
    let cache = temp_dir("dotdot-cache");
    let home = temp_dir("dotdot-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = dest.to_string_lossy().into_owned();
    ok_stdout(
        snapdir(&cache, &home),
        &["pull", "--store", &store_url, "--id", &id, &dest_str],
    );

    // An external dir sibling of dest, with contents, addressed via `..`.
    let outside = parent.join("outside_dir");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("KEEP.txt"), b"keep me").unwrap();
    let before = snapshot_tree(&outside);

    let escape = dest.join("escape-dotdot-dir");
    symlink(Path::new("../outside_dir"), &escape).unwrap();

    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );

    assert!(escape.symlink_metadata().is_err(), "link unlinked");
    assert_eq!(
        snapshot_tree(&outside),
        before,
        "a `..`-to-dir escape must NOT recurse and delete the external dir's contents"
    );

    cleanup(&[&src, &store, &cache, &home, &parent]);
}

// ===========================================================================
// INVARIANT 3 + 5 — HARD-REFUSE dangerous dests (no `--force` bypass), nothing
// deleted; the dest tree is byte-identical to before the refusal.
// ===========================================================================

/// Plants a sentinel + an extra junk subtree inside `dest`, snapshots it, runs
/// `checkout --delete [--force] <dest>` (expecting REFUSAL), and asserts: the
/// command exits non-zero AND the dest tree is BYTE-IDENTICAL (nothing deleted —
/// the guard fired before any prune, all-or-nothing). `extra_args` lets a case
/// add `--store`/`--id` as needed.
fn assert_refuses_and_dest_unchanged(cmd: Command, dest: &Path, dest_arg: &str, args: &[&str]) {
    // Plant a sentinel AND an extraneous junk subtree: if the impl wrongly
    // pruned (treated this as a normal mirror), the junk would vanish. The
    // refusal must fire first, so the WHOLE tree must be byte-identical.
    let sentinel = dest.join("DO_NOT_DELETE.sentinel");
    fs::write(&sentinel, b"sentinel").unwrap();
    fs::create_dir_all(dest.join("junk").join("deep")).unwrap();
    fs::write(dest.join("junk").join("deep").join("x"), b"x").unwrap();
    let before = snapshot_tree(dest);

    let mut cmd = cmd;
    let out = cmd.args(args).arg(dest_arg).output().expect("run snapdir");
    assert!(
        !out.status.success(),
        "checkout --delete on dangerous dest {} (args {args:?}) MUST refuse non-zero",
        dest.display()
    );
    assert!(
        sentinel.exists(),
        "the sentinel under {} must survive the refusal",
        dest.display()
    );
    assert_eq!(
        snapshot_tree(dest),
        before,
        "NOTHING may be deleted on a guard refusal — dest {} must be byte-identical",
        dest.display()
    );
}

/// INVARIANT 3/5: dest = `$HOME` is hard-refused with AND without `--force`;
/// nothing deleted. HOME is env-redirected to a tempdir, so even a buggy bypass
/// can only touch the sandbox.
#[test]
fn refuses_home_dest_no_force_bypass_nothing_deleted() {
    let cache = temp_dir("home-cache");
    let home = temp_dir("home-home");
    let id = "0".repeat(64);
    assert_refuses_and_dest_unchanged(
        snapdir(&cache, &home),
        &home,
        &home.to_string_lossy(),
        &["checkout", "--id", &id, "--delete"],
    );
    assert_refuses_and_dest_unchanged(
        snapdir(&cache, &home),
        &home,
        &home.to_string_lossy(),
        &["checkout", "--id", &id, "--delete", "--force"],
    );
    cleanup(&[&cache, &home]);
}

/// INVARIANT 3: dest = `/` is hard-refused with AND without `--force`. We NEVER
/// write to or place a sentinel at real `/`; the refusal must fire on path
/// identity alone, so we assert ONLY the non-zero exit.
#[test]
fn refuses_filesystem_root_no_force_bypass() {
    let cache = temp_dir("root-cache");
    let home = temp_dir("root-home");
    let id = "0".repeat(64);
    for force in [false, true] {
        let mut cmd = snapdir(&cache, &home);
        cmd.args(["checkout", "--id", &id, "--delete"]);
        if force {
            cmd.arg("--force");
        }
        let out = cmd.arg("/").output().expect("run snapdir");
        assert!(
            !out.status.success(),
            "checkout --delete / (force={force}) MUST hard-refuse"
        );
    }
    cleanup(&[&cache, &home]);
}

/// INVARIANT 3/5: dest = the explicit cache dir (`$SNAPDIR_CACHE_DIR`) is
/// hard-refused (no `--force` bypass); the cache (content-addressable objects)
/// is untouched.
#[test]
fn refuses_explicit_cache_dir_no_force_bypass_nothing_deleted() {
    let cache = temp_dir("cachedir-cache");
    let home = temp_dir("cachedir-home");
    let id = "0".repeat(64);
    assert_refuses_and_dest_unchanged(
        snapdir(&cache, &home),
        &cache,
        &cache.to_string_lossy(),
        &["checkout", "--id", &id, "--delete"],
    );
    assert_refuses_and_dest_unchanged(
        snapdir(&cache, &home),
        &cache,
        &cache.to_string_lossy(),
        &["checkout", "--id", &id, "--delete", "--force"],
    );
    cleanup(&[&cache, &home]);
}

/// INVARIANT 3/5: dest = the DEFAULT cache dir (`$XDG_CACHE_HOME/snapdir`) is
/// guarded too, even when an explicit `SNAPDIR_CACHE_DIR` points elsewhere. A
/// sentinel in the default cache survives; `--force` does not bypass.
#[test]
fn refuses_default_cache_dir_no_force_bypass_nothing_deleted() {
    let home = temp_dir("defcache-home");
    let explicit_cache = temp_dir("defcache-explicit"); // a DIFFERENT explicit cache
    let default_cache = home.join(".cache").join("snapdir");
    fs::create_dir_all(&default_cache).unwrap();
    let id = "0".repeat(64);

    // snapdir() pins SNAPDIR_CACHE_DIR to `explicit_cache`, while XDG_CACHE_HOME
    // resolves the DEFAULT to `default_cache` — so dest == the default location,
    // not the configured one. The guard must still refuse it.
    assert_refuses_and_dest_unchanged(
        snapdir(&explicit_cache, &home),
        &default_cache,
        &default_cache.to_string_lossy(),
        &["checkout", "--id", &id, "--delete", "--force"],
    );
    cleanup(&[&home, &explicit_cache]);
}

/// INVARIANT 3/5: dest = a local store's on-disk root is hard-refused (no
/// `--force` bypass). The store holds `.manifests`/`.objects`; mirror-pruning it
/// would destroy the snapshot. The landed manifest + the whole store tree
/// survive byte-identical.
#[test]
fn refuses_store_path_no_force_bypass_nothing_deleted() {
    let src = build_src("storepath-src");
    let store = temp_dir("storepath-store");
    let cache = temp_dir("storepath-cache");
    let home = temp_dir("storepath-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);

    let manifest_key = store.join(format!(
        ".manifests/{}/{}/{}/{}",
        &id[0..3],
        &id[3..6],
        &id[6..9],
        &id[9..]
    ));
    assert!(manifest_key.is_file(), "precondition: manifest landed");
    let store_str = store.to_string_lossy().into_owned();
    let before = snapshot_tree(&store);

    for force in [false, true] {
        let mut cmd = snapdir(&cache, &home);
        cmd.args(["checkout", "--store", &store_url, "--id", &id, "--delete"]);
        if force {
            cmd.arg("--force");
        }
        let out = cmd.arg(&store_str).output().expect("run snapdir");
        assert!(
            !out.status.success(),
            "checkout --delete onto the store path (force={force}) MUST hard-refuse"
        );
        assert!(
            manifest_key.is_file(),
            "the store's manifest must survive (force={force})"
        );
        assert_eq!(
            snapshot_tree(&store),
            before,
            "the ENTIRE store tree must be byte-identical after the refusal (force={force})"
        );
    }

    cleanup(&[&src, &store, &cache, &home]);
}

// --- INVARIANT 3: canonicalization aliases all refuse (no lexical bypass) ---

/// INVARIANT 3: a guarded dest reached via a CANONICALIZATION ALIAS still
/// refuses — `$HOME` with a trailing `/`, with `/.`, and via a symlink that
/// resolves to `$HOME`. A lexical-only guard would miss these. HOME is
/// env-redirected; a sentinel proves nothing was pruned.
#[test]
fn refuses_home_canonicalization_aliases_no_force_bypass() {
    let cache = temp_dir("alias-cache");
    let home = temp_dir("alias-home");
    let sentinel = home.join("DO_NOT_DELETE.sentinel");
    fs::write(&sentinel, b"sentinel").unwrap();
    let before = snapshot_tree(&home);

    let home_str = home.to_string_lossy().into_owned();
    let link = home
        .parent()
        .unwrap()
        .join(format!("home-alias-{}", std::process::id()));
    fs::remove_file(&link).ok();
    symlink(&home, &link).unwrap();

    let aliases = [
        format!("{home_str}/"),
        format!("{home_str}/."),
        link.to_string_lossy().into_owned(),
    ];
    let id = "0".repeat(64);
    for alias in &aliases {
        let out = snapdir(&cache, &home)
            .args(["checkout", "--id", &id, "--delete", "--force", alias])
            .output()
            .expect("run snapdir");
        assert!(
            !out.status.success(),
            "checkout --delete onto a canonicalization-alias of $HOME ({alias}) MUST refuse"
        );
        assert!(sentinel.exists(), "sentinel must survive alias {alias}");
        assert_eq!(
            snapshot_tree(&home),
            before,
            "nothing deleted via $HOME alias {alias}"
        );
    }

    fs::remove_file(&link).ok();
    cleanup(&[&cache, &home]);
}

/// INVARIANT 3/6: a dest that is itself a SYMLINK resolving to a guarded dir
/// (e.g. `link -> $HOME`) is refused via canonicalization and is NOT followed
/// into `$HOME` to delete its contents. HOME is env-redirected; a sentinel in
/// the (sandbox) home survives, and the home tree is byte-identical.
#[test]
fn refuses_dest_symlink_to_guarded_home_not_followed() {
    let cache = temp_dir("symguard-cache");
    let home = temp_dir("symguard-home");
    let sentinel = home.join("DO_NOT_DELETE.sentinel");
    fs::write(&sentinel, b"sentinel").unwrap();
    fs::create_dir(home.join("private")).unwrap();
    fs::write(home.join("private").join("secret"), b"secret").unwrap();
    let before = snapshot_tree(&home);

    // A symlink (outside home) that points AT the guarded home.
    let link_parent = temp_dir("symguard-linkparent");
    let link = link_parent.join("dest-link");
    symlink(&home, &link).unwrap();
    let link_str = link.to_string_lossy().into_owned();
    let id = "0".repeat(64);

    let out = snapdir(&cache, &home)
        .args(["checkout", "--id", &id, "--delete", "--force", &link_str])
        .output()
        .expect("run snapdir");
    assert!(
        !out.status.success(),
        "a dest symlink resolving to $HOME MUST be refused (canonicalized guard)"
    );
    assert!(sentinel.exists(), "home sentinel must survive");
    assert_eq!(
        snapshot_tree(&home),
        before,
        "the guarded home tree must be byte-identical (not followed/deleted)"
    );

    cleanup(&[&cache, &home, &link_parent]);
}

// ===========================================================================
// INVARIANT 4 — `--exclude` honored: matching extraneous protected; sibling pruned.
// ===========================================================================

/// INVARIANT 4: an extraneous path matching `--exclude` is PROTECTED, while a
/// non-matching extraneous sibling is still pruned. (Extended-regex semantics.)
#[test]
fn exclude_protects_match_but_prunes_nonmatching_sibling() {
    let src = build_src("excl-src");
    let store = temp_dir("excl-store");
    let dest = temp_dir("excl-dest");
    let cache = temp_dir("excl-cache");
    let home = temp_dir("excl-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = pull_into(&dest, &store_url, &id, &cache, &home);

    fs::write(dest.join("keep.log"), b"protected").unwrap();
    fs::write(dest.join("drop.tmp"), b"prunable").unwrap();

    ok_stdout(
        snapdir(&cache, &home),
        &[
            "checkout",
            "--id",
            &id,
            "--delete",
            "--exclude",
            "keep\\.log",
            &dest_str,
        ],
    );
    assert!(
        dest.join("keep.log").exists(),
        "--exclude must PROTECT the matching extraneous path"
    );
    assert!(
        !dest.join("drop.tmp").exists(),
        "a non-matching extraneous sibling must still be pruned"
    );

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

// ===========================================================================
// INVARIANT 5 — `--dryrun --delete` deletes nothing while listing.
// ===========================================================================

/// INVARIANT 5: `--delete --dryrun` LISTS the deletion set and removes NOTHING —
/// the extraneous file (and a nested junk subtree) survive byte-identical.
#[test]
fn dryrun_delete_lists_and_removes_nothing() {
    let src = build_src("dry-src");
    let store = temp_dir("dry-store");
    let dest = temp_dir("dry-dest");
    let cache = temp_dir("dry-cache");
    let home = temp_dir("dry-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = pull_into(&dest, &store_url, &id, &cache, &home);

    fs::write(dest.join("WOULD_DELETE.txt"), b"junk").unwrap();
    fs::create_dir_all(dest.join("junkdir").join("deep")).unwrap();
    fs::write(dest.join("junkdir").join("deep").join("z"), b"z").unwrap();
    let before = snapshot_tree(&dest);

    let out = snapdir(&cache, &home)
        .args(["checkout", "--id", &id, "--delete", "--dryrun", &dest_str])
        .output()
        .expect("run snapdir");
    assert!(
        out.status.success(),
        "dryrun checkout --delete should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("WOULD_DELETE.txt"),
        "--dryrun must LIST the path it would delete; got: {combined}"
    );
    assert_eq!(
        snapshot_tree(&dest),
        before,
        "--dryrun --delete must delete NOTHING (dest byte-identical)"
    );

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

// ===========================================================================
// INVARIANT 6 — adversarial extras: symlink loop, deep nested prune.
// ===========================================================================

/// INVARIANT 6: a SYMLINK LOOP among extraneous dest entries does not hang or
/// panic the prune — the loop links are unlinked, the command completes, and the
/// in-manifest mirror survives. A naive symlink-following walk would loop
/// forever; the prune lstat's entries and never follows them.
#[test]
fn symlink_loop_in_dest_does_not_hang_or_panic() {
    let src = build_src("loop-src");
    let store = temp_dir("loop-store");
    let dest = temp_dir("loop-dest");
    let cache = temp_dir("loop-cache");
    let home = temp_dir("loop-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = pull_into(&dest, &store_url, &id, &cache, &home);

    // A mutual symlink loop among extraneous entries: loopA -> loopB -> loopA.
    let a = dest.join("loopA");
    let b = dest.join("loopB");
    symlink("loopB", &a).unwrap();
    symlink("loopA", &b).unwrap();
    // And a self-loop dir-symlink for good measure.
    let selfl = dest.join("selfloop");
    symlink("selfloop", &selfl).unwrap();

    // Must terminate (no hang) and succeed.
    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );

    assert!(a.symlink_metadata().is_err(), "loopA unlinked");
    assert!(b.symlink_metadata().is_err(), "loopB unlinked");
    assert!(selfl.symlink_metadata().is_err(), "selfloop unlinked");
    // The in-manifest mirror survives + still re-manifests to the source id.
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
    assert_eq!(ok_stdout(snapdir(&cache, &home), &["id", &dest_str]), id);

    cleanup(&[&src, &store, &dest, &cache, &home]);
}

/// INVARIANT 6: a DEEPLY NESTED extraneous tree is pruned entirely (deepest-first,
/// no escaping), and crucially an escaping symlink BURIED deep inside that
/// extraneous tree does not cause the prune to follow it out of the dest — an
/// external canary survives.
#[test]
fn deep_nested_extraneous_tree_pruned_without_escaping() {
    let src = build_src("deep-src");
    let store = temp_dir("deep-store");
    let dest = temp_dir("deep-dest");
    let cache = temp_dir("deep-cache");
    let home = temp_dir("deep-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = pull_into(&dest, &store_url, &id, &cache, &home);

    // A deep extraneous subtree under the dest.
    let mut deep = dest.join("junk");
    for seg in ["l1", "l2", "l3", "l4", "l5"] {
        deep = deep.join(seg);
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("leaf"), b"leaf").unwrap();

    // An external canary + a buried escaping symlink pointing at it.
    let outside = temp_dir("deep-outside");
    let canary = outside.join("CANARY.txt");
    fs::write(&canary, b"deep escape must not reach me").unwrap();
    symlink(&canary, deep.join("buried-escape")).unwrap();

    ok_stdout(
        snapdir(&cache, &home),
        &["checkout", "--id", &id, "--delete", &dest_str],
    );

    assert!(
        !dest.join("junk").exists(),
        "the whole deep extraneous subtree must be pruned"
    );
    assert!(canary.exists(), "the external canary must survive");
    assert_eq!(
        fs::read(&canary).unwrap(),
        b"deep escape must not reach me",
        "a buried escaping symlink must NOT be followed out of the dest"
    );
    // The mirror is intact.
    assert_eq!(ok_stdout(snapdir(&cache, &home), &["id", &dest_str]), id);

    cleanup(&[&src, &store, &dest, &cache, &home, &outside]);
}

/// INVARIANT 1 (pull parity): the `pull --delete` path enforces the same
/// outside-dest safety — an escaping symlink is unlinked, the external canary
/// survives. (`pull` = fetch + checkout; the guard/prune must apply on both.)
#[test]
fn pull_delete_escaping_symlink_target_survives() {
    let src = build_src("pull-esc-src");
    let store = temp_dir("pull-esc-store");
    let dest = temp_dir("pull-esc-dest");
    let cache = temp_dir("pull-esc-cache");
    let home = temp_dir("pull-esc-home");
    let (store_url, id) = push_to_store(&src, &cache, &home, &store);
    let dest_str = pull_into(&dest, &store_url, &id, &cache, &home);

    let outside = temp_dir("pull-esc-outside");
    let canary = outside.join("CANARY.txt");
    fs::write(&canary, b"pull must not delete me").unwrap();
    let escape = dest.join("escape");
    symlink(&outside, &escape).unwrap();

    // Re-run via PULL with --delete.
    ok_stdout(
        snapdir(&cache, &home),
        &[
            "pull", "--store", &store_url, "--id", &id, "--delete", &dest_str,
        ],
    );

    assert!(escape.symlink_metadata().is_err(), "escaping link unlinked");
    assert!(
        canary.exists(),
        "external canary must survive pull --delete"
    );
    assert_eq!(fs::read(&canary).unwrap(), b"pull must not delete me");

    cleanup(&[&src, &store, &dest, &cache, &home, &outside]);
}
