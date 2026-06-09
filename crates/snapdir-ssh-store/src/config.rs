//! Env-family configuration + the **un-weakenable security-floor** flag
//! builder.
//!
//! Each engine reads its own prefix family ([`crate::Engine::env_prefix`]):
//! `SNAPDIR_SSH_STORE_*` for `ssh://`, `SNAPDIR_SFTP_STORE_*` for `sftp://`.
//!
//! # The ordering invariant (why extras can never weaken the floor)
//!
//! OpenSSH resolves every `-o Key=Value` option **first-obtained-value-wins**
//! across the whole command line (and command-line options beat
//! `~/.ssh/config`). [`Config::flag_args`] therefore emits, in order:
//!
//! 1. the **security floor** (`BatchMode`, `StrictHostKeyChecking`, no
//!    password/keyboard-interactive auth, `ClearAllForwardings`, pinned
//!    modern-only Kex/Cipher/HostKey algorithm lists, `ConnectTimeout`) —
//!    always first;
//! 2. config-derived options (`Port` — URL beats env —, `User`,
//!    `IdentityFile` + `IdentitiesOnly`, `UserKnownHostsFile`);
//! 3. `EXTRA_OPTS` tokens — always **last**.
//!
//! So a hostile/typo'd `EXTRA_OPTS="StrictHostKeyChecking=no"` is structurally
//! inert: the floor's `=yes` was already obtained. Extras can only *add*
//! options the floor doesn't set (`ProxyJump`, `Compression`,
//! `ServerAlive*`, …).
//!
//! # Why no `MACs` pin
//!
//! Every cipher on the floor list is AEAD (chacha20-poly1305, AES-GCM), and
//! OpenSSH ignores the MAC negotiation entirely for AEAD ciphers — pinning
//! `MACs` would add a breakage surface for exactly zero security gain.

use crate::url::SshUrl;
use crate::{Engine, Error};

/// Pinned key-exchange floor: post-quantum hybrid first, then X25519.
/// All names exist in OpenSSH 8.5+ (the [`crate::version`] floor).
pub const FLOOR_KEX_ALGORITHMS: &str =
    "sntrup761x25519-sha512@openssh.com,curve25519-sha256,curve25519-sha256@libssh.org";

/// Pinned cipher floor: AEAD-only (see the module docs on why `MACs` is
/// deliberately not pinned).
pub const FLOOR_CIPHERS: &str =
    "chacha20-poly1305@openssh.com,aes256-gcm@openssh.com,aes128-gcm@openssh.com";

/// Pinned host-key floor: Ed25519 preferred, RSA only as SHA-2, ECDSA kept at
/// the tail (not broken; since the floor is un-weakenable, excluding it would
/// strand ecdsa-only hosts). `ssh-rsa` (SHA-1) and DSS are excluded.
pub const FLOOR_HOST_KEY_ALGORITHMS: &str = "ssh-ed25519-cert-v01@openssh.com,ssh-ed25519,\
     rsa-sha2-512-cert-v01@openssh.com,rsa-sha2-256-cert-v01@openssh.com,\
     rsa-sha2-512,rsa-sha2-256,\
     ecdsa-sha2-nistp256-cert-v01@openssh.com,ecdsa-sha2-nistp384-cert-v01@openssh.com,\
     ecdsa-sha2-nistp521-cert-v01@openssh.com,\
     ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521";

/// Default `CONNECT_TIMEOUT` (seconds).
pub const DEFAULT_CONNECT_TIMEOUT: u32 = 10;

/// Default `JOBS` when neither the engine family nor the global
/// `SNAPDIR_JOBS` / `SNAPDIR_MAX_JOBS` fallbacks are set.
pub const DEFAULT_JOBS: u32 = 4;

/// Default `CONTROL_PERSIST` (seconds) — the `ControlMaster` leak backstop.
pub const DEFAULT_CONTROL_PERSIST: u32 = 60;

/// Default `UMASK` for remote writes (`ssh://` engine only; the sftp engine
/// uses explicit `chmod 600` instead).
pub const DEFAULT_UMASK: &str = "077";

