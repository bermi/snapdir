//! T1 hermetic round-trip tests for the `ssh://` dumb engine, driven through
//! the real external-store contract: `snapdir_stores::ExternalStore::
//! with_binary` spawns the actual `snapdir-ssh-store` binary, captures the
//! scripts it emits, and `eval`s them exactly like the orchestrator
//! (`set -eEuo pipefail; trap 'kill 0' INT; <script> wait`).
//!
//! Hermetic: a per-test bin dir containing `tests/fixtures/fake-ssh`
//! installed as `ssh` is prepended to `PATH`, and the "remote" filesystem is
//! fenced into a temp dir by constructing the store URL as
//! `ssh://fakehost<abs-fake-root-base>` — the emitted base path IS an
//! absolute path under `FAKE_REMOTE_ROOT`, so the fixture needs no path
//! remapping (remote commands run locally via `sh -c` with stdin/stdout
//! passed through, so the tar pipelines are honestly exercised). The
//! skeleton's `-O exit` cleanup also hits the fixture (a no-op success).
//!
//! Env vars are process-global, so every env-touching test serializes on
//! `ENV_LOCK` and restores via the `EnvGuard` drop. The emitted-text tests
//! at the bottom are pure (library calls, no env, no lock). The harness
//! helpers mirror `tests/fake_sftp_roundtrip.rs` (duplicated deliberately:
//! the two suites stay independently readable and runnable).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path, Store, StoreError};

use snapdir_ssh_store::config::Config;
use snapdir_ssh_store::script::{remote_manifest_path, sh_quote};
use snapdir_ssh_store::ssh_engine;
use snapdir_ssh_store::url::SshUrl;
use snapdir_ssh_store::Engine;

use snapdir_stores::ExternalStore;

/// Serializes env-touching tests (`PATH` / `FAKE_*` env are process-global
/// and flow into the spawned binary + eval shell + fixture).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A unique temp dir removed on drop (no dev-dependency needed).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("snapdir-ssh-test-{}-{tag}-{n}", std::process::id()));
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

/// Holds `ENV_LOCK` and restores every touched env var on drop (panic-safe).
struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new() -> Self {
        Self {
            saved: Vec::new(),
            _lock: ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    fn set(&mut self, key: &str, value: &str) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            self.saved.push((key.to_owned(), std::env::var(key).ok()));
        }
        std::env::set_var(key, value);
    }

    fn remove(&mut self, key: &str) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            self.saved.push((key.to_owned(), std::env::var(key).ok()));
        }
        std::env::remove_var(key);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(v) => std::env::set_var(&key, v),
                None => std::env::remove_var(&key),
            }
        }
    }
}

/// Installs `tests/fixtures/fake-ssh` as `<bindir>/ssh` (mode 0755) and
/// returns the env guard with `PATH` prepended and `FAKE_REMOTE_ROOT` set.
///
/// Also shadows `bash` with the system `/bin/bash` (3.2 on macOS): the
/// shim's eval shell then runs the emitted scripts under the OLDEST bash we
/// support, proving the bash-3.2-cleanliness of the emitted text on every
/// test run (the remote half runs under plain `sh` inside the fixture).
fn fake_remote_env(bindir: &Path, remote_root: &Path) -> EnvGuard {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-ssh");
    let ssh = bindir.join("ssh");
    fs::copy(&fixture, &ssh).expect("install fake-ssh as ssh");
    let mut perms = fs::metadata(&ssh).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&ssh, perms).unwrap();
    if Path::new("/bin/bash").is_file() {
        std::os::unix::fs::symlink("/bin/bash", bindir.join("bash")).expect("shadow bash");
    }

    let mut guard = EnvGuard::new();
    let old_path = std::env::var("PATH").unwrap_or_default();
    guard.set("PATH", &format!("{}:{old_path}", bindir.display()));
    guard.set("FAKE_REMOTE_ROOT", &remote_root.display().to_string());
    // This suite is the DUMB-path contract: pin the runtime dispatch to the
    // dumb bodies so a `snapdir` with wire plumbing on the host machine's
    // PATH can never flip these tests onto the accel path (tests/accel.rs
    // owns the dispatch + accel behavior), and scrub the other accel knobs
    // a developer shell might leak.
    guard.set("SNAPDIR_SSH_NO_ACCEL", "1");
    guard.remove("SNAPDIR_SSH_FORCE_ACCEL");
    guard.remove("SNAPDIR_SSH_PULL_SENDALL");
    guard.remove("SNAPDIR_SSH_LOCAL_SNAPDIR");
    guard.remove("FAKE_SSH_REMOTE_PATH");
    guard
}

