//! T1 hermetic round-trip tests for the `sftp://` engine, driven through the
//! real external-store contract: `snapdir_stores::ExternalStore::with_binary`
//! spawns the actual `snapdir-sftp-store` binary, captures the scripts it
//! emits, and `eval`s them exactly like the orchestrator
//! (`set -eEuo pipefail; trap 'kill 0' INT; <script> wait`).
//!
//! Hermetic: a per-test bin dir containing `tests/fixtures/fake-sftp`
//! installed as `sftp` is prepended to `PATH`, and `FAKE_REMOTE_ROOT` fences
//! the "remote" filesystem into a temp dir. No network, no real ssh
//! connection. The skeleton's cleanup does run the real `ssh -O exit`
//! (system PATH stays appended); with no `ControlMaster` socket it fails
//! instantly and the skeleton's `2>/dev/null || true` swallows it — which is
//! why no `ssh` stub is needed (verified by every test here passing with the
//! cleanup trap firing).
//!
//! Env vars are process-global, so every env-touching test serializes on
//! `ENV_LOCK` and restores via the `EnvGuard` drop. The emitted-text tests
//! at the bottom are pure (library calls, no env, no lock).

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
use snapdir_ssh_store::script::{sftp_quote, sh_quote};
use snapdir_ssh_store::sftp_engine;
use snapdir_ssh_store::url::SshUrl;
use snapdir_ssh_store::Engine;

use snapdir_stores::ExternalStore;

/// The store base path on the fake remote: maps to `<FAKE_REMOTE_ROOT>/snap`.
const REMOTE_BASE: &str = "/snap";
const STORE_URL: &str = "sftp://fakehost/snap";

/// Serializes env-touching tests (`PATH` / `FAKE_*` / `SNAPDIR_SFTP_STORE_*`
/// are process-global and flow into the spawned binary + eval shell +
/// fixture).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A unique temp dir removed on drop (no dev-dependency needed).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "snapdir-sftp-test-{}-{tag}-{n}",
            std::process::id()
        ));
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

/// Installs `tests/fixtures/fake-sftp` as `<bindir>/sftp` (mode 0755) and
/// returns the env guard with `PATH` prepended and `FAKE_REMOTE_ROOT` set.
///
/// Also shadows `bash` with the system `/bin/bash` (3.2 on macOS): the
/// shim's eval shell and the fixture's `env bash` shebang then run the
/// emitted scripts under the OLDEST bash we support, proving the
/// bash-3.2-cleanliness of the emitted text on every test run.
fn fake_remote_env(bindir: &Path, remote_root: &Path) -> EnvGuard {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-sftp");
    let sftp = bindir.join("sftp");
    fs::copy(&fixture, &sftp).expect("install fake-sftp as sftp");
    let mut perms = fs::metadata(&sftp).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&sftp, perms).unwrap();
    if Path::new("/bin/bash").is_file() {
        std::os::unix::fs::symlink("/bin/bash", bindir.join("bash")).expect("shadow bash");
    }

    let mut guard = EnvGuard::new();
    let old_path = std::env::var("PATH").unwrap_or_default();
    guard.set("PATH", &format!("{}:{old_path}", bindir.display()));
    guard.set("FAKE_REMOTE_ROOT", &remote_root.display().to_string());
    guard
}