/// Engine configuration read from the `SNAPDIR_<SSH|SFTP>_STORE_*` env family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `IDENTITY_FILE` — private key path; also turns on `IdentitiesOnly=yes`.
    pub identity_file: Option<String>,
    /// `PORT` — remote port; a port in the store URL beats it.
    pub port: Option<u16>,
    /// `KNOWN_HOSTS` — `UserKnownHostsFile` override.
    pub known_hosts: Option<String>,
    /// `CONNECT_TIMEOUT` — seconds, default 10.
    pub connect_timeout: u32,
    /// `JOBS` — transfer parallelism; falls back to `SNAPDIR_JOBS`, then
    /// `SNAPDIR_MAX_JOBS`, then 4.
    pub jobs: u32,
    /// `CONTROL_PERSIST` — `ControlMaster` linger seconds, default 60.
    pub control_persist: u32,
    /// `UMASK` — octal string applied by the remote shell engine, default
    /// `"077"`.
    pub umask: String,
    /// `EXTRA_OPTS` — validated `Key=Value` ssh options, appended **last**
    /// (see the module docs: extras can never weaken the floor).
    pub extra_opts: Vec<String>,
}

impl Config {
    /// Reads `engine`'s env family from the process environment.
    ///
    /// # Errors
    ///
    /// Propagates the validation errors of [`Config::from_lookup`].
    pub fn from_env(engine: Engine) -> Result<Self, Error> {
        Self::from_lookup(engine, |name| std::env::var(name).ok())
    }

    /// Reads `engine`'s env family through an injected `lookup` (the pure,
    /// table-testable core of [`Config::from_env`]).
    ///
    /// # Errors
    ///
    /// Rejects unparsable numbers (`PORT`, `CONNECT_TIMEOUT`, `JOBS`,
    /// `CONTROL_PERSIST`), a non-octal `UMASK`, and malformed `EXTRA_OPTS`
    /// tokens, naming the offending variable.
    pub fn from_lookup<F>(engine: Engine, lookup: F) -> Result<Self, Error>
    where
        F: Fn(&str) -> Option<String>,
    {
        let prefix = engine.env_prefix();
        let var = |suffix: &str| format!("{prefix}{suffix}");

        let port = match lookup(&var("PORT")) {
            Some(raw) => Some(parse_port(&var("PORT"), &raw)?),
            None => None,
        };
        let connect_timeout = parse_or_default(
            &var("CONNECT_TIMEOUT"),
            lookup(&var("CONNECT_TIMEOUT")),
            DEFAULT_CONNECT_TIMEOUT,
        )?;
        let control_persist = parse_or_default(
            &var("CONTROL_PERSIST"),
            lookup(&var("CONTROL_PERSIST")),
            DEFAULT_CONTROL_PERSIST,
        )?;
        let (jobs_var, jobs_raw) = [
            var("JOBS"),
            "SNAPDIR_JOBS".into(),
            "SNAPDIR_MAX_JOBS".into(),
        ]
        .into_iter()
        .find_map(|name| lookup(&name).map(|raw| (name, Some(raw))))
        .unwrap_or((var("JOBS"), None));
        let jobs = parse_or_default(&jobs_var, jobs_raw, DEFAULT_JOBS)?;
        let umask = match lookup(&var("UMASK")) {
            Some(raw) => parse_umask(&var("UMASK"), &raw)?,
            None => DEFAULT_UMASK.to_owned(),
        };
        let extra_opts = match lookup(&var("EXTRA_OPTS")) {
            Some(raw) => parse_extra_opts(&var("EXTRA_OPTS"), &raw)?,
            None => Vec::new(),
        };

        Ok(Self {
            identity_file: lookup(&var("IDENTITY_FILE")),
            port,
            known_hosts: lookup(&var("KNOWN_HOSTS")),
            connect_timeout,
            jobs,
            control_persist,
            umask,
            extra_opts,
        })
    }

