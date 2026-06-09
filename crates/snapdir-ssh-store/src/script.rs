//! Shared emitted-script building blocks: the connection-multiplexing
//! skeleton, POSIX/sftp quoting, heredoc emission, and the remote sharded
//! path helpers (reusing `snapdir_core::store` — the frozen interop layout is
//! never reimplemented here).
//!
//! Everything emitted must be **bash-3.2-clean** (macOS ships bash 3.2): no
//! associative arrays, no `${var^^}`/`${var,,}`, no `readarray`. The
//! orchestrator wraps emitted scripts in `set -eEuo pipefail; trap 'kill 0'
//! INT; <script>` + `wait`, so the skeleton composes with `set -u` and never
//! traps `INT` itself (the orchestrator owns it).

use crate::config::Config;
use crate::url::SshUrl;

/// Quotes `s` for POSIX shells: wraps in single quotes, escaping embedded
/// single quotes as `'\''`. Safe for any byte sequence without NUL (URL and
/// config validation already rejected control characters upstream).
#[must_use]
pub fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Quotes `s` for `sftp -b` batchfile arguments: wraps in double quotes,
/// backslash-escaping embedded `"` and `\` (the quoting grammar of OpenSSH's
/// sftp batch parser).
#[must_use]
pub fn sftp_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Emits `command <<'DELIM'` feeding `lines` as a quoted (no-expansion)
/// heredoc, with a **collision-safe delimiter**: the base `SNAPDIR_EOF` grows
/// trailing underscores until it equals no line.
///
/// `lines` must not contain newlines (callers feed validated relative store
/// paths / checksums; URL and config validation rejects control characters).
#[must_use]
pub fn heredoc(command: &str, lines: &[String]) -> String {
    debug_assert!(
        lines.iter().all(|line| !line.contains('\n')),
        "heredoc lines must not contain newlines"
    );
    let mut delimiter = String::from("SNAPDIR_EOF");
    while lines.iter().any(|line| line == &delimiter) {
        delimiter.push('_');
    }
    let mut out = format!("{command} <<'{delimiter}'\n");
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&delimiter);
    out.push('\n');
    out
}

/// The remote path of a snapshot manifest under `base`, using the frozen
/// sharded layout from [`snapdir_core::store::manifest_path`].
#[must_use]
pub fn remote_manifest_path(base: &str, snapshot_id: &str) -> String {
    format!("{base}/{}", snapdir_core::store::manifest_path(snapshot_id))
}

/// The remote path of a content object under `base`, using the frozen
/// sharded layout from [`snapdir_core::store::object_path`].
#[must_use]
pub fn remote_object_path(base: &str, checksum: &str) -> String {
    format!("{base}/{}", snapdir_core::store::object_path(checksum))
}

/// Builds the shared script skeleton every emitted script starts with:
///
/// - a private `0700` temp dir (keeps the `ControlPath` socket short —
///   `sun_path` limit — and unreadable to others);
/// - `_snapdir_cleanup` (closes the `ControlMaster` via `ssh -O exit`, removes
///   the temp dir) wired to `trap … EXIT TERM HUP` — never `INT`, which the
///   orchestrator wrapper owns; `ControlPersist` self-reaps the master on
///   the signal paths bash skips the EXIT trap for;
/// - `_snapdir_ssh` / `_snapdir_sftp` wrappers carrying
///   `ControlMaster=auto` + `ControlPath` + `ControlPersist` and the full
///   ordered flag list ([`Config::flag_args`]: floor, then config, then
///   extras) ahead of `-- <host>`, so every invocation multiplexes one
///   TCP+auth handshake and none can fall below the floor.
#[must_use]
pub fn skeleton(url: &SshUrl, cfg: &Config) -> String {
    let flags = render_opt_flags(&cfg.flag_args(url));
    let host = sh_quote(&url.host_arg());
    let persist = cfg.control_persist;
    let mux = "-o ControlMaster=auto -o ControlPath=\"$snapdir_tmp/cm\"";
    format!(
        r#"snapdir_tmp="$(mktemp -d "${{TMPDIR:-/tmp}}/snapdir-ssh-store.XXXXXX")"
chmod 700 "$snapdir_tmp"
_snapdir_cleanup() {{
  command ssh {flags} -o ControlPath="$snapdir_tmp/cm" -O exit -- {host} 2>/dev/null || true
  rm -rf "$snapdir_tmp"
}}
trap _snapdir_cleanup EXIT TERM HUP
_snapdir_ssh() {{
  command ssh {mux} -o ControlPersist={persist} {flags} -- {host} "$@"
}}
_snapdir_sftp() {{
  command sftp {mux} -o ControlPersist={persist} {flags} -b "$1" -- {host}
}}
"#
    )
}

/// Renders a [`Config::flag_args`] list (alternating `-o`/token pairs) as
/// shell text, single-quoting each token.
fn render_opt_flags(flags: &[String]) -> String {
    flags
        .chunks(2)
        .map(|pair| {
            debug_assert_eq!(pair.len(), 2, "flag_args yields -o/token pairs");
            debug_assert_eq!(pair[0], "-o");
            format!("-o {}", sh_quote(&pair[1]))
        })
        .collect::<Vec<_>>()
        .join(" ")
}