/// The store base on the fake remote: an absolute path under the fenced
/// remote root (see the module docs — the URL embeds it, so the fixture
/// needs no remapping).
fn remote_base(remote_root: &Path) -> PathBuf {
    remote_root.join("snap")
}

fn store_url(remote_root: &Path) -> String {
    format!("ssh://fakehost{}", remote_base(remote_root).display())
}

fn external_store(remote_root: &Path) -> ExternalStore {
    ExternalStore::with_binary(
        &store_url(remote_root),
        env!("CARGO_BIN_EXE_snapdir-ssh-store"),
    )
}

/// Builds a manifest for `files` (name → content), writes the sharded
/// staging layout (objects + manifest) under `staging`, and returns the
/// manifest, its snapshot id, and the per-file checksums.
fn stage_tree(staging: &Path, files: &[(&str, &[u8])]) -> (Manifest, String, Vec<String>) {
    let hasher = Blake3Hasher::new();
    let mut entries = Vec::new();
    let mut sums = Vec::new();
    let mut total = 0u64;
    for (name, content) in files {
        let sum = hasher.hash_hex(content);
        entries.push(ManifestEntry::new(
            PathType::File,
            "600",
            sum.clone(),
            content.len() as u64,
            format!("./{name}"),
        ));
        sums.push(sum);
        total += content.len() as u64;
    }
    let root = directory_checksum(sums.iter().map(String::as_str), &hasher);
    entries.push(ManifestEntry::new(
        PathType::Directory,
        "700",
        root,
        total,
        "./",
    ));
    let manifest = Manifest::from_entries(entries);
    let id = snapshot_id(&manifest, &hasher);

    for (sum, (_, content)) in sums.iter().zip(files) {
        let obj = staging.join(object_path(sum));
        fs::create_dir_all(obj.parent().unwrap()).unwrap();
        fs::write(&obj, content).unwrap();
    }
    let man = staging.join(manifest_path(&id));
    fs::create_dir_all(man.parent().unwrap()).unwrap();
    fs::write(&man, manifest.to_string()).unwrap();
    (manifest, id, sums)
}

/// The fake remote path of `rel` under the store base.
fn remote(remote_root: &Path, rel: &str) -> PathBuf {
    remote_base(remote_root).join(rel)
}

/// Collects every regular file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(files_under(&path));
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// parity round-trip
// ---------------------------------------------------------------------------

