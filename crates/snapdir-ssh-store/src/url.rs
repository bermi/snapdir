//! Store-URL parsing for `ssh://[user@]host[:port]/abs/base` (and `sftp://`).
//!
//! The grammar is deliberately strict — user/host charsets double as
//! shell-injection defense, because both end up inside emitted bash text:
//!
//! - the scheme must match the engine (`ssh://` vs `sftp://` are distinct
//!   stores, not aliases);
//! - embedded passwords (`user:pw@`) are rejected outright (use an
//!   `IdentityFile` key or an ssh-agent — `BatchMode=yes` would make a
//!   password unusable anyway);
//! - user: `[A-Za-z0-9._-]+`, not starting with `-` (argv-option smuggling);
//! - host: `[A-Za-z0-9.-]+`, not starting with `-`, **or** a bracketed IPv6
//!   literal `[...]` (brackets stripped for storage, content `[0-9a-fA-F:]+`);
//! - port: 1–65535;
//! - base path: the literal bytes from the first `/` (NO percent-decoding),
//!   trailing `/` trimmed, control characters (incl. NUL) rejected, and the
//!   bare root `/` rejected (refusing to scatter `.objects/` over `/`).

use crate::{Engine, Error};

/// A parsed `ssh://` / `sftp://` store URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshUrl {
    /// The remote user, if given (`-o User=<user>`).
    pub user: Option<String>,
    /// The remote host: a hostname or an IPv6 literal **without** brackets.
    pub host: String,
    /// The URL port, if given. Beats any env-configured port.
    pub port: Option<u16>,
    /// The absolute base directory on the remote, leading `/` kept and
    /// trailing `/` trimmed. Literal bytes — never percent-decoded; always
    /// shell-quoted at emission.
    pub base: String,
}

impl SshUrl {
    /// Parses `input` as a store URL for `engine`'s scheme.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic [`Error`] for a scheme mismatch, an embedded
    /// password, an invalid user/host/port, or a missing/bare/`control`-char
    /// base path. See the module docs for the exact grammar.
    pub fn parse(engine: Engine, input: &str) -> Result<Self, Error> {
        let scheme = engine.scheme();
        let Some(rest) = input
            .strip_prefix(scheme)
            .and_then(|r| r.strip_prefix("://"))
        else {
            return Err(Error::new(format!(
                "invalid store URL '{input}': expected scheme '{scheme}://' \
                 (the {} binary serves only {scheme}:// stores)",
                engine.binary_name()
            )));
        };

        let Some(slash) = rest.find('/') else {
            return Err(Error::new(format!(
                "invalid store URL '{input}': missing absolute base path \
                 (expected {scheme}://[user@]host[:port]/abs/base)"
            )));
        };
        let (authority, raw_base) = rest.split_at(slash);
        let base = parse_base(input, raw_base)?;

        let (user, host_port) = match authority.rsplit_once('@') {
            Some((user, host_port)) => (Some(parse_user(input, user)?), host_port),
            None => (None, authority),
        };
        let (host, port) = parse_host_port(input, host_port)?;

        Ok(Self {
            user,
            host,
            port,
            base,
        })
    }

    /// The host as it should appear as the ssh/sftp destination argument:
    /// IPv6 literals get their brackets back (sftp would otherwise read the
    /// colons as its `host:path` separator).
    #[must_use]
    pub fn host_arg(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }
}

fn parse_base(input: &str, raw_base: &str) -> Result<String, Error> {
    let base = raw_base.trim_end_matches('/');
    if base.is_empty() {
        return Err(Error::new(format!(
            "invalid store URL '{input}': the base path must be an absolute \
             directory below the root, not '/' itself"
        )));
    }
    if base.chars().any(char::is_control) {
        return Err(Error::new(format!(
            "invalid store URL '{input}': the base path contains control characters"
        )));
    }
    Ok(base.to_owned())
}

fn parse_user(input: &str, user: &str) -> Result<String, Error> {
    if user.contains(':') {
        return Err(Error::new(format!(
            "invalid store URL '{input}': embedded passwords (user:password@) are \
             not supported — authenticate with an SSH key (IdentityFile / \
             SNAPDIR_SSH_STORE_IDENTITY_FILE) or an ssh-agent instead"
        )));
    }
    let valid = !user.is_empty()
        && !user.starts_with('-')
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid {
        return Err(Error::new(format!(
            "invalid store URL '{input}': invalid user '{user}' \
             (allowed: [A-Za-z0-9._-]+, not starting with '-')"
        )));
    }
    Ok(user.to_owned())
}

fn parse_host_port(input: &str, host_port: &str) -> Result<(String, Option<u16>), Error> {
    if let Some(after_bracket) = host_port.strip_prefix('[') {
        // Bracketed IPv6 literal: [content][:port]
        let Some((content, tail)) = after_bracket.split_once(']') else {
            return Err(Error::new(format!(
                "invalid store URL '{input}': unterminated '[' in IPv6 host"
            )));
        };
        let valid =
            !content.is_empty() && content.chars().all(|c| c.is_ascii_hexdigit() || c == ':');
        if !valid {
            return Err(Error::new(format!(
                "invalid store URL '{input}': invalid IPv6 host '[{content}]' \
                 (allowed inside brackets: [0-9a-fA-F:]+)"
            )));
        }
        let port = match tail {
            "" => None,
            _ => match tail.strip_prefix(':') {
                Some(port) => Some(parse_port(input, port)?),
                None => {
                    return Err(Error::new(format!(
                        "invalid store URL '{input}': unexpected text after ']' \
                         (expected ':port' or the base path)"
                    )));
                }
            },
        };
        return Ok((content.to_owned(), port));
    }

    let (host, port) = match host_port.split_once(':') {
        Some((host, port)) => (host, Some(parse_port(input, port)?)),
        None => (host_port, None),
    };
    let valid = !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
    if !valid {
        return Err(Error::new(format!(
            "invalid store URL '{input}': invalid host '{host}' (allowed: \
             [A-Za-z0-9.-]+ not starting with '-', or a bracketed IPv6 literal)"
        )));
    }
    Ok((host.to_owned(), port))
}

fn parse_port(input: &str, port: &str) -> Result<u16, Error> {
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err(Error::new(format!(
            "invalid store URL '{input}': invalid port '{port}' (expected 1-65535)"
        ))),
        Ok(port) => Ok(port),
    }
}
