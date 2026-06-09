//! Gate `ssh-accel` (phase 24): the runtime-negotiated SNAPPACK acceleration
//! in the emitted `ssh://` scripts, exercised hermetically through the real
//! external-store contract (`ExternalStore::with_binary` →
//! `snapdir-ssh-store` → emitted script → `tests/fixtures/fake-ssh`).
//!
//! The "remote host" is the fake-ssh fixture executing locally via `sh -c`
//! with `FAKE_SSH_REMOTE_PATH` prepended to its PATH — tests point it at a
//! per-test bin dir holding either the REAL `snapdir` binary (wrapped in a
//! logging shim so accel engagement is asserted, never assumed), a hostile
//! fake (`wire=99`), an "old snapdir" fake (errors on `version
//! --capabilities`), or a sentinel fake that records any plumbing
//! invocation. The local pipe ends are injected via the script-runtime
//! `SNAPDIR_SSH_LOCAL_SNAPDIR` override (documented test/debug plumbing).
//!
//! **Locating the real `snapdir` binary**: `CARGO_BIN_EXE_snapdir` only
//! resolves inside snapdir-cli, so the tests look for a sibling target-dir
//! artifact (`current_exe()/../../snapdir`, i.e. `target/<profile>/snapdir`,
//! with a `CARGO_MANIFEST_DIR/../../target/<profile>` fallback). Under
//! `cargo test --workspace` (CI) snapdir-cli builds first, so the binary is
//! present and the accel tests RUN; a bare `cargo test -p snapdir-ssh-store`
//! on a clean tree skips them with an eprintln (run
//! `cargo build -p snapdir-cli` first).
//!
//! Harness helpers mirror `tests/fake_ssh_roundtrip.rs` (duplicated
//! deliberately — the suites stay independently readable and runnable);
//! that suite pins `SNAPDIR_SSH_NO_ACCEL=1` and owns the dumb-path
//! contract, THIS suite owns the dispatch + accel behavior.

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
use snapdir_ssh_store::script::sh_quote;
use snapdir_ssh_store::ssh_engine;
use snapdir_ssh_store::url::SshUrl;
use snapdir_ssh_store::Engine;

use snapdir_stores::ExternalStore;

/// Serializes env-touching tests (`PATH` / `FAKE_*` / `SNAPDIR_SSH_*` env
/// are process-global and flow into the spawned binary + eval shell +
/// fixture + "remote" snapdir).
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
            "snapdir-ssh-accel-{}-{tag}-{n}",
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

/// Locates the REAL `snapdir` CLI binary as a sibling target-dir artifact
/// (see the module docs). `None` = not built.
fn snapdir_cli_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        // target/<profile>/deps/accel-<hash> → target/<profile>/snapdir
        if let Some(profile_dir) = exe.parent().and_then(Path::parent) {
            let candidate = profile_dir.join("snapdir");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let workspace_target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    ["debug", "release"]
        .iter()
        .map(|profile| workspace_target.join(profile).join("snapdir"))
        .find(|candidate| candidate.is_file())
}

/// The real binary, or an eprintln skip marker (`None`). CI runs
/// `cargo test --workspace`, where snapdir-cli builds first — the accel
/// tests therefore always RUN there.
fn require_snapdir(test: &str) -> Option<PathBuf> {
    let bin = snapdir_cli_binary();
    if bin.is_none() {
        eprintln!(
            "SKIP {test}: the real `snapdir` binary is not in the target dir \
             (run `cargo build -p snapdir-cli` first, or `cargo test --workspace`)"
        );
    }
    bin
}