#[test]
fn ssh_push_get_manifest_fetch_roundtrip() {
    let staging = TempDir::new("rt-stage");
    let remote_root = TempDir::new("rt-remote");
    let bindir = TempDir::new("rt-bin");
    let cache = TempDir::new("rt-cache");
    let _env = fake_remote_env(bindir.path(), remote_root.path());

    let files: &[(&str, &[u8])] = &[("foo", b"foo\n"), ("bar", b"bar bar\n")];
    let (manifest, id, sums) = stage_tree(staging.path(), files);
    let store = external_store(remote_root.path());

    store.push(&manifest, staging.path()).expect("push");

    // Objects and the manifest landed at their sharded remote paths, with
    // the umask discipline (no group/other bits).
    for (sum, (_, content)) in sums.iter().zip(files) {
        let obj = remote(remote_root.path(), &object_path(sum));
        assert!(obj.is_file(), "object {sum} should be on the remote");
        assert_eq!(&fs::read(&obj).unwrap(), content);
        let mode = fs::metadata(&obj).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "umask 077 discipline on {sum}: {mode:o}");
    }
    let man = remote(remote_root.path(), &manifest_path(&id));
    assert!(man.is_file(), "manifest should be on the remote");
    assert_eq!(fs::read_to_string(&man).unwrap(), manifest.to_string());
    let man_mode = fs::metadata(&man).unwrap().permissions().mode() & 0o777;
    assert_eq!(man_mode & 0o077, 0, "umask 077 discipline on the manifest");

    // No leftover remote temp dirs (the extract removed its incoming dir).
    assert!(
        !files_under(&remote_base(remote_root.path()))
            .iter()
            .any(|p| p.to_string_lossy().contains(".snapdir-incoming.")),
        "no incoming temp residue on the remote"
    );

    // get-manifest round-trips byte-identically (the shim also id-verifies).
    let fetched = store.get_manifest(&id).expect("get_manifest");
    assert_eq!(fetched.to_string(), manifest.to_string());

    // fetch_files lands objects in the sharded cache layout.
    store
        .fetch_files(&manifest, cache.path())
        .expect("fetch_files");
    for (sum, (_, content)) in sums.iter().zip(files) {
        let cached = cache.path().join(object_path(sum));
        assert!(cached.is_file(), "object {sum} should be in the cache");
        assert_eq!(&fs::read(&cached).unwrap(), content);
    }
    // The incoming temp dir was cleaned up.
    assert!(
        !files_under(cache.path())
            .iter()
            .any(|p| p.to_string_lossy().contains(".snapdir-incoming.")),
        "no incoming temp residue in the cache"
    );
}

// ---------------------------------------------------------------------------
// not-found mapping
// ---------------------------------------------------------------------------

#[test]
fn ssh_get_manifest_missing_id_maps_to_manifest_not_found() {
    let remote_root = TempDir::new("nf-remote");
    let bindir = TempDir::new("nf-bin");
    let _env = fake_remote_env(bindir.path(), remote_root.path());

    let missing = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    match external_store(remote_root.path()).get_manifest(missing) {
        Err(StoreError::ManifestNotFound { id }) => assert_eq!(id, missing),
        other => panic!("expected ManifestNotFound, got {other:?}"),
    }
}

