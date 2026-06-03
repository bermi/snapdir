//! External-store **emit-command shim**.
//!
//! snapdir's documented extension mechanism for third-party stores is the
//! *emit-command contract*: a `snapdir-<name>-store` binary on `PATH` does not
//! transfer anything itself — instead each of its three interface subcommands
//! **prints a shell script** to stdout, and the orchestrator captures that
//! script and `eval`s it. The Rust port serves the built-in `file`/`s3`/`b2`/
//! `gcs` adapters in-process, but for any third-party adapter it preserves this
//! contract verbatim via [`ExternalStore`].
//!
//! # The emit-command contract
//!
//! The three emit subcommands and their argument/stdin protocol, as the
//! orchestrator invokes them (`_snapdir_get_fetch_snapdir_manifest_command`,
//! `_snapdir_get_fetch_snapdir_files_command`, `_snapdir_get_push_command`):
//!
//! ```text
//! <bin> get-manifest-command     --id <id> --store <store>
//! <bin> get-fetch-files-command  --id <id> --store <store> --cache-dir <dir>   (manifest on stdin)
//! <bin> get-push-command         --id <id> --staging-dir <dir> --store <store>
//! ```
//!
//! Each prints a `set -eEuo pipefail; …` script on stdout. The orchestrator
//! runs that script the way `snapdir_push` does:
//!
//! ```text
//! bash -c "set -eEuo pipefail; trap 'kill 0' INT; <emitted-script> wait"
//! ```
//!
//! Invariants the script encodes (and which this shim therefore inherits, the
//! same ones the built-in stores keep):
//!
//! - **`get-manifest-command`** prints a script that `cat`s the stored manifest
//!   to stdout, or writes `ID '<id>' not found on --store '<store>'.` to stderr
//!   and exits 1 → mapped to [`StoreError::ManifestNotFound`].
//! - **`get-push-command`** is a no-op (`echo "Manifest already exists…"`) when
//!   the manifest is already present, and otherwise emits the object-transfer
//!   commands **before** the `commit-manifest` command, so a present manifest
//!   always implies its objects are present (objects-before-manifest).
//! - **`get-fetch-files-command`** emits the object-fetch commands followed by
//!   an `ensure-no-errors` command that scans the transfer log for `ERROR:`
//!   lines and fails the transaction if any are found (verify discipline).
//!
//! # Why shelling out is allowed here
//!
//! The zero-runtime-dependency rule bans the shipped binary from shelling to
//! `b3sum`/`gcloud`/`aws`/`b2`/`sqlite3` for its *own* core work. This shim is
//! different: it spawns a **third-party** `snapdir-<name>-store` binary, which
//! is the documented, user-installed extension point. The built-in stores never
//! reach this code.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use snapdir_core::manifest::Manifest;
use snapdir_core::merkle::Blake3Hasher;
use snapdir_core::store::{Store, StoreError};

use crate::router::{resolve_adapter, Adapter};

/// The three emit-command subcommands of the store interface.
const GET_MANIFEST_COMMAND: &str = "get-manifest-command";
const GET_FETCH_FILES_COMMAND: &str = "get-fetch-files-command";
const GET_PUSH_COMMAND: &str = "get-push-command";

/// A store backed by a third-party `snapdir-<name>-store` binary, dispatched
/// through the emit-command contract.
///
/// Construct with [`ExternalStore::new`] (resolving the binary name from the
/// store URL's protocol via the router) or [`ExternalStore::with_binary`] (to
/// point at a specific binary path/name, e.g. a mock in tests).
#[derive(Debug, Clone)]
pub struct ExternalStore {
    /// The full `--store` URL, passed back to the binary verbatim.
    store_url: String,
    /// The `snapdir-<name>-store` binary to spawn (resolved on `PATH`, or an
    /// explicit path).
    binary: PathBuf,
    /// The shell used to `eval` emitted scripts (`bash` by default).
    shell: String,
}