/// Installs `tests/fixtures/fake-ssh` as `<bindir>/ssh` (mode 0755) and
/// returns the env guard with `PATH` prepended, `FAKE_REMOTE_ROOT` set, the
/// accel/runtime knobs scrubbed (this suite sets them per test), and
/// `SNAPDIR_CACHE_DIR` pinned to a temp dir (the "remote" snapdir inherits
/// this process's env through the fixture — its cache must never touch the
/// developer's real one). Shadows `bash` with `/bin/bash` (3.2 on macOS)
/// like the dumb suite, proving emitted-text bash-3.2-cleanliness.
fn fake_remote_env(bindir: &Path, remote_root: &Path, cache_pin: &Path) -> EnvGuard {
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
    guard.set("SNAPDIR_CACHE_DIR", &cache_pin.display().to_string());
    guard.remove("SNAPDIR_STORE");
    guard.remove("SNAPDIR_SSH_NO_ACCEL");
    guard.remove("SNAPDIR_SSH_FORCE_ACCEL");
    guard.remove("SNAPDIR_SSH_PULL_SENDALL");
    guard.remove("SNAPDIR_SSH_LOCAL_SNAPDIR");
    guard.remove("FAKE_SSH_REMOTE_PATH");
    guard.remove("FAKE_SSH_FAIL_MATCH");
    guard.remove("FAKE_SSH_TRUNCATE_TAR");
    guard.remove("FAKE_SSH_EVIL_TAR");
    guard
}

/// Writes `content` as an executable script at `path`.
fn write_script(path: &Path, content: &str) {
    fs::write(path, content).expect("write script");
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

/// Installs a remote `snapdir` into `dir` that logs every invocation's argv
/// to `log` and execs the REAL binary — accel engagement is asserted from
/// the log, never assumed.
fn install_logging_snapdir(dir: &Path, real: &Path, log: &Path) {
    write_script(
        &dir.join("snapdir"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>{log}\nexec {real} \"$@\"\n",
            log = sh_quote(&log.display().to_string()),
            real = sh_quote(&real.display().to_string()),
        ),
    );
}

/// Installs a remote `snapdir` that answers `version --capabilities` with
/// `caps_line` and records any OTHER invocation (the plumbing the dispatch
/// must not call) by creating `sentinel` and failing.
fn install_caps_only_snapdir(dir: &Path, caps_line: &str, sentinel: &Path) {
    write_script(
        &dir.join("snapdir"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = version ] && [ \"$2\" = --capabilities ]; then\n  \
             printf '%s\\n' {caps}\n  exit 0\nfi\nprintf '%s\\n' \"$*\" >{sentinel}\nexit 1\n",
            caps = sh_quote(caps_line),
            sentinel = sh_quote(&sentinel.display().to_string()),
        ),
    );
}

/// Installs a remote `snapdir` that predates the wire plumbing: ANY
/// invocation (including `version --capabilities`) errors, so the combined
/// probe degrades to `caps none`.
fn install_old_snapdir(dir: &Path) {
    write_script(
        &dir.join("snapdir"),
        "#!/bin/sh\necho 'snapdir: unknown option' >&2\nexit 2\n",
    );
}

fn store_url(base: &Path) -> String {
    format!("ssh://fakehost{}", base.display())
}

fn external_store(base: &Path) -> ExternalStore {
    ExternalStore::with_binary(&store_url(base), env!("CARGO_BIN_EXE_snapdir-ssh-store"))
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
    // The CANONICAL stored byte form (`to_string()` + trailing `\n`,
    // matching file_store.rs::write_manifest): the real orchestrator stages
    // through the cache FileStore, and the pack stream re-serializes to the
    // same form — the oracle compares manifest BYTES across both paths.
    fs::write(&man, manifest_bytes(&manifest)).unwrap();
    (manifest, id, sums)
}

/// The serialized (stored) byte form of a manifest: text + trailing `\n`.
fn manifest_bytes(manifest: &Manifest) -> String {
    format!("{manifest}\n")
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

/// The SORTED relative paths of every regular file under `root`.
fn relative_file_set(root: &Path) -> Vec<String> {
    let mut rels: Vec<String> = files_under(root)
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .expect("file under root")
                .display()
                .to_string()
        })
        .collect();
    rels.sort();
    rels
}