#[test]
fn ssh_connectivity_failure_is_backend_error_not_not_found() {
    // The probe's exit-code discipline: a non-0/1 ssh exit (here 255 from
    // the injected connection failure) must surface as a Backend error and
    // NEVER masquerade as "not found" (which would trigger silent re-push).
    let remote_root = TempDir::new("cn-remote");
    let bindir = TempDir::new("cn-bin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path());
    env.set("FAKE_SSH_FAIL_MATCH", "test -f");

    let id = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    match external_store(remote_root.path()).get_manifest(id) {
        Err(StoreError::Backend { .. }) => {}
        other => panic!("expected Backend error for unreachable store, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// idempotency
// ---------------------------------------------------------------------------

#[test]
fn ssh_push_is_noop_when_manifest_already_present() {
    let staging = TempDir::new("id-stage");
    let remote_root = TempDir::new("id-remote");
    let bindir = TempDir::new("id-bin");
    let _env = fake_remote_env(bindir.path(), remote_root.path());

    let (manifest, id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n")]);
    let store = external_store(remote_root.path());
    store.push(&manifest, staging.path()).expect("first push");

    // Remove the object but keep the manifest: a second push must short-
    // circuit on the manifest probe and NOT touch the remote at all.
    let obj = remote(remote_root.path(), &object_path(&sums[0]));
    fs::remove_file(&obj).unwrap();

    store
        .push(&manifest, staging.path())
        .expect("second push (no-op)");
    assert!(
        !obj.exists(),
        "second push should have been a no-op (manifest already present)"
    );
    assert!(remote(remote_root.path(), &manifest_path(&id)).is_file());
}

// ---------------------------------------------------------------------------
// atomicity
// ---------------------------------------------------------------------------

#[test]
fn ssh_push_atomicity_truncated_transfer_leaves_no_manifest_then_retry_completes() {
    let staging = TempDir::new("at-stage");
    let remote_root = TempDir::new("at-remote");
    let bindir = TempDir::new("at-bin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path());
    // The remote tar-extract sees only the first 1536 bytes of the stream
    // (first object complete, second cut mid-header) and then the
    // "connection" dies.
    env.set("FAKE_SSH_TRUNCATE_TAR", "1536");

    let files: &[(&str, &[u8])] = &[("a", b"alpha\n"), ("b", b"bravo\n"), ("c", b"charlie\n")];
    let (manifest, id, sums) = stage_tree(staging.path(), files);
    let store = external_store(remote_root.path());

    let err = store.push(&manifest, staging.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::Backend { .. }),
        "expected Backend error from failed push, got {err:?}"
    );

    // NO manifest committed.
    assert!(
        !remote(remote_root.path(), &manifest_path(&id)).exists(),
        "a failed push must not commit the manifest"
    );
    // Nothing incomplete at FINAL paths: every file on the remote is either
    // inside a .snapdir-incoming temp dir (debris of the killed transfer —
    // allowed, a retry ignores it) or a complete expected object.
    let expected: Vec<Vec<u8>> = files.iter().map(|(_, c)| c.to_vec()).collect();
    for path in files_under(&remote_base(remote_root.path())) {
        if path.to_string_lossy().contains(".snapdir-incoming.") {
            continue;
        }
        let content = fs::read(&path).unwrap();
        assert!(
            expected.contains(&content),
            "incomplete file at final path {path:?}: {content:?}"
        );
    }

    // Clear the injection: the retry is idempotent and completes the push.
    env.remove("FAKE_SSH_TRUNCATE_TAR");
    store.push(&manifest, staging.path()).expect("retry push");
    for (sum, (_, content)) in sums.iter().zip(files) {
        let obj = remote(remote_root.path(), &object_path(sum));
        assert_eq!(&fs::read(&obj).unwrap(), content, "object {sum} complete");
    }
    assert!(remote(remote_root.path(), &manifest_path(&id)).is_file());
}

// ---------------------------------------------------------------------------
// fetch-missing-object
// ---------------------------------------------------------------------------

#[test]
fn ssh_fetch_missing_object_fails_before_any_transfer_with_exact_error() {
    let staging = TempDir::new("fm-stage");
    let remote_root = TempDir::new("fm-remote");
    let bindir = TempDir::new("fm-bin");
    let cache = TempDir::new("fm-cache");
    let _env = fake_remote_env(bindir.path(), remote_root.path());

    let (manifest, _id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);
    let store = external_store(remote_root.path());
    store.push(&manifest, staging.path()).expect("push");

    // Delete one remote object: fetch must fail with the exact wording.
    fs::remove_file(remote(remote_root.path(), &object_path(&sums[1]))).unwrap();

    let err = store.fetch_files(&manifest, cache.path()).unwrap_err();
    match &err {
        StoreError::Backend { message, .. } => assert!(
            message.contains(&format!("ERROR: missing object {}", sums[1])),
            "exact missing-object wording expected in: {message}"
        ),
        other => panic!("expected Backend error, got {other:?}"),
    }
    // The pre-transfer check fired BEFORE any object moved: the cache got
    // nothing (not even the object that does exist on the remote).
    assert!(
        files_under(cache.path()).is_empty(),
        "the missing-object check must abort before any transfer"
    );
}

// ---------------------------------------------------------------------------
// tar-allowlist (malicious remote)
// ---------------------------------------------------------------------------

#[test]
fn ssh_fetch_rejects_unexpected_tar_entries_without_extracting() {
    let staging = TempDir::new("ev-stage");
    let remote_root = TempDir::new("ev-remote");
    let bindir = TempDir::new("ev-bin");
    let cache = TempDir::new("ev-cache");
    let mut env = fake_remote_env(bindir.path(), remote_root.path());

    let (manifest, _id, _sums) = stage_tree(staging.path(), &[("foo", b"foo\n")]);
    let store = external_store(remote_root.path());
    store.push(&manifest, staging.path()).expect("push");

    // Hostile remote: the tar stream carries a foreign `payload/evil`
    // entry. The exact-match allowlist must name it and refuse to extract
    // — `../`/absolute/symlink entry names are covered by the same
    // exact-match property (anything not literally expected is rejected).
    env.set("FAKE_SSH_EVIL_TAR", "1");
    let err = store.fetch_files(&manifest, cache.path()).unwrap_err();
    match &err {
        StoreError::Backend { message, .. } => assert!(
            message.contains("payload/evil"),
            "the unexpected entry must be named: {message}"
        ),
        other => panic!("expected Backend error, got {other:?}"),
    }
    // NOTHING was extracted: no foreign files, no objects, no temp dirs.
    assert!(
        files_under(cache.path()).is_empty(),
        "a rejected tar must never reach the cache: {:?}",
        files_under(cache.path())
    );
}

// ---------------------------------------------------------------------------
// emitted-text invariants (pure library calls — no env, no fixture)
// ---------------------------------------------------------------------------

const TEXT_URL: &str = "ssh://fakehost/srv/snap";

fn ssh_url() -> SshUrl {
    SshUrl::parse(Engine::Ssh, TEXT_URL).unwrap()
}

fn default_cfg() -> Config {
    Config::from_lookup(Engine::Ssh, |_| None).unwrap()
}

/// Every `ssh` process the script can spawn goes through a line carrying
/// the security floor, and nothing traps `INT` (the orchestrator owns it).
fn assert_skeleton_invariants(script: &str) {
    for line in script.lines() {
        if line.contains("command ssh") {
            assert!(
                line.contains("'StrictHostKeyChecking=yes'"),
                "every ssh invocation must carry the floor: {line}"
            );
        }
    }
    assert!(
        script.contains("command ssh"),
        "the _snapdir_ssh wrapper must be defined"
    );
    assert!(!script.contains("INT"), "the script must never trap INT");
}

#[test]
fn emitted_push_script_probes_then_transfers_then_commits_inside_dumb_function() {
    let staging = TempDir::new("tx-stage");
    let (_, id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);
    let staging_dir = staging.path().display().to_string();
    let script =
        ssh_engine::get_push_script(&ssh_url(), &default_cfg(), &id, &staging_dir).unwrap();
    assert_skeleton_invariants(&script);

    // Ordering: manifest probe → object transfer → manifest commit.
    let probe = script.find("test -f").expect("manifest probe present");
    let transfer = script.find("tar -C").expect("tar transfer present");
    let commit = script
        .find(".snapdir-manifest.")
        .expect("manifest commit present");
    assert!(probe < transfer, "probe must precede the object transfer");
    assert!(
        transfer < commit,
        "the object transfer must precede the manifest commit"
    );

    // The dumb body is a function the runtime dispatch branches around
    // (both paths embedded; tests/accel.rs owns the dispatch behavior).
    assert!(script.contains("_snapdir_dumb_push() {"));
    assert!(script.contains("_snapdir_accel_push() {"));
    assert!(
        script.contains("  _snapdir_dumb_push\n"),
        "the dispatch must still invoke the dumb body"
    );

    // Exact no-op wording on the short-circuit path, BEFORE the dumb body.
    let noop = script
        .find("echo 'Manifest already exists on store.'")
        .expect("exact no-op wording");
    assert!(noop < script.find("_snapdir_dumb_push() {").unwrap());

    // Every candidate sharded relpath is baked into the heredoc; the
    // staging dir is quoted at the local tar.
    for sum in &sums {
        assert!(script.contains(&object_path(sum)), "relpath for {sum}");
    }
    assert!(script.contains(&format!("tar -C {} -cf -", sh_quote(&staging_dir))));

    // Remote commands stay POSIX (no bashisms): atomic same-fs rename via
    // mktemp temp dir + mv -f, under the configured umask.
    assert!(script.contains("mktemp -d .snapdir-incoming.XXXXXX"));
    assert!(script.contains("umask 077"));
    assert!(script.contains("mv -f"));
}

#[test]
fn emitted_get_manifest_script_has_exact_not_found_wording_and_exit_code_discipline() {
    let id = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    let script = ssh_engine::get_manifest_script(&ssh_url(), &default_cfg(), id, TEXT_URL).unwrap();
    assert_skeleton_invariants(&script);

    // Exact not-found wording, on stderr, exit 1 — only on probe exit 1.
    let wording = sh_quote(&format!("ID '{id}' not found on --store '{TEXT_URL}'."));
    assert!(
        script.contains(&format!("printf '%s\\n' {wording} >&2")),
        "exact not-found wording: {script}"
    );
    // Any other nonzero probe exit is connectivity, surfaced with the real
    // code — NEVER the not-found wording.
    assert!(script.contains("elif [ \"$snapdir_probe_status\" -ne 1 ]; then"));
    assert!(script.contains("exit \"$snapdir_probe_status\""));
    assert!(script.contains("failed to reach the store"));

    // stdout purity: the only stdout producer is the final `cat <manifest>`.
    let remote_man = remote_manifest_path("/srv/snap", id);
    let cat_invocation = format!(
        "_snapdir_ssh {}",
        sh_quote(&format!("cat {}", sh_quote(&remote_man)))
    );
    assert!(
        script.trim_end().ends_with(&cat_invocation),
        "the final command must be the manifest cat: {script}"
    );
}

#[test]
fn emitted_fetch_script_gates_extraction_on_the_exact_match_allowlist() {
    let staging = TempDir::new("fx-stage");
    let cache = TempDir::new("fx-cache");
    let (manifest, _id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);
    let script = ssh_engine::get_fetch_files_script(
        &ssh_url(),
        &default_cfg(),
        &manifest.to_string(),
        &cache.path().display().to_string(),
    )
    .unwrap();
    assert_skeleton_invariants(&script);

    // The dumb body is a function the runtime dispatch branches around
    // (both paths embedded; tests/accel.rs owns the dispatch behavior).
    assert!(script.contains("_snapdir_dumb_fetch() {"));
    assert!(script.contains("_snapdir_accel_fetch() {"));
    assert!(
        script.contains("  _snapdir_dumb_fetch\n"),
        "the dispatch must still invoke the dumb body"
    );

    // Ordering: pre-transfer remote existence check → tar saved to a local
    // file → allowlist gate → extraction into the incoming temp dir.
    let preflight = script
        .find("ERROR: missing object")
        .expect("pre-transfer check present");
    let saved = script.find("objects.tar").expect("tar saved locally");
    let allowlist = script
        .find("LC_ALL=C grep -vxF -f")
        .expect("exact-match allowlist present");
    let extract = script
        .find("tar -C \"$snapdir_ltmp\" -xf")
        .expect("extraction present");
    assert!(preflight < saved, "existence check precedes the transfer");
    assert!(
        saved < allowlist && allowlist < extract,
        "no extraction before the allowlist"
    );

    // Exact ERROR wording in BOTH the remote pre-check and the epilogue.
    assert!(script.matches("ERROR: missing object").count() >= 2);
    assert!(script.contains("printf 'ERROR: missing object %s\\n' \"$snapdir_sum\" >&2"));

    // Incoming temp dir under the cache; every needed pair baked.
    assert!(script.contains("mktemp -d \"$snapdir_cache/.snapdir-incoming.XXXXXX\""));
    for sum in &sums {
        assert!(script.contains(&format!("{sum} {}", object_path(sum))));
    }
}

#[test]
fn emitted_fetch_script_skips_objects_already_cached() {
    let staging = TempDir::new("fc-stage");
    let cache = TempDir::new("fc-cache");
    let (manifest, _id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);

    // Pre-seed one object in the cache: it must not be re-fetched.
    let cached = cache.path().join(object_path(&sums[0]));
    fs::create_dir_all(cached.parent().unwrap()).unwrap();
    fs::write(&cached, b"foo\n").unwrap();

    let script = ssh_engine::get_fetch_files_script(
        &ssh_url(),
        &default_cfg(),
        &manifest.to_string(),
        &cache.path().display().to_string(),
    )
    .unwrap();
    // The cached object is dropped from the transfer set at emit time: its
    // sharded relpath never appears (no dumb pair, no expected-list entry),
    // and the only place the bare checksum survives is the full-list
    // `ids_all` heredoc baked for the runtime SNAPDIR_SSH_PULL_SENDALL knob.
    assert!(
        !script.contains(&object_path(&sums[0])),
        "cached object must be dropped from the transfer set at emit time"
    );
    assert_eq!(
        script.matches(&sums[0]).count(),
        1,
        "the cached checksum may only survive in the baked SENDALL list"
    );
    assert!(script.contains(&sums[1]), "uncached object must be fetched");
}
