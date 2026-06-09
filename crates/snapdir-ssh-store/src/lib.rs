//! snapdir ssh/sftp external stores.
//!
//! Implements snapdir's *emit-command* external-store contract for two
//! schemes over the **system OpenSSH client** (no SSH reimplementation, zero
//! crypto dependencies):
//!
//! - `ssh://` ([`Engine::Ssh`], binary `snapdir-ssh-store`) — requires a POSIX
//!   shell on the remote host; later phases add a remote-`snapdir`
//!   acceleration path with graceful fallback.
//! - `sftp://` ([`Engine::Sftp`], binary `snapdir-sftp-store`) — speaks pure
//!   SFTP, so it works against restricted accounts (chroots with
//!   `ForceCommand internal-sftp`) with no remote shell at all.
//!
//! Per the contract (see the shim in `snapdir-stores`), each interface
//! subcommand does not transfer anything itself: it **prints a bash script**
//! on stdout and the orchestrator runs it wrapped in
//! `set -eEuo pipefail; trap 'kill 0' INT; <script>` followed by `wait`.
//! The three subcommands are:
//!
//! ```text
//! <bin> get-manifest-command     --id <id> --store <url>
//! <bin> get-push-command         --id <id> --staging-dir <dir> --store <url>
//! <bin> get-fetch-files-command  --id <id> --store <url> --cache-dir <dir>   (manifest on stdin)
//! ```
//!
//! This crate's pure layers — argument grammar ([`args`]), URL parsing
//! ([`url`]), env-family configuration + the un-weakenable security-floor
//! flag builder ([`config`]), `ssh -V` floor check ([`version`]), and the
//! emitted-script skeleton/quoting helpers ([`script`]) — are fully
//! implemented and table-tested. The transport engines (the actual
//! `ssh`/`sftp` script bodies) land in later gates; until then the
//! subcommands fail closed with a clear "not implemented" error.

use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};

pub mod args;
pub mod config;
pub mod script;
pub mod url;
pub mod version;

/// Which transport engine a binary shim runs as.
///
/// `ssh://` and `sftp://` are **distinct schemes, not aliases**: the ssh
/// engine needs a remote POSIX shell, the sftp engine speaks pure SFTP. They
/// share the URL grammar, env-family shape, security floor, and script
/// skeleton, but each reads its own env prefix and rejects the other's
/// scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    /// The `ssh://` engine (`snapdir-ssh-store`).
    Ssh,
    /// The `sftp://` engine (`snapdir-sftp-store`).
    Sftp,
}

impl Engine {
    /// The store-URL scheme this engine serves.
    #[must_use]
    pub const fn scheme(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Sftp => "sftp",
        }
    }

    /// The `snapdir-<scheme>-store` binary name this engine ships as.
    #[must_use]
    pub const fn binary_name(self) -> &'static str {
        match self {
            Self::Ssh => "snapdir-ssh-store",
            Self::Sftp => "snapdir-sftp-store",
        }
    }

    /// The env-var family prefix this engine reads its configuration from
    /// (matching the `SNAPDIR_S3_STORE_ENDPOINT_URL` naming convention).
    #[must_use]
    pub const fn env_prefix(self) -> &'static str {
        match self {
            Self::Ssh => "SNAPDIR_SSH_STORE_",
            Self::Sftp => "SNAPDIR_SFTP_STORE_",
        }
    }
}

/// Error type shared by every pure layer in this crate.
///
/// A plain diagnostic message: the binaries' only error channel is stderr +
/// exit 1 (the orchestrator surfaces stderr verbatim), so a message-carrying
/// newtype keeps the std-only crate small while staying `std::error::Error`-
/// compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    /// Builds an error from a diagnostic message.
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Entry point shared by both binary shims: parses `args` (including the
/// program name as the first element, as produced by [`std::env::args_os`]),
/// dispatches, and returns the process exit code.
///
/// Output goes to the real stdout/stderr; see [`run_with`] for the
/// writer-injected core used by tests.
pub fn run<I, R>(engine: Engine, args: I, stdin: R) -> u8
where
    I: IntoIterator<Item = OsString>,
    R: Read,
{
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    run_with(engine, args, stdin, &mut out, &mut err)
}

/// [`run`] with injectable stdout/stderr writers (the testable core).
///
/// `args` must include the program name as its first element (it is
/// skipped). `stdin` carries the manifest text for
/// `get-fetch-files-command`; the scaffold does not read it yet (the
/// transport engines, which bake stdin-derived object lists into the emitted
/// scripts, are later gates).
pub fn run_with<I, R, W, E>(engine: Engine, args: I, _stdin: R, out: &mut W, err: &mut E) -> u8
where
    I: IntoIterator<Item = OsString>,
    R: Read,
    W: Write,
    E: Write,
{
    let bin = engine.binary_name();
    let invocation = match args::parse(args.into_iter().skip(1)) {
        Ok(invocation) => invocation,
        Err(e) => {
            let _ = writeln!(err, "{bin}: {e}");
            return 1;
        }
    };
    let command = match invocation {
        args::Invocation::Version => {
            let _ = writeln!(out, "{bin} {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        args::Invocation::Command(command) => command,
    };

    // Validate everything pure up front so configuration/URL mistakes fail
    // with precise diagnostics even before the engines exist.
    let parsed_url = match url::SshUrl::parse(engine, &command.store) {
        Ok(parsed) => parsed,
        Err(e) => {
            let _ = writeln!(err, "{bin}: {e}");
            return 1;
        }
    };
    let cfg = match config::Config::from_env(engine) {
        Ok(cfg) => cfg,
        Err(e) => {
            let _ = writeln!(err, "{bin}: {e}");
            return 1;
        }
    };
    // The skeleton/flag layers are real; only the per-subcommand script
    // bodies are missing. Fail closed until the engine gates land.
    let _ = (&parsed_url, &cfg);
    let _ = writeln!(
        err,
        "{bin}: {}: the {}:// transport engine is not implemented yet",
        command.subcommand.as_str(),
        engine.scheme()
    );
    1
}