/// The default test files (several distinct objects).
const FILES: &[(&str, &[u8])] = &[
    ("a.txt", b"alpha payload\n"),
    ("b.txt", b"bravo bravo payload\n"),
    ("c.bin", b"charlie third object\n"),
];

fn log_lines(log: &Path) -> String {
    fs::read_to_string(log).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// accel-oracle (HEADLINE): forced-dumb and accel pushes are byte-identical
// ---------------------------------------------------------------------------

#[test]
fn accel_oracle_dumb_and_accel_pushes_are_byte_identical() {
    let Some(real) = require_snapdir("accel_oracle_dumb_and_accel_pushes_are_byte_identical")
    else {
        return;
    };
    let staging = TempDir::new("or-stage");
    let remote_root = TempDir::new("or-remote");
    let bindir = TempDir::new("or-bin");
    let remote_bin = TempDir::new("or-remote-bin");
    let cache_pin = TempDir::new("or-cachepin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path(), cache_pin.path());

    let (manifest, id, _sums) = stage_tree(staging.path(), FILES);
    let root_dumb = remote_root.path().join("dumb");
    let root_accel = remote_root.path().join("accel");

    // Push 1: forced dumb into root A.
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");
    external_store(&root_dumb)
        .push(&manifest, staging.path())
        .expect("forced-dumb push");

    // Push 2: accel into root B (real snapdir on the remote PATH behind a
    // logging wrapper; the local pipe ends use the same real binary).
    let log = remote_bin.path().join("invocations.log");
    install_logging_snapdir(remote_bin.path(), &real, &log);
    env.remove("SNAPDIR_SSH_NO_ACCEL");
    env.set(
        "FAKE_SSH_REMOTE_PATH",
        &remote_bin.path().display().to_string(),
    );
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());
    external_store(&root_accel)
        .push(&manifest, staging.path())
        .expect("accel push");

    // The accel path actually ran (never assume): the remote saw the diff
    // and the pack stream.
    let log = log_lines(&log);
    assert!(log.contains("objects-needed"), "accel diff ran: {log}");
    assert!(log.contains("receive-pack"), "accel stream ran: {log}");

    // IDENTICAL results: same snapshot id committed (same manifest path),
    // identical recursive file SET, byte-equal contents (objects AND
    // manifest) — mirroring sync.rs's mirrors-same-snapshot assertions.
    let set_dumb = relative_file_set(&root_dumb);
    let set_accel = relative_file_set(&root_accel);
    assert_eq!(
        set_dumb, set_accel,
        "the .objects/** + manifest file sets must be identical"
    );
    assert!(
        set_dumb.contains(&manifest_path(&id)),
        "snapshot id committed on both"
    );
    for rel in &set_dumb {
        assert_eq!(
            fs::read(root_dumb.join(rel)).unwrap(),
            fs::read(root_accel.join(rel)).unwrap(),
            "byte-equal at {rel}"
        );
    }
    assert_eq!(
        fs::read_to_string(root_accel.join(manifest_path(&id))).unwrap(),
        manifest_bytes(&manifest),
        "manifest bytes are the staged manifest's bytes"
    );
}

// ---------------------------------------------------------------------------
// accel push → get-manifest → accel fetch round trip
// ---------------------------------------------------------------------------

#[test]
fn accel_push_get_manifest_accel_fetch_roundtrip() {
    let Some(real) = require_snapdir("accel_push_get_manifest_accel_fetch_roundtrip") else {
        return;
    };
    let staging = TempDir::new("rt-stage");
    let remote_root = TempDir::new("rt-remote");
    let bindir = TempDir::new("rt-bin");
    let remote_bin = TempDir::new("rt-remote-bin");
    let cache = TempDir::new("rt-cache");
    let cache_pin = TempDir::new("rt-cachepin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path(), cache_pin.path());

    let log = remote_bin.path().join("invocations.log");
    install_logging_snapdir(remote_bin.path(), &real, &log);
    env.set(
        "FAKE_SSH_REMOTE_PATH",
        &remote_bin.path().display().to_string(),
    );
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());

    let (manifest, id, sums) = stage_tree(staging.path(), FILES);
    let base = remote_root.path().join("snap");
    let store = external_store(&base);

    store.push(&manifest, staging.path()).expect("accel push");
    assert!(log_lines(&log).contains("receive-pack"), "push used accel");

    // get-manifest round-trips byte-identically (the shim also id-verifies).
    let fetched = store.get_manifest(&id).expect("get_manifest");
    assert_eq!(fetched.to_string(), manifest.to_string());

    // Accel fetch into a cold cache lands every object, byte-equal, at the
    // sharded cache paths (the LOCAL receive-pack verified each record).
    store
        .fetch_files(&manifest, cache.path())
        .expect("accel fetch");
    assert!(log_lines(&log).contains("send-pack"), "fetch used accel");
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        let cached = cache.path().join(object_path(sum));
        assert_eq!(
            &fs::read(&cached).unwrap_or_else(|_| panic!("object {sum} in cache")),
            content
        );
    }
}