impl ExternalStore {
    /// Builds a shim for `store_url`, resolving the third-party binary name from
    /// its protocol via [`resolve_adapter`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] if the store URL's protocol is invalid,
    /// or if it resolves to a built-in adapter (`file`/`s3`/`b2`/`gcs`) — those
    /// are served in-process and must not be routed through the shim.
    pub fn new(store_url: &str) -> Result<Self, StoreError> {
        let adapter = resolve_adapter(store_url).map_err(|e| StoreError::Backend {
            message: e.to_string(),
            source: Some(Box::new(e)),
        })?;
        match adapter {
            Adapter::External { .. } => Ok(Self::with_binary(store_url, adapter.store_binary())),
            builtin => Err(StoreError::Backend {
                message: format!(
                    "store protocol resolves to built-in adapter '{}' served in-process, \
                     not via the external-store shim",
                    builtin.name()
                ),
                source: None,
            }),
        }
    }

    /// Builds a shim that dispatches to an explicit `binary` (path or name on
    /// `PATH`) for `store_url`, bypassing protocol resolution.
    ///
    /// Useful for tests (pointing at a mock store script) and for honoring an
    /// explicit `_SNAPDIR_<PROTO>_STORE_BIN_PATH`-style override.
    #[must_use]
    pub fn with_binary(store_url: &str, binary: impl Into<PathBuf>) -> Self {
        Self {
            store_url: store_url.to_owned(),
            binary: binary.into(),
            shell: "bash".to_owned(),
        }
    }