fn external_store() -> ExternalStore {
    ExternalStore::with_binary(STORE_URL, env!("CARGO_BIN_EXE_snapdir-sftp-store"))
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
    remote_root
        .join(REMOTE_BASE.trim_start_matches('/'))
        .join(rel)
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
fn sftp_push_get_manifest_fetch_roundtrip() {
    let staging = TempDir::new("rt-stage");
    let remote_root = TempDir::new("rt-remote");
    let bindir = TempDir::new("rt-bin");
    let cache = TempDir::new("rt-cache");
    let _env = fake_remote_env(bindir.path(), remote_root.path());

    let files: &[(&str, &[u8])] = &[("foo", b"foo\n"), ("bar", b"bar bar\n")];
    let (manifest, id, sums) = stage_tree(staging.path(), files);
    let store = external_store();

    store.push(&manifest, staging.path()).expect("push");

    // Objects and the manifest landed at their sharded remote paths.
    for (sum, (_, content)) in sums.iter().zip(files) {
        let obj = remote(remote_root.path(), &object_path(sum));
        assert!(obj.is_file(), "object {sum} should be on the remote");
        assert_eq!(&fs::read(&obj).unwrap(), content);
        let mode = fs::metadata(&obj).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "chmod 600 discipline on {sum}");
    }
    let man = remote(remote_root.path(), &manifest_path(&id));
    assert!(man.is_file(), "manifest should be on the remote");
    assert_eq!(fs::read_to_string(&man).unwrap(), manifest.to_string());

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
fn sftp_get_manifest_missing_id_maps_to_manifest_not_found() {
    let remote_root = TempDir::new("nf-remote");
    let bindir = TempDir::new("nf-bin");
    let _env = fake_remote_env(bindir.path(), remote_root.path());

    let missing = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    match external_store().get_manifest(missing) {
        Err(StoreError::ManifestNotFound { id }) => assert_eq!(id, missing),
        other => panic!("expected ManifestNotFound, got {other:?}"),
    }
}

#[test]
fn sftp_unreachable_host_is_backend_error_not_not_found() {
    // Connectivity failure must NEVER map to not-found: the probe's `pwd`
    // liveness batch fails too, so the script exits with the real failure.
    let remote_root = TempDir::new("ur-remote");
    let bindir = TempDir::new("ur-bin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path());
    env.set("FAKE_SFTP_UNREACHABLE", "1");

    let id = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    match external_store().get_manifest(id) {
        Err(StoreError::Backend { .. }) => {}
        other => panic!("expected Backend error for unreachable store, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// idempotency
// ---------------------------------------------------------------------------

#[test]
fn sftp_push_is_noop_when_manifest_already_present() {
    let staging = TempDir::new("id-stage");
    let remote_root = TempDir::new("id-remote");
    let bindir = TempDir::new("id-bin");
    let _env = fake_remote_env(bindir.path(), remote_root.path());

    let (manifest, id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n")]);
    let store = external_store();
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
fn sftp_push_atomicity_failed_put_leaves_no_manifest_then_retry_completes() {
    let staging = TempDir::new("at-stage");
    let remote_root = TempDir::new("at-remote");
    let bindir = TempDir::new("at-bin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path());
    // One chunk so the per-process put counter is deterministic: the second
    // object's put fails mid-batch.
    env.set("SNAPDIR_SFTP_STORE_JOBS", "1");
    env.set("FAKE_SFTP_FAIL_VERB", "put");
    env.set("FAKE_SFTP_FAIL_AFTER", "2");

    let files: &[(&str, &[u8])] = &[("a", b"alpha\n"), ("b", b"bravo\n"), ("c", b"charlie\n")];
    let (manifest, id, sums) = stage_tree(staging.path(), files);
    let store = external_store();

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
    // SOME objects landed; every file at a FINAL object path is complete
    // (partials only ever exist at .tmp.<nonce> paths).
    let expected: Vec<Vec<u8>> = files.iter().map(|(_, c)| c.to_vec()).collect();
    let object_files = files_under(&remote(remote_root.path(), ".objects"));
    let mut finals = 0;
    for path in &object_files {
        let name = path.file_name().unwrap().to_string_lossy();
        if name.contains(".tmp.") {
            continue; // orphaned partial from the killed transfer — allowed
        }
        finals += 1;
        let content = fs::read(path).unwrap();
        assert!(
            expected.contains(&content),
            "non-tmp partial at final path {path:?}: {content:?}"
        );
    }
    assert!(finals >= 1, "the put before the injected failure landed");
    assert!(
        finals < files.len(),
        "the injected failure stopped the transfer early"
    );

    // Clear the injection: the retry is idempotent and completes the push.
    env.remove("FAKE_SFTP_FAIL_VERB");
    env.remove("FAKE_SFTP_FAIL_AFTER");
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
fn sftp_fetch_missing_object_reports_exact_error() {
    let staging = TempDir::new("fm-stage");
    let remote_root = TempDir::new("fm-remote");
    let bindir = TempDir::new("fm-bin");
    let cache = TempDir::new("fm-cache");
    let _env = fake_remote_env(bindir.path(), remote_root.path());

    let (manifest, _id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);
    let store = external_store();
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
}

// ---------------------------------------------------------------------------
// emitted-text invariants (pure library calls — no env, no fixture)
// ---------------------------------------------------------------------------

fn sftp_url() -> SshUrl {
    SshUrl::parse(Engine::Sftp, STORE_URL).unwrap()
}

fn default_cfg() -> Config {
    Config::from_lookup(Engine::Sftp, |_| None).unwrap()
}

#[test]
fn emitted_push_script_orders_manifest_last_with_tmp_rename_discipline() {
    let staging = TempDir::new("tx-stage");
    let (_, id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);
    let staging_dir = staging.path().display().to_string();
    let script =
        sftp_engine::get_push_script(&sftp_url(), &default_cfg(), &id, &staging_dir).unwrap();

    // Manifest commit comes AFTER the object-chunk machinery (objects-before-
    // manifest): the manifest batch is the last _snapdir_sftp invocation.
    let chunks_pos = script
        .rfind("chunk.")
        .expect("object chunk machinery present");
    let manifest_pos = script
        .find("$snapdir_tmp/manifest.batch")
        .expect("manifest batch present");
    assert!(
        manifest_pos > chunks_pos,
        "manifest batch must come after the object chunks"
    );
    let last_invocation = script.rfind("_snapdir_sftp ").unwrap();
    assert!(
        script[last_invocation..].contains("manifest.batch"),
        "the final transfer is the manifest commit"
    );

    // put-to-tmp + rename + chmod discipline for every object and the manifest.
    for sum in &sums {
        let remote_obj = format!("{REMOTE_BASE}/{}", object_path(sum));
        let staged_obj = format!("{staging_dir}/{}", object_path(sum));
        assert!(
            script.contains(&format!("put \"{staged_obj}\" \"{remote_obj}.tmp.")),
            "put-to-tmp for {sum}"
        );
        assert!(
            script.contains(&format!("\" \"{remote_obj}\"\nchmod 600 \"{remote_obj}\"")),
            "rename-into-place + chmod 600 for {sum}"
        );
        assert!(
            script.contains(&format!("-rm \"{remote_obj}\"")),
            "-rm before rename"
        );
    }
    let remote_man = format!("{REMOTE_BASE}/{}", manifest_path(&id));
    let staged_man = format!("{staging_dir}/{}", manifest_path(&id));
    assert!(script.contains(&format!("put \"{staged_man}\" \"{remote_man}.tmp.")));
    assert!(script.contains(&format!("chmod 600 \"{remote_man}\"")));

    // Exact no-op wording on the short-circuit path.
    assert!(script.contains("echo 'Manifest already exists on store.'"));

    // Existence probing of OBJECT paths is only ever tolerated (-ls): no
    // unprefixed `ls` of an object path (the manifest probe alone is
    // unprefixed, and intentionally so).
    for line in script.lines() {
        if line.starts_with("ls ") {
            assert!(
                !line.contains(".objects/"),
                "unprefixed ls of an object path: {line}"
            );
        }
        if line.contains(".objects/") && line.starts_with("-ls ") {
            assert!(line.starts_with("-ls \""), "tolerated quoted -ls: {line}");
        }
    }
    assert!(
        script.contains(&format!("-ls \"{REMOTE_BASE}/.objects/")),
        "tolerated object existence probe present"
    );

    // Explicit per-pid wait with status collection.
    assert!(script.contains("wait \"$snapdir_pid\" || snapdir_failed=1"));
}

#[test]
fn emitted_get_manifest_script_keeps_stdout_pure_with_exact_not_found_wording() {
    let id = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    let script =
        sftp_engine::get_manifest_script(&sftp_url(), &default_cfg(), id, STORE_URL).unwrap();

    // Exact not-found wording, on stderr, exit 1.
    let wording = sh_quote(&format!("ID '{id}' not found on --store '{STORE_URL}'."));
    assert!(
        script.contains(&format!("printf '%s\\n' {wording} >&2")),
        "exact not-found wording: {script}"
    );

    // Every sftp invocation's stdout is redirected away; only the final cat
    // emits manifest bytes on stdout.
    for line in script.lines() {
        if line.trim_start().starts_with("_snapdir_sftp ") {
            assert!(
                line.contains(">/dev/null") || line.contains(">\"$snapdir_tmp/"),
                "sftp chatter must not reach stdout: {line}"
            );
        }
    }
    assert!(script.trim_end().ends_with("cat \"$snapdir_tmp/manifest\""));

    // The probe disambiguates missing vs unreachable over the live master.
    assert!(script.contains("pwd"));
    assert!(script.contains("cat \"$snapdir_tmp/probe.err\" >&2"));
}

#[test]
fn emitted_fetch_script_has_exact_error_wording_tolerated_gets_and_ignored_chunk_exits() {
    let staging = TempDir::new("fx-stage");
    let cache = TempDir::new("fx-cache");
    let (manifest, _id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);
    let script = sftp_engine::get_fetch_files_script(
        &sftp_url(),
        &default_cfg(),
        &manifest.to_string(),
        &cache.path().display().to_string(),
    )
    .unwrap();

    // Incoming temp dir under the cache; exact missing-object wording.
    assert!(script.contains("mktemp -d \"$snapdir_cache/.snapdir-incoming.XXXXXX\""));
    assert!(script.contains("printf 'ERROR: missing object %s\\n' \"$snapdir_sum\" >&2"));

    // Transfers are tolerated -get lines (the post-check decides), chunk
    // exits are explicitly waited and IGNORED.
    assert!(script.contains("printf -- '-get %s \"%s/%s\"\\n'"));
    assert!(script.contains("wait \"$snapdir_pid\" || true"));

    // Every needed checksum's remote path is baked, sftp-quoted, at emit time.
    for sum in &sums {
        let remote_obj = format!("{REMOTE_BASE}/{}", object_path(sum));
        assert!(script.contains(&format!("{sum} {}", sftp_quote(&remote_obj))));
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

    let script = sftp_engine::get_fetch_files_script(
        &sftp_url(),
        &default_cfg(),
        &manifest.to_string(),
        &cache.path().display().to_string(),
    )
    .unwrap();
    assert!(
        !script.contains(&sums[0]),
        "cached object must be dropped at emit time"
    );
    assert!(script.contains(&sums[1]), "uncached object must be fetched");
}