// ---------------------------------------------------------------------------
// interrupted-accel completion: manifest-only pack
// ---------------------------------------------------------------------------

#[test]
fn accel_push_completes_interrupted_push_with_manifest_only_pack() {
    let Some(real) =
        require_snapdir("accel_push_completes_interrupted_push_with_manifest_only_pack")
    else {
        return;
    };
    let staging = TempDir::new("ip-stage");
    let remote_root = TempDir::new("ip-remote");
    let bindir = TempDir::new("ip-bin");
    let remote_bin = TempDir::new("ip-remote-bin");
    let cache_pin = TempDir::new("ip-cachepin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path(), cache_pin.path());

    let log = remote_bin.path().join("invocations.log");
    install_logging_snapdir(remote_bin.path(), &real, &log);
    env.set(
        "FAKE_SSH_REMOTE_PATH",
        &remote_bin.path().display().to_string(),
    );
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());

    let (manifest, id, sums) = stage_tree(staging.path(), FILES);
    let base = remote_root.path().join("snap");

    // Interrupted state: every OBJECT already on the remote, NO manifest
    // (the want list is fully present → objects-needed answers empty → the
    // stream is the manifest-only pack).
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        let obj = base.join(object_path(sum));
        fs::create_dir_all(obj.parent().unwrap()).unwrap();
        fs::write(&obj, content).unwrap();
    }
    assert!(!base.join(manifest_path(&id)).exists());

    external_store(&base)
        .push(&manifest, staging.path())
        .expect("completing accel push");

    let log = log_lines(&log);
    assert!(log.contains("objects-needed"), "diff ran: {log}");
    assert!(
        log.contains("receive-pack"),
        "manifest-only pack streamed: {log}"
    );
    assert_eq!(
        fs::read_to_string(base.join(manifest_path(&id))).unwrap(),
        manifest_bytes(&manifest),
        "the manifest landed (interrupted push completed)"
    );
}

// ---------------------------------------------------------------------------
// fallback (a): remote snapdir without the plumbing → dumb, byte-identical
// ---------------------------------------------------------------------------