    /// Overrides the shell used to `eval` emitted scripts (default `bash`).
    #[must_use]
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    /// The resolved store binary name/path.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Invokes the store binary's `subcommand` with `args`, optionally feeding
    /// `stdin`, and returns the emitted shell script (stdout).
    fn emit(
        &self,
        subcommand: &str,
        args: &[&OsStr],
        stdin: Option<&[u8]>,
    ) -> Result<String, StoreError> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg(subcommand)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });

        let mut child = cmd.spawn().map_err(|e| StoreError::Backend {
            message: format!("failed to spawn store binary '{}'", self.binary.display()),
            source: Some(Box::new(e)),
        })?;

        if let Some(bytes) = stdin {
            let mut sink = child.stdin.take().ok_or_else(|| StoreError::Backend {
                message: "store binary stdin unavailable".to_owned(),
                source: None,
            })?;
            sink.write_all(bytes)?;
            // Drop closes the pipe so the child sees EOF.
            drop(sink);
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(StoreError::Backend {
                message: format!(
                    "store binary '{}' {} exited with {}: {}",
                    self.binary.display(),
                    subcommand,
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                source: None,
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `eval`s an emitted shell `script` the way the orchestrator historically
    /// did (`bash -c "set -eEuo pipefail; trap 'kill 0' INT; <script> wait"`),
    /// optionally feeding `stdin`, returning the script's stdout.
    fn eval(&self, script: &str, stdin: Option<&[u8]>) -> Result<EvalOutput, StoreError> {
        let wrapped = format!("set -eEuo pipefail;\ntrap 'kill 0' INT;\n{script}\nwait");
        let mut cmd = Command::new(&self.shell);
        cmd.arg("-c")
            .arg(&wrapped)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });

        let mut child = cmd.spawn().map_err(|e| StoreError::Backend {
            message: format!("failed to spawn shell '{}'", self.shell),
            source: Some(Box::new(e)),
        })?;

        if let Some(bytes) = stdin {
            let mut sink = child.stdin.take().ok_or_else(|| StoreError::Backend {
                message: "shell stdin unavailable".to_owned(),
                source: None,
            })?;
            sink.write_all(bytes)?;
            drop(sink);
        }

        let output = child.wait_with_output()?;
        Ok(EvalOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Result of `eval`ing an emitted script.
struct EvalOutput {
    success: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Store for ExternalStore {
    fn get_manifest(&self, id: &str) -> Result<Manifest, StoreError> {
        // 1. Ask the store binary for the manifest-fetch script.
        let args: [&OsStr; 4] = [
            OsStr::new("--id"),
            OsStr::new(id),
            OsStr::new("--store"),
            OsStr::new(&self.store_url),
        ];
        let script = self.emit(GET_MANIFEST_COMMAND, &args, None)?;

        // 2. Run it. The script `cat`s the manifest on success, or writes
        //    "ID '<id>' not found on --store '<store>'." to stderr + exit 1.
        let out = self.eval(&script, None)?;
        if !out.success {
            if out.stderr.contains("not found on --store") {
                return Err(StoreError::ManifestNotFound { id: id.to_owned() });
            }
            return Err(StoreError::Backend {
                message: format!(
                    "{GET_MANIFEST_COMMAND} script for id '{id}' failed (exit {}): {}",
                    out.code.unwrap_or(-1),
                    out.stderr.trim()
                ),
                source: None,
            });
        }

        // 3. Parse + verify the manifest hashes back to `id`.
        let manifest = Manifest::parse(&out.stdout)?;
        let hasher = Blake3Hasher;
        let actual = snapdir_core::snapshot_id(&manifest, &hasher);
        if actual != id {
            return Err(StoreError::Integrity {
                address: snapdir_core::store::manifest_path(id),
                expected: id.to_owned(),
                actual,
            });
        }
        Ok(manifest)
    }

    fn fetch_files(&self, manifest: &Manifest, dest: &Path) -> Result<(), StoreError> {
        let hasher = Blake3Hasher;
        let id = snapdir_core::snapshot_id(manifest, &hasher);
        let manifest_text = manifest.to_string();

        // get-fetch-files-command reads the manifest from stdin and emits the
        // object-fetch commands + an ensure-no-errors verify command.
        let args: [&OsStr; 6] = [
            OsStr::new("--id"),
            OsStr::new(&id),
            OsStr::new("--store"),
            OsStr::new(&self.store_url),
            OsStr::new("--cache-dir"),
            dest.as_os_str(),
        ];
        let script = self.emit(
            GET_FETCH_FILES_COMMAND,
            &args,
            Some(manifest_text.as_bytes()),
        )?;

        let out = self.eval(&script, None)?;
        if !out.success {
            return Err(StoreError::Backend {
                message: format!(
                    "{GET_FETCH_FILES_COMMAND} script for id '{id}' failed (exit {}): {}",
                    out.code.unwrap_or(-1),
                    out.stderr.trim()
                ),
                source: None,
            });
        }
        // Verify discipline: the emitted ensure-no-errors guards the transfer
        // log, but scan the combined output for ERROR: lines defensively too.
        if out.stdout.contains("ERROR:") || out.stderr.contains("ERROR:") {
            return Err(StoreError::Backend {
                message: format!(
                    "{GET_FETCH_FILES_COMMAND} transaction for id '{id}' reported an error: {}",
                    out.stderr.trim()
                ),
                source: None,
            });
        }
        Ok(())
    }

    fn push(&self, manifest: &Manifest, source: &Path) -> Result<(), StoreError> {
        let hasher = Blake3Hasher;
        let id = snapdir_core::snapshot_id(manifest, &hasher);

        // get-push-command emits a no-op when the manifest already exists,
        // otherwise object-transfer commands BEFORE the commit-manifest command.
        let args: [&OsStr; 6] = [
            OsStr::new("--id"),
            OsStr::new(&id),
            OsStr::new("--staging-dir"),
            source.as_os_str(),
            OsStr::new("--store"),
            OsStr::new(&self.store_url),
        ];
        let script = self.emit(GET_PUSH_COMMAND, &args, None)?;

        let out = self.eval(&script, None)?;
        if !out.success {
            return Err(StoreError::Backend {
                message: format!(
                    "{GET_PUSH_COMMAND} script for id '{id}' failed (exit {}): {}",
                    out.code.unwrap_or(-1),
                    out.stderr.trim()
                ),
                source: None,
            });
        }
        if out.stdout.contains("ERROR:") || out.stderr.contains("ERROR:") {
            return Err(StoreError::Backend {
                message: format!(
                    "{GET_PUSH_COMMAND} transaction for id '{id}' reported an error: {}",
                    out.stderr.trim()
                ),
                source: None,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_new_rejects_builtin_adapters() {
        for url in ["file:///x", "s3://b/x", "b2://b/x", "gs://b/x"] {
            let err = ExternalStore::new(url).unwrap_err();
            assert!(
                matches!(err, StoreError::Backend { .. }),
                "expected Backend error for built-in {url}, got {err:?}"
            );
        }
    }

    #[test]
    fn shim_new_resolves_third_party_binary_from_protocol() {
        let store = ExternalStore::new("mock://bucket/base").unwrap();
        assert_eq!(store.binary(), Path::new("snapdir-mock-store"));
    }
}
