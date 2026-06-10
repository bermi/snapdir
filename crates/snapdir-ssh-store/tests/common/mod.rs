//! Shared harness for the loopback-sshd integration suite
//! (`tests/loopback_sshd.rs`): temp dirs, the env-serialization guard,
//! manifest staging/assertion helpers, and the real-`snapdir` locator.
//!
//! The hermetic suites (`fake_ssh_roundtrip.rs`, `fake_sftp_roundtrip.rs`,
//! `accel.rs`) deliberately keep their own private copies of these helpers so
//! each stays independently readable and runnable; THIS module exists so the
//! loopback suite and its sshd fixture share one copy without churning those
//! green suites.

pub mod sshd;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use snapdir_core::manifest::{Manifest, ManifestEntry, PathType};
use snapdir_core::merkle::{directory_checksum, Blake3Hasher, Hasher};
use snapdir_core::snapshot_id;
use snapdir_core::store::{manifest_path, object_path};
use snapdir_ssh_store::script::sh_quote;

/// Serializes env-touching tests (`SNAPDIR_*` / `TMPDIR` env are
/// process-global and flow into the spawned store binary, the eval shell,
/// and every ssh client it runs).
static ENV_LOCK: Mutex<()> = Mutex::new(());

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temp dir removed on drop (no dev-dependency needed).
///
/// Rooted at a FIXED base (`/tmp`), deliberately NOT `std::env::temp_dir()`:
/// the suite pins the process-global `TMPDIR` per test (under the env
/// lock), but tests CREATE their dirs in parallel outside the lock —
/// reading `TMPDIR` here would let one test's staging/cache land inside
/// another test's pinned scratch and be wiped along with it (the exact
/// parallel-scheduling race the PM re-run caught). The fixed short base
/// also keeps the emitted scripts' `ControlPath` sockets
/// (`$TMPDIR/snapdir-ssh-store.XXXXXX/cm`) under the ~104-byte `sun_path`
/// limit. Names carry the pid + a process-wide counter, so nothing is
/// shared across tests or test binaries.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from("/tmp").join(format!("sd-lb-{}-{tag}-{n}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Holds `ENV_LOCK` and restores every touched env var on drop (panic-safe).
pub struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    pub fn new() -> Self {
        Self {
            saved: Vec::new(),
            _lock: ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    pub fn set(&mut self, key: &str, value: &str) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            self.saved.push((key.to_owned(), std::env::var(key).ok()));
        }
        std::env::set_var(key, value);
    }

    pub fn remove(&mut self, key: &str) {
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

/// `SNAPDIR_SSH_TEST_REQUIRE=1` — set by CI so this suite can never silently
/// rot into all-skips: any environmental skip becomes a hard failure.
pub fn test_require() -> bool {
    std::env::var("SNAPDIR_SSH_TEST_REQUIRE").ok().as_deref() == Some("1")
}

/// Builds a manifest for `files` (name → content), writes the sharded
/// staging layout (objects + manifest in its CANONICAL stored byte form)
/// under `staging`, and returns the manifest, its snapshot id, and the
/// per-file checksums.
pub fn stage_tree(staging: &Path, files: &[(&str, &[u8])]) -> (Manifest, String, Vec<String>) {
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
    fs::write(&man, manifest_bytes(&manifest)).unwrap();
    (manifest, id, sums)
}

/// The serialized (stored) byte form of a manifest: text + trailing `\n`
/// (matching `file_store.rs::write_manifest`, so the accel oracle can
/// compare manifest BYTES across the dumb and pack paths).
pub fn manifest_bytes(manifest: &Manifest) -> String {
    format!("{manifest}\n")
}

/// Collects every regular file under `dir`, recursively.
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
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
pub fn relative_file_set(root: &Path) -> Vec<String> {
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

/// Writes `content` as an executable script at `path`.
pub fn write_script(path: &Path, content: &str) {
    fs::write(path, content).expect("write script");
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

/// Installs a `snapdir` into `dir` that logs every invocation's argv to
/// `log` and execs the REAL binary — accel engagement is asserted from the
/// log, never assumed.
pub fn install_logging_snapdir(dir: &Path, real: &Path, log: &Path) {
    write_script(
        &dir.join("snapdir"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>{log}\nexec {real} \"$@\"\n",
            log = sh_quote(&log.display().to_string()),
            real = sh_quote(&real.display().to_string()),
        ),
    );
}

/// Locates the REAL `snapdir` CLI binary as a sibling target-dir artifact
/// (`CARGO_BIN_EXE_snapdir` only resolves inside snapdir-cli, so the suite
/// looks for `target/<profile>/snapdir` next to its own test executable,
/// with a workspace-target fallback). `None` = not built.
pub fn snapdir_cli_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        // target/<profile>/deps/loopback_sshd-<hash> → target/<profile>/snapdir
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

/// The real `snapdir` binary, or a skip marker (`None`). Under
/// `SNAPDIR_SSH_TEST_REQUIRE=1` a missing binary PANICS instead of
/// skipping (CI runs the full workspace, where snapdir-cli builds first).
pub fn require_snapdir(test: &str) -> Option<PathBuf> {
    if let Some(bin) = snapdir_cli_binary() {
        return Some(bin);
    }
    let msg = format!(
        "{test}: the real `snapdir` binary is not in the target dir \
         (run `cargo build -p snapdir-cli` first, or `cargo test --workspace`)"
    );
    assert!(
        !test_require(),
        "SNAPDIR_SSH_TEST_REQUIRE=1 forbids skipping — {msg}"
    );
    eprintln!("SKIP {msg}");
    None
}