#[test]
fn fallback_old_remote_snapdir_falls_back_to_dumb_byte_identically() {
    let Some(real) =
        require_snapdir("fallback_old_remote_snapdir_falls_back_to_dumb_byte_identically")
    else {
        return;
    };
    let staging = TempDir::new("fa-stage");
    let remote_root = TempDir::new("fa-remote");
    let bindir = TempDir::new("fa-bin");
    let remote_bin = TempDir::new("fa-remote-bin");
    let cache_pin = TempDir::new("fa-cachepin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path(), cache_pin.path());

    let (manifest, _id, _sums) = stage_tree(staging.path(), FILES);
    let root_dumb = remote_root.path().join("dumb");
    let root_fallback = remote_root.path().join("fallback");

    // Reference: forced dumb.
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");
    external_store(&root_dumb)
        .push(&manifest, staging.path())
        .expect("forced-dumb push");
    env.remove("SNAPDIR_SSH_NO_ACCEL");

    // Remote `snapdir` predates `version --capabilities` (any invocation
    // errors — also the hermetic stand-in for "no snapdir on the remote
    // PATH": the probe degrades to `caps none` either way). Local snapdir IS
    // available, so the dispatch genuinely reaches the caps check.
    install_old_snapdir(remote_bin.path());
    env.set(
        "FAKE_SSH_REMOTE_PATH",
        &remote_bin.path().display().to_string(),
    );
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());
    external_store(&root_fallback)
        .push(&manifest, staging.path())
        .expect("push must fall back to dumb and succeed");

    let set_dumb = relative_file_set(&root_dumb);
    assert_eq!(
        set_dumb,
        relative_file_set(&root_fallback),
        "fallback result must be byte-identical to the dumb root"
    );
    for rel in &set_dumb {
        assert_eq!(
            fs::read(root_dumb.join(rel)).unwrap(),
            fs::read(root_fallback.join(rel)).unwrap(),
            "byte-equal at {rel}"
        );
    }
}

// ---------------------------------------------------------------------------
// fallback (b): wire mismatch → dumb, plumbing never invoked
// ---------------------------------------------------------------------------

#[test]
fn fallback_wire_mismatch_falls_back_to_dumb_without_invoking_plumbing() {
    let Some(real) =
        require_snapdir("fallback_wire_mismatch_falls_back_to_dumb_without_invoking_plumbing")
    else {
        return;
    };
    let staging = TempDir::new("fw-stage");
    let remote_root = TempDir::new("fw-remote");
    let bindir = TempDir::new("fw-bin");
    let remote_bin = TempDir::new("fw-remote-bin");
    let cache_pin = TempDir::new("fw-cachepin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path(), cache_pin.path());

    // The remote advertises every cap — but on a FUTURE wire. Exact-integer
    // negotiation must refuse and go dumb.
    let sentinel = remote_bin.path().join("plumbing-invoked");
    install_caps_only_snapdir(
        remote_bin.path(),
        "snapdir 9.9.9 wire=99 caps=objects-needed,send-pack,receive-pack",
        &sentinel,
    );
    env.set(
        "FAKE_SSH_REMOTE_PATH",
        &remote_bin.path().display().to_string(),
    );
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());

    let (manifest, id, sums) = stage_tree(staging.path(), FILES);
    let base = remote_root.path().join("snap");
    external_store(&base)
        .push(&manifest, staging.path())
        .expect("wire-mismatch push must fall back to dumb and succeed");

    assert!(
        !sentinel.exists(),
        "the remote plumbing must never be invoked on a wire mismatch: {}",
        log_lines(&sentinel)
    );
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        assert_eq!(&fs::read(base.join(object_path(sum))).unwrap(), content);
    }
    assert_eq!(
        fs::read_to_string(base.join(manifest_path(&id))).unwrap(),
        manifest_bytes(&manifest)
    );
}

// ---------------------------------------------------------------------------
// fallback (c): FORCE_ACCEL without caps → designed error
// ---------------------------------------------------------------------------