    /// Builds the ordered `-o`-flag argv list for every `ssh`/`sftp`
    /// invocation: **floor first, config-derived next, `EXTRA_OPTS` last**
    /// (first-obtained-wins — see the module docs for why this ordering is
    /// the security property).
    ///
    /// Returned as alternating `["-o", "Key=Value", …]` pairs.
    #[must_use]
    pub fn flag_args(&self, url: &SshUrl) -> Vec<String> {
        let mut flags = Vec::new();
        let mut opt = |value: String| {
            flags.push("-o".to_owned());
            flags.push(value);
        };

        // 1. The security floor — ALWAYS first.
        opt("BatchMode=yes".to_owned());
        opt("StrictHostKeyChecking=yes".to_owned());
        opt("PasswordAuthentication=no".to_owned());
        opt("KbdInteractiveAuthentication=no".to_owned());
        opt("ClearAllForwardings=yes".to_owned());
        opt(format!("KexAlgorithms={FLOOR_KEX_ALGORITHMS}"));
        opt(format!("Ciphers={FLOOR_CIPHERS}"));
        opt(format!("HostKeyAlgorithms={FLOOR_HOST_KEY_ALGORITHMS}"));
        opt(format!("ConnectTimeout={}", self.connect_timeout));

        // 2. Config-derived options. URL port beats env port.
        if let Some(port) = url.port.or(self.port) {
            opt(format!("Port={port}"));
        }
        if let Some(user) = &url.user {
            opt(format!("User={user}"));
        }
        if let Some(identity_file) = &self.identity_file {
            opt(format!("IdentityFile={identity_file}"));
            opt("IdentitiesOnly=yes".to_owned());
        }
        if let Some(known_hosts) = &self.known_hosts {
            opt(format!("UserKnownHostsFile={known_hosts}"));
        }

        // 3. EXTRA_OPTS — ALWAYS last (already validated Key=Value tokens).
        for extra in &self.extra_opts {
            opt(extra.clone());
        }

        flags
    }
}

fn parse_port(name: &str, raw: &str) -> Result<u16, Error> {
    match raw.trim().parse::<u16>() {
        Ok(0) | Err(_) => Err(Error::new(format!(
            "{name}: invalid port '{raw}' (expected 1-65535)"
        ))),
        Ok(port) => Ok(port),
    }
}

fn parse_or_default(name: &str, raw: Option<String>, default: u32) -> Result<u32, Error> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    match raw.trim().parse::<u32>() {
        Ok(0) | Err(_) => Err(Error::new(format!(
            "{name}: invalid value '{raw}' (expected a positive integer)"
        ))),
        Ok(value) => Ok(value),
    }
}

fn parse_umask(name: &str, raw: &str) -> Result<String, Error> {
    let raw = raw.trim();
    let valid = (1..=4).contains(&raw.len()) && raw.chars().all(|c| ('0'..='7').contains(&c));
    if valid {
        Ok(raw.to_owned())
    } else {
        Err(Error::new(format!(
            "{name}: invalid umask '{raw}' (expected 1-4 octal digits, e.g. 077)"
        )))
    }
}

/// Splits `EXTRA_OPTS` on whitespace into `Key=Value` tokens and validates
/// their shape: the key is an ssh option name (`[A-Za-z][A-Za-z0-9]*`) and
/// the value is drawn from a conservative allowlist that excludes every shell
/// metacharacter (quotes, `$`, backticks, `;`, `&`, `|`, redirects, globs,
/// braces, whitespace, control chars) — the tokens are re-emitted inside
/// generated bash, so the charset is the injection defense.
fn parse_extra_opts(name: &str, raw: &str) -> Result<Vec<String>, Error> {
    raw.split_whitespace()
        .map(|token| {
            let err = |why: &str| {
                Error::new(format!(
                    "{name}: invalid token '{token}' ({why}); expected \
                     whitespace-separated Key=Value ssh options"
                ))
            };
            let Some((key, value)) = token.split_once('=') else {
                return Err(err("missing '='"));
            };
            let key_ok = key.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && key.chars().all(|c| c.is_ascii_alphanumeric());
            if !key_ok {
                return Err(err("invalid option name"));
            }
            let value_ok = !value.is_empty()
                && value.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(
                            c,
                            '.' | ',' | ':' | '/' | '_' | '@' | '+' | '%' | '-' | '~' | '=' | '^'
                        )
                });
            if !value_ok {
                return Err(err(
                    "the value contains characters outside [A-Za-z0-9.,:/_@+%-~=^]",
                ));
            }
            Ok(token.to_owned())
        })
        .collect()
}
