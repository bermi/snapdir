//! Std-only parsing of the emit-command contract's argument grammar.
//!
//! Mirrors the reference grammar exercised by
//! `crates/snapdir-stores/tests/snapdir-mock-store`:
//!
//! - subcommand token: `get-manifest-command` | `get-push-command` |
//!   `get-fetch-files-command` (position-independent);
//! - options as `--key value` **and** `--key=value`;
//! - `-v` | `--version` | `version` short-circuits to a version print.
//!
//! **Documented divergences from the mock** (the mock is bash and maximally
//! tolerant; this parser is deliberately *rejected-with-clear-error* so user
//! typos surface instead of silently changing behavior):
//!
//! - unknown `--options` and stray positional tokens are **rejected** (the
//!   mock silently skips/stores them);
//! - a trailing `--key` with no value is **rejected** (the mock defaults it
//!   to `true`);
//! - repeated options and repeated subcommands are **rejected** (the mock is
//!   last-wins).

use std::ffi::OsString;

use crate::Error;

/// The three interface subcommands of the emit-command store contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subcommand {
    /// Emit the script that prints the stored manifest (or a not-found error).
    GetManifestCommand,
    /// Emit the script that pushes staged objects then the manifest (last).
    GetPushCommand,
    /// Emit the script that fetches the manifest's objects into the cache.
    GetFetchFilesCommand,
}

impl Subcommand {
    /// The literal token this subcommand is spelled as on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GetManifestCommand => "get-manifest-command",
            Self::GetPushCommand => "get-push-command",
            Self::GetFetchFilesCommand => "get-fetch-files-command",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "get-manifest-command" => Some(Self::GetManifestCommand),
            "get-push-command" => Some(Self::GetPushCommand),
            "get-fetch-files-command" => Some(Self::GetFetchFilesCommand),
            _ => None,
        }
    }
}

/// A fully validated contract invocation (subcommand + its options).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandArgs {
    /// Which script to emit.
    pub subcommand: Subcommand,
    /// `--id`: the snapshot id (required by every subcommand).
    pub id: String,
    /// `--store`: the full store URL, passed back verbatim by the
    /// orchestrator (required by every subcommand).
    pub store: String,
    /// `--staging-dir`: the local sharded staging root (required by
    /// `get-push-command`).
    pub staging_dir: Option<String>,
    /// `--cache-dir`: the local sharded cache root (required by
    /// `get-fetch-files-command`).
    pub cache_dir: Option<String>,
}

/// What a command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// `-v` / `--version` / `version`: print `snapdir-<scheme>-store <ver>`.
    Version,
    /// One of the three emit subcommands.
    Command(CommandArgs),
}

/// Parses contract arguments (program name already stripped).
///
/// # Errors
///
/// Rejects non-UTF-8 arguments, unknown/duplicate options, stray positional
/// tokens, a `--key` missing its value, a missing subcommand, and missing
/// required options for the given subcommand.
pub fn parse<I>(args: I) -> Result<Invocation, Error>
where
    I: IntoIterator<Item = OsString>,
{
    let mut subcommand: Option<Subcommand> = None;
    let mut id: Option<String> = None;
    let mut store: Option<String> = None;
    let mut staging_dir: Option<String> = None;
    let mut cache_dir: Option<String> = None;

    let mut iter = args.into_iter();
    while let Some(raw) = iter.next() {
        let arg = into_utf8(raw)?;
        // Version short-circuits wherever it appears, like the mock.
        if matches!(arg.as_str(), "-v" | "--version" | "version") {
            return Ok(Invocation::Version);
        }
        if let Some(sub) = Subcommand::from_token(&arg) {
            if subcommand.is_some() {
                return Err(Error::new(format!(
                    "multiple subcommands given (second was '{arg}')"
                )));
            }
            subcommand = Some(sub);
            continue;
        }
        let Some(rest) = arg.strip_prefix("--") else {
            // Divergence: the mock silently skips stray tokens.
            return Err(Error::new(format!("unexpected argument '{arg}'")));
        };
        let (key, value) = if let Some((key, value)) = rest.split_once('=') {
            (key.to_owned(), value.to_owned())
        } else {
            let value = iter
                .next()
                .ok_or_else(|| Error::new(format!("missing value for '--{rest}'")))?;
            (rest.to_owned(), into_utf8(value)?)
        };
        let slot = match key.as_str() {
            "id" => &mut id,
            "store" => &mut store,
            "staging-dir" => &mut staging_dir,
            "cache-dir" => &mut cache_dir,
            // Divergence: the mock stores unknown options and never reads them.
            _ => return Err(Error::new(format!("unknown option '--{key}'"))),
        };
        if slot.is_some() {
            // Divergence: the mock is last-wins on repeated options.
            return Err(Error::new(format!("option '--{key}' given more than once")));
        }
        *slot = Some(value);
    }

    let subcommand = subcommand.ok_or_else(|| {
        Error::new(
            "missing subcommand: expected get-manifest-command, get-push-command, \
             or get-fetch-files-command",
        )
    })?;
    let id = require(id, "--id")?;
    let store = require(store, "--store")?;
    match subcommand {
        Subcommand::GetPushCommand if staging_dir.is_none() => {
            return Err(missing("--staging-dir", subcommand));
        }
        Subcommand::GetFetchFilesCommand if cache_dir.is_none() => {
            return Err(missing("--cache-dir", subcommand));
        }
        _ => {}
    }

    Ok(Invocation::Command(CommandArgs {
        subcommand,
        id,
        store,
        staging_dir,
        cache_dir,
    }))
}

fn into_utf8(raw: OsString) -> Result<String, Error> {
    raw.into_string().map_err(|bad| {
        Error::new(format!(
            "argument '{}' is not valid UTF-8",
            bad.to_string_lossy()
        ))
    })
}

fn require(value: Option<String>, option: &str) -> Result<String, Error> {
    value.ok_or_else(|| Error::new(format!("missing required option {option}")))
}

fn missing(option: &str, subcommand: Subcommand) -> Error {
    Error::new(format!(
        "missing required option {option} for {}",
        subcommand.as_str()
    ))
}