#[test]
fn force_accel_without_caps_fails_naming_host_wire_caps_and_remedies() {
    let Some(real) =
        require_snapdir("force_accel_without_caps_fails_naming_host_wire_caps_and_remedies")
    else {
        return;
    };
    let staging = TempDir::new("fc-stage");
    let remote_root = TempDir::new("fc-remote");
    let bindir = TempDir::new("fc-bin");
    let remote_bin = TempDir::new("fc-remote-bin");
    let cache_pin = TempDir::new("fc-cachepin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path(), cache_pin.path());

    install_old_snapdir(remote_bin.path());
    env.set(
        "FAKE_SSH_REMOTE_PATH",
        &remote_bin.path().display().to_string(),
    );
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());
    env.set("SNAPDIR_SSH_FORCE_ACCEL", "1");

    let (manifest, id, _sums) = stage_tree(staging.path(), FILES);
    let base = remote_root.path().join("snap");
    let err = external_store(&base)
        .push(&manifest, staging.path())
        .unwrap_err();
    match &err {
        StoreError::Backend { message, .. } => {
            assert!(message.contains("fakehost"), "names the host: {message}");
            assert!(
                message.contains("wire=1"),
                "names the required wire: {message}"
            );
            assert!(
                message.contains("objects-needed,receive-pack"),
                "names the required caps: {message}"
            );
            assert!(
                message.contains("unset SNAPDIR_SSH_FORCE_ACCEL"),
                "names the remedies: {message}"
            );
            assert!(
                message.contains("install or upgrade snapdir"),
                "names the remedies: {message}"
            );
        }
        other => panic!("expected Backend error, got {other:?}"),
    }
    // Nothing was pushed (the error fires before any transfer).
    assert!(
        !base.join(manifest_path(&id)).exists(),
        "FORCE_ACCEL error must abort before any transfer"
    );
}

// ---------------------------------------------------------------------------
// NO_ACCEL forces dumb even when the remote has full caps
// ---------------------------------------------------------------------------

#[test]
fn no_accel_forces_dumb_even_with_full_remote_caps() {
    let Some(real) = require_snapdir("no_accel_forces_dumb_even_with_full_remote_caps") else {
        return;
    };
    let staging = TempDir::new("na-stage");
    let remote_root = TempDir::new("na-remote");
    let bindir = TempDir::new("na-bin");
    let remote_bin = TempDir::new("na-remote-bin");
    let cache_pin = TempDir::new("na-cachepin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path(), cache_pin.path());

    // The remote advertises full wire=1 caps, but any actual plumbing
    // invocation (objects-needed / receive-pack) trips the sentinel.
    let sentinel = remote_bin.path().join("plumbing-invoked");
    install_caps_only_snapdir(
        remote_bin.path(),
        "snapdir 9.9.9 wire=1 caps=objects-needed,send-pack,receive-pack",
        &sentinel,
    );
    env.set(
        "FAKE_SSH_REMOTE_PATH",
        &remote_bin.path().display().to_string(),
    );
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");

    let (manifest, id, sums) = stage_tree(staging.path(), FILES);
    let base = remote_root.path().join("snap");
    external_store(&base)
        .push(&manifest, staging.path())
        .expect("NO_ACCEL push must succeed via the dumb path");

    assert!(
        !sentinel.exists(),
        "NO_ACCEL must keep the remote plumbing uninvoked: {}",
        log_lines(&sentinel)
    );
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        assert_eq!(&fs::read(base.join(object_path(sum))).unwrap(), content);
    }
    assert!(base.join(manifest_path(&id)).is_file());
}

// ---------------------------------------------------------------------------
// PULL_SENDALL: a warm cache still requests the FULL list
// ---------------------------------------------------------------------------

#[test]
fn pull_sendall_streams_full_list_even_with_warm_cache() {
    let Some(real) = require_snapdir("pull_sendall_streams_full_list_even_with_warm_cache") else {
        return;
    };
    let staging = TempDir::new("sa-stage");
    let remote_root = TempDir::new("sa-remote");
    let bindir = TempDir::new("sa-bin");
    let remote_bin = TempDir::new("sa-remote-bin");
    let cache = TempDir::new("sa-cache");
    let cache_pin = TempDir::new("sa-cachepin");
    let mut env = fake_remote_env(bindir.path(), remote_root.path(), cache_pin.path());

    let log = remote_bin.path().join("invocations.log");
    install_logging_snapdir(remote_bin.path(), &real, &log);
    env.set(
        "FAKE_SSH_REMOTE_PATH",
        &remote_bin.path().display().to_string(),
    );
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());

    let (manifest, _id, sums) = stage_tree(staging.path(), FILES);
    let base = remote_root.path().join("snap");
    let store = external_store(&base);
    store.push(&manifest, staging.path()).expect("accel push");

    // Warm the cache completely.
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        let obj = cache.path().join(object_path(sum));
        fs::create_dir_all(obj.parent().unwrap()).unwrap();
        fs::write(&obj, content).unwrap();
    }

    // Warm cache WITHOUT the knob: the emit-time diff is empty → the script
    // skips the probe + transfer round trips entirely.
    fs::write(&log, b"").unwrap();
    store
        .fetch_files(&manifest, cache.path())
        .expect("warm-cache fetch (no-op)");
    assert_eq!(
        log_lines(&log),
        "",
        "an empty needed set must skip every remote snapdir round trip"
    );

    // Warm cache WITH the knob: the baked FULL list is streamed anyway.
    env.set("SNAPDIR_SSH_PULL_SENDALL", "1");
    store
        .fetch_files(&manifest, cache.path())
        .expect("SENDALL fetch");
    let log = log_lines(&log);
    assert!(
        log.contains("send-pack"),
        "SENDALL must still request the full list: {log}"
    );
    // The cache stays byte-correct (receive-pack re-verified every record).
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        assert_eq!(
            &fs::read(cache.path().join(object_path(sum))).unwrap(),
            content
        );
    }
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

/// Every `ssh` process the script can spawn carries the security floor, and
/// nothing traps `INT` (the orchestrator owns it).
fn assert_skeleton_invariants(script: &str) {
    for line in script.lines() {
        if line.contains("command ssh") {
            assert!(
                line.contains("'StrictHostKeyChecking=yes'"),
                "every ssh invocation must carry the floor: {line}"
            );
        }
    }
    assert!(!script.contains("INT"), "the script must never trap INT");
}

#[test]
fn emitted_push_script_carries_combined_probe_and_both_paths() {
    let staging = TempDir::new("tp-stage");
    let (_, id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);
    let staging_dir = staging.path().display().to_string();
    let script =
        ssh_engine::get_push_script(&ssh_url(), &default_cfg(), &id, &staging_dir).unwrap();
    assert_skeleton_invariants(&script);

    // ONE combined probe round trip: the manifest test and the capability
    // query travel in the SAME _snapdir_ssh invocation.
    let probe_line = script
        .lines()
        .find(|l| l.contains("version --capabilities"))
        .expect("capability probe present");
    assert!(
        probe_line.contains("manifest=1") && probe_line.contains("test -f"),
        "manifest probe and caps probe must share one round trip: {probe_line}"
    );
    assert!(
        probe_line.contains("caps none"),
        "snapdir-less remotes must degrade inside the same probe: {probe_line}"
    );
    assert_eq!(
        script.matches("version --capabilities").count(),
        1,
        "exactly one capability probe"
    );

    // The wire check is a baked literal (emit-time constant), not a runtime
    // derivation from the remote semver.
    assert!(
        script.contains(" wire=1 "),
        "baked literal wire token: {script}"
    );

    // BOTH paths embedded; dispatch covers the four runtime branches.
    assert!(script.contains("_snapdir_dumb_push() {"));
    assert!(script.contains("_snapdir_accel_push() {"));
    assert!(script.contains("SNAPDIR_SSH_NO_ACCEL"));
    assert!(script.contains("_snapdir_caps_ok objects-needed receive-pack"));
    assert!(script.contains("SNAPDIR_SSH_FORCE_ACCEL"));

    // Local pipe end: env override consulted at runtime, graceful dumb
    // fallback when no local snapdir exists.
    assert!(script.contains("\"${SNAPDIR_SSH_LOCAL_SNAPDIR:-snapdir}\""));
    assert!(script.contains("snapdir_local_ok"));

    // Accel body: want heredoc carries every deduped F-checksum; the
    // manifest rides the pack last and is required remotely.
    for sum in &sums {
        assert!(script.contains(sum.as_str()), "want list carries {sum}");
    }
    // The remote commands travel as ONE sh_quoted argument to _snapdir_ssh
    // (nested quoting), targeting the remote base as a file:// store.
    assert!(script.contains(&format!(
        "_snapdir_ssh {}",
        sh_quote("snapdir objects-needed --store 'file:///srv/snap'")
    )));
    assert!(script.contains(&format!("--manifest-id '{id}'")));
    assert!(
        script.contains("--require-manifest"),
        "the remote receive-pack must require the pushed manifest id"
    );
    // The send-pack side hands an ABSOLUTE staging store to file://.
    assert!(
        script.contains(&format!(
            "--store {}",
            sh_quote(&format!("file://{staging_dir}"))
        )),
        "absolute staging dir handed to file://"
    );

    // FORCE_ACCEL error text: host + remedies.
    assert!(script.contains("'fakehost'"), "error names the host");
    assert!(script.contains("unset SNAPDIR_SSH_FORCE_ACCEL"));
    assert!(script.contains("install or upgrade snapdir"));

    // Stream failures never silently retry dumb.
    assert!(script.contains("retrying the push resumes incrementally"));
}

#[test]
fn emitted_fetch_script_carries_caps_probe_and_both_id_lists() {
    let staging = TempDir::new("tf-stage");
    let cache = TempDir::new("tf-cache");
    let (manifest, _id, sums) = stage_tree(staging.path(), &[("foo", b"foo\n"), ("bar", b"bar\n")]);

    // Pre-seed one object so needed != all (the two lists must differ).
    let cached = cache.path().join(object_path(&sums[0]));
    fs::create_dir_all(cached.parent().unwrap()).unwrap();
    fs::write(&cached, b"foo\n").unwrap();

    let cache_dir = cache.path().display().to_string();
    let script = ssh_engine::get_fetch_files_script(
        &ssh_url(),
        &default_cfg(),
        &manifest.to_string(),
        &cache_dir,
    )
    .unwrap();
    assert_skeleton_invariants(&script);

    // Caps-only probe (no manifest test on fetch), exactly one.
    assert_eq!(script.matches("version --capabilities").count(), 1);
    assert!(script.contains(" wire=1 "), "baked literal wire token");

    // BOTH paths and BOTH baked id lists; the dispatch picks at runtime.
    assert!(script.contains("_snapdir_dumb_fetch() {"));
    assert!(script.contains("_snapdir_accel_fetch() {"));
    assert!(script.contains("cat >\"$snapdir_tmp/ids\""));
    assert!(script.contains("cat >\"$snapdir_tmp/ids_all\""));
    assert!(script.contains("SNAPDIR_SSH_PULL_SENDALL"));
    assert!(script.contains("_snapdir_caps_ok send-pack"));
    assert!(script.contains("SNAPDIR_SSH_NO_ACCEL"));
    assert!(script.contains("\"${SNAPDIR_SSH_LOCAL_SNAPDIR:-snapdir}\""));

    // The remote streams via send-pack reading ids on ITS stdin; the LOCAL
    // (trusted) receive-pack lands them in the ABSOLUTE cache store.
    assert!(script.contains(&format!(
        "_snapdir_ssh {} <\"$snapdir_ids\"",
        sh_quote("snapdir send-pack --store 'file:///srv/snap' --ids -")
    )));
    assert!(
        script.contains(&format!(
            "receive-pack --store {}",
            sh_quote(&format!("file://{cache_dir}"))
        )),
        "absolute cache dir handed to file://"
    );

    // The needed list excludes the cached sum; the SENDALL list carries it.
    assert!(script.contains(&sums[1]), "needed sum baked");
    assert_eq!(
        script.matches(sums[0].as_str()).count(),
        1,
        "cached sum appears ONLY in the ids_all heredoc"
    );

    assert!(script.contains("retrying the fetch resumes incrementally"));
    assert!(script.contains("unset SNAPDIR_SSH_FORCE_ACCEL"));
}
