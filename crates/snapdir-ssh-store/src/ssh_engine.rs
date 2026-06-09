//! The `ssh://` transport engine (dumb path): emits bash scripts that drive
//! `_snapdir_ssh` remote shell commands over the shared `ControlMaster`
//! (the [`crate::script::skeleton`] wrapper).
//!
//! Requires a **POSIX shell** on the remote (the remote may be `dash` — no
//! bashisms in remote command strings) plus `tar`, `mktemp`, and `find`. The
//! manifest is parsed **at emit time** in Rust (push: from
//! `--staging-dir/.manifests/<sharded id>`; fetch: from the binary's own
//! stdin); every checksum is hex64-validated and the sharded relpath lists
//! are baked into the emitted script as quoted heredocs — unvalidated
//! strings never reach emitted text.
//!
//! The dumb push/fetch bodies live in shell functions
//! (`_snapdir_dumb_push` / `_snapdir_dumb_fetch`) invoked from a tiny main
//! dispatch, so the acceleration gate can later add a capability probe and
//! branch in that dispatch without touching the bodies.
//!
//! Engine invariants (mirroring the sftp engine and the mock contract):
//!
//! - **Probe before trusting absence.** The manifest probe is a
//!   `test -f <path>` with exit-code discipline: 0 = present, 1 = absent,
//!   anything else (255 = connection failure, 127 = no shell, …) surfaces
//!   the real exit code — connectivity NEVER masquerades as "not found" and
//!   never triggers a silent re-push.
//! - **Objects before manifest.** Push runs ONE batched remote existence
//!   probe, streams the missing objects in a single `tar | ssh 'tar -x'`
//!   pipeline that extracts into a remote `.snapdir-incoming.XXXXXX` temp
//!   dir and `mv`s each object into its final sharded path (same-filesystem
//!   rename — atomic per object), then commits the manifest LAST in its own
//!   call with the same mktemp+`mv -f` discipline. A killed transfer leaves
//!   no manifest and no partials at final paths; a retry is idempotent.
//! - **Never extract an untrusted tar blindly.** Fetch saves the remote tar
//!   to a local file and gates extraction on an **exact-match allowlist**:
//!   `tar -tf` output is compared against the client-generated expected-path
//!   list with `LC_ALL=C grep -vxF`; ANY unexpected entry name (which is how
//!   `../`, absolute-path, and symlink-entry attacks from a malicious remote
//!   must manifest) aborts before extraction.
//! - **Exact contract wordings.** Absence: `ID '<id>' not found on --store
//!   '<store>'.` on stderr + exit 1. Missing object: `ERROR: missing object
//!   <checksum>` (both in the pre-transfer remote check and the local
//!   ensure-no-errors epilogue). Push no-op: `Manifest already exists on
//!   store.`

use std::fmt::Write as _;
use std::path::Path;

use snapdir_core::manifest::Manifest;
use snapdir_core::store::{manifest_path, object_path};

use crate::config::Config;
use crate::script::{heredoc, remote_manifest_path, sh_quote, skeleton};
use crate::sftp_engine::{file_checksums, validate_id, validate_local_dir};
use crate::url::SshUrl;
use crate::Error;

/// Emits the `get-manifest-command` script: `test -f` probe with exit-code
/// discipline (absent → exact not-found wording on stderr + exit 1; any
/// non-0/1 exit → connectivity error with the real exit code), then
/// `cat <manifest>` — stdout carries ONLY manifest bytes.
///
/// # Errors
///
/// Rejects a snapshot id that is not 64 lowercase hex characters (ids are
/// embedded in emitted text; the charset is the injection defense).
pub fn get_manifest_script(
    url: &SshUrl,
    cfg: &Config,
    id: &str,
    store: &str,
) -> Result<String, Error> {
    validate_id(id)?;
    let remote_manifest = remote_manifest_path(&url.base, id);
    let not_found = format!("ID '{id}' not found on --store '{store}'.");

    let mut script = skeleton(url, cfg);
    script.push_str(&probe_block(&remote_manifest));
    let cat_cmd = format!("cat {}", sh_quote(&remote_manifest));
    let _ = write!(
        script,
        r#"if [ "$snapdir_manifest_present" -ne 1 ]; then
  printf '%s\n' {msg} >&2
  exit 1
fi
_snapdir_ssh {cat}
"#,
        msg = sh_quote(&not_found),
        cat = sh_quote(&cat_cmd),
    );
    Ok(script)
}

/// Emits the `get-push-command` script. The staged manifest is read **at
/// emit time** from `<staging_dir>/.manifests/<sharded id>`; its `F`-entry
/// checksums (deduped, sorted, hex64-validated) become the candidate object
/// set baked into the script.
///
/// Script flow: manifest probe (present → exact no-op wording, exit 0) →
/// `_snapdir_dumb_push`: (a) ONE batched remote existence probe (candidate
/// relpath heredoc piped to a remote `while read` loop under `umask <U>`,
/// missing paths captured locally); (b) if anything is missing, one
/// `tar -C <staging> -cf - -T missing | ssh 'tar -x into temp + mv -f'`
/// pipeline (atomic per object via same-filesystem rename; failure
/// propagates under the orchestrator's `pipefail`); (c) manifest LAST in a
/// separate call (`mktemp` sibling + `cat` + `mv -f`).
///
/// # Errors
///
/// Rejects an invalid id, a `--staging-dir` containing control characters,
/// a staged manifest that is absent or unparsable, and non-hex64 object
/// checksums.
pub fn get_push_script(
    url: &SshUrl,
    cfg: &Config,
    id: &str,
    staging_dir: &str,
) -> Result<String, Error> {
    validate_id(id)?;
    validate_local_dir("--staging-dir", staging_dir)?;
    let staging = staging_dir.trim_end_matches('/');
    let staged_manifest = format!("{staging}/{}", manifest_path(id));
    let manifest_text = std::fs::read_to_string(&staged_manifest).map_err(|e| {
        Error::new(format!(
            "staged manifest for id '{id}' not found at '{staged_manifest}': {e}"
        ))
    })?;
    let manifest = Manifest::parse(&manifest_text)
        .map_err(|e| Error::new(format!("invalid staged manifest '{staged_manifest}': {e}")))?;
    let checksums = file_checksums(&manifest)?;

    let base = &url.base;
    let umask = &cfg.umask;
    let remote_manifest = remote_manifest_path(base, id);

    let mut script = skeleton(url, cfg);
    script.push_str(&probe_block(&remote_manifest));
    // The exact no-op wording (the probe already proved the manifest is
    // present, which implies its objects are too).
    script.push_str(
        "if [ \"$snapdir_manifest_present\" -eq 1 ]; then\n  \
         echo 'Manifest already exists on store.'\n  exit 0\nfi\n",
    );

    // The dumb body, wrapped in a function so the accel gate can add a
    // capability probe + branch in the dispatch below without touching it.
    let mut body = String::new();
    if !checksums.is_empty() {
        let relpaths: Vec<String> = checksums.iter().map(|sum| object_path(sum)).collect();
        body.push_str(&heredoc("cat >\"$snapdir_tmp/candidates\"", &relpaths));
        // (a) ONE batched existence probe: candidates on the remote loop's
        // stdin, missing relpaths on its stdout. POSIX-sh only — the remote
        // shell may be dash.
        let probe_cmd = format!(
            "umask {umask} && mkdir -p {base_q} && cd {base_q} && \
             while IFS= read -r p; do [ -e \"$p\" ] || printf '%s\\n' \"$p\"; done",
            base_q = sh_quote(base),
        );
        let _ = writeln!(
            body,
            "_snapdir_ssh {} <\"$snapdir_tmp/candidates\" >\"$snapdir_tmp/missing\"",
            sh_quote(&probe_cmd),
        );
        // (b) one tar pipeline for every missing object: extract into a
        // remote same-filesystem temp dir, then mv -f each object into its
        // final sharded path (atomic rename). The relpaths are literal
        // [0-9a-f/.]-only, so `-T` is portable across GNU tar and bsdtar
        // (no wildcard or option-injection surface). Runs under the
        // orchestrator's `set -eEuo pipefail`, so either side failing fails
        // the push.
        let extract_cmd = format!(
            "umask {umask} && cd {base_q} && \
             t=$(mktemp -d .snapdir-incoming.XXXXXX) && tar -C \"$t\" -xf - && \
             (cd \"$t\" && find .objects -type f) | \
             while IFS= read -r p; do mkdir -p \"${{p%/*}}\" && mv -f \"$t/$p\" \"$p\"; done && \
             rm -rf \"$t\"",
            base_q = sh_quote(base),
        );
        let _ = writeln!(
            body,
            "if [ -s \"$snapdir_tmp/missing\" ]; then\n  \
             tar -C {staging_q} -cf - -T \"$snapdir_tmp/missing\" | _snapdir_ssh {extract}\nfi",
            staging_q = sh_quote(staging),
            extract = sh_quote(&extract_cmd),
        );
    }
    // (c) manifest LAST, in its own call: mktemp sibling in the manifest's
    // shard dir, fill from the staged manifest on stdin, mv -f into place.
    let mandir = remote_manifest
        .rsplit_once('/')
        .map_or("/", |(dir, _)| dir)
        .to_owned();
    let commit_cmd = format!(
        "umask {umask} && mkdir -p {mandir_q} && \
         t=$(mktemp {mandir_q}/.snapdir-manifest.XXXXXX) && cat >\"$t\" && \
         mv -f \"$t\" {manifest_q}",
        mandir_q = sh_quote(&mandir),
        manifest_q = sh_quote(&remote_manifest),
    );
    let _ = writeln!(
        body,
        "_snapdir_ssh {} <{}",
        sh_quote(&commit_cmd),
        sh_quote(&staged_manifest),
    );

    script.push_str("_snapdir_dumb_push() {\n");
    script.push_str(&body);
    script.push_str("}\n_snapdir_dumb_push\n");
    Ok(script)
}

/// Emits the `get-fetch-files-command` script. The manifest text arrives on
/// the **binary's stdin** (passed here already read); its `F` entries are
/// deduped by checksum and filtered against `--cache-dir` at emit time
/// (objects already cached are not re-fetched).
///
/// Script flow (`_snapdir_dumb_fetch`): (a) baked `checksum relpath` pair
/// heredoc; (b) batched remote existence check emitting exact
/// `ERROR: missing object <checksum>` lines — any line aborts on stderr
/// BEFORE any transfer; (c) one remote `tar -cf -` of the needed relpaths
/// saved to `$snapdir_tmp/objects.tar`; (d) exact-match allowlist gate
/// (`tar -tf` vs the expected list via `LC_ALL=C grep -vxF`) — any
/// unexpected entry name aborts with the entry named, NO extraction;
/// (e) extract into `mktemp -d <cache>/.snapdir-incoming.XXXXXX`, then
/// per-pair `mkdir -p` shard + `mv -f` into the cache; (f) ensure-no-errors
/// epilogue re-checking every expected object in the cache with the exact
/// ERROR wording, exit nonzero on any.
///
/// # Errors
///
/// Rejects a `--cache-dir` containing control characters, an unparsable
/// manifest, and non-hex64 checksums.
pub fn get_fetch_files_script(
    url: &SshUrl,
    cfg: &Config,
    manifest_text: &str,
    cache_dir: &str,
) -> Result<String, Error> {
    validate_local_dir("--cache-dir", cache_dir)?;
    let manifest = Manifest::parse(manifest_text)
        .map_err(|e| Error::new(format!("invalid manifest on stdin: {e}")))?;
    let checksums = file_checksums(&manifest)?;
    let cache = cache_dir.trim_end_matches('/');
    let needed: Vec<&String> = checksums
        .iter()
        .filter(|sum| !Path::new(cache).join(object_path(sum)).is_file())
        .collect();

    let mut script = skeleton(url, cfg);
    // Extend the cleanup trap so an aborted run does not strand the incoming
    // temp dir inside the user's cache (same pattern as the sftp engine).
    let _ = write!(
        script,
        r#"snapdir_cache={cache_q}
snapdir_ltmp=""
_snapdir_fetch_cleanup() {{
  if [ -n "$snapdir_ltmp" ]; then
    rm -rf "$snapdir_ltmp"
  fi
  _snapdir_cleanup
}}
trap _snapdir_fetch_cleanup EXIT TERM HUP
mkdir -p "$snapdir_cache"
"#,
        cache_q = sh_quote(cache),
    );

    let mut body = String::new();
    // (a) `<checksum> <relpath>` pairs, baked at emit time (also drives the
    // epilogue, so it is emitted even when nothing needs transferring).
    let pairs: Vec<String> = needed
        .iter()
        .map(|sum| format!("{sum} {}", object_path(sum)))
        .collect();
    body.push_str(&heredoc("cat >\"$snapdir_tmp/pairs\"", &pairs));

    if !needed.is_empty() {
        let expected: Vec<String> = needed.iter().map(|sum| object_path(sum)).collect();
        body.push_str(&heredoc("cat >\"$snapdir_tmp/expected\"", &expected));

        // (b) batched remote existence check — exact ERROR wording, fail
        // BEFORE any transfer.
        let preflight_cmd = format!(
            "cd {base_q} && while read -r sum p; do \
             [ -f \"$p\" ] || printf 'ERROR: missing object %s\\n' \"$sum\"; done",
            base_q = sh_quote(&url.base),
        );
        let _ = writeln!(
            body,
            "_snapdir_ssh {} <\"$snapdir_tmp/pairs\" >\"$snapdir_tmp/preflight-missing\"",
            sh_quote(&preflight_cmd),
        );
        body.push_str(
            r#"if [ -s "$snapdir_tmp/preflight-missing" ]; then
  cat "$snapdir_tmp/preflight-missing" >&2
  exit 1
fi
"#,
        );

        // (c) one remote tar of the needed relpaths, saved locally (NEVER
        // piped straight into extraction). The remote spool file preserves
        // tar's exit status past the cleanup `rm`.
        let tar_cmd = format!(
            "cd {base_q} && t=$(mktemp) && \
             {{ cat >\"$t\" && tar -cf - -T \"$t\"; s=$?; rm -f \"$t\"; exit $s; }}",
            base_q = sh_quote(&url.base),
        );
        let _ = writeln!(
            body,
            "_snapdir_ssh {} <\"$snapdir_tmp/expected\" >\"$snapdir_tmp/objects.tar\"",
            sh_quote(&tar_cmd),
        );

        // (d) ALLOWLIST GATE: every entry name must exactly match an
        // expected relpath. `../`, absolute paths, and symlink entries from
        // a malicious remote all fail the same exact-match property —
        // refuse to extract and name the offender.
        body.push_str(
            r#"tar -tf "$snapdir_tmp/objects.tar" >"$snapdir_tmp/entries"
if LC_ALL=C grep -vxF -f "$snapdir_tmp/expected" "$snapdir_tmp/entries" >"$snapdir_tmp/unexpected"; then
  printf 'snapdir-ssh-store: unexpected entry in remote tar (refusing to extract):\n' >&2
  cat "$snapdir_tmp/unexpected" >&2
  exit 1
fi
"#,
        );

        // (e) extract into an incoming temp dir under the cache (same
        // filesystem), then mv -f each object into its sharded final path.
        body.push_str(
            r#"snapdir_ltmp="$(mktemp -d "$snapdir_cache/.snapdir-incoming.XXXXXX")"
tar -C "$snapdir_ltmp" -xf "$snapdir_tmp/objects.tar"
while IFS= read -r snapdir_line; do
  snapdir_sum="${snapdir_line%% *}"
  snapdir_rel="${snapdir_line#* }"
  if [ -f "$snapdir_ltmp/$snapdir_rel" ]; then
    mkdir -p "$snapdir_cache/${snapdir_rel%/*}"
    mv -f "$snapdir_ltmp/$snapdir_rel" "$snapdir_cache/$snapdir_rel"
  fi
done <"$snapdir_tmp/pairs"
rm -rf "$snapdir_ltmp"
snapdir_ltmp=""
"#,
        );
    }

    // (f) ensure-no-errors epilogue: re-check every needed object in the
    // cache with the exact wording; exit nonzero on any.
    body.push_str(
        r#"snapdir_missing=0
while IFS= read -r snapdir_line; do
  snapdir_sum="${snapdir_line%% *}"
  snapdir_rel="${snapdir_line#* }"
  if [ ! -f "$snapdir_cache/$snapdir_rel" ]; then
    printf 'ERROR: missing object %s\n' "$snapdir_sum" >&2
    snapdir_missing=1
  fi
done <"$snapdir_tmp/pairs"
if [ "$snapdir_missing" -ne 0 ]; then
  exit 1
fi
"#,
    );

    script.push_str("_snapdir_dumb_fetch() {\n");
    script.push_str(&body);
    script.push_str("}\n_snapdir_dumb_fetch\n");
    Ok(script)
}

/// The shared manifest probe: `_snapdir_ssh 'test -f <path>'` with strict
/// exit-code discipline — 0 → `snapdir_manifest_present=1`; 1 → absent
/// (`snapdir_manifest_present=0`); anything else (255 = connection failure,
/// 127 = no remote shell, …) → the real exit code is surfaced on stderr and
/// the script exits with it. Connectivity NEVER maps to not-found.
fn probe_block(remote_manifest: &str) -> String {
    let probe_cmd = format!("test -f {}", sh_quote(remote_manifest));
    format!(
        r#"snapdir_manifest_present=0
snapdir_probe_status=0
_snapdir_ssh {probe} || snapdir_probe_status=$?
if [ "$snapdir_probe_status" -eq 0 ]; then
  snapdir_manifest_present=1
elif [ "$snapdir_probe_status" -ne 1 ]; then
  printf 'snapdir-ssh-store: failed to reach the store (ssh exit %s)\n' "$snapdir_probe_status" >&2
  exit "$snapdir_probe_status"
fi
"#,
        probe = sh_quote(&probe_cmd),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;

    fn url() -> SshUrl {
        SshUrl::parse(Engine::Ssh, "ssh://example.com/srv/snap").unwrap()
    }

    fn cfg() -> Config {
        Config::from_lookup(Engine::Ssh, |_| None).unwrap()
    }

    #[test]
    fn probe_block_distinguishes_absent_from_unreachable() {
        let block = probe_block("/srv/snap/.manifests/abc/def/xyz");
        assert!(block.contains("test -f"));
        assert!(block.contains("-ne 1"), "non-0/1 exits are connectivity");
        assert!(block.contains("exit \"$snapdir_probe_status\""));
        assert!(
            !block.contains("not found"),
            "connectivity wording must never look like not-found"
        );
    }

    #[test]
    fn get_manifest_rejects_invalid_id() {
        let err = get_manifest_script(&url(), &cfg(), "not-hex", "ssh://example.com/srv/snap")
            .unwrap_err();
        assert!(err.to_string().contains("invalid snapshot id"));
    }

    #[test]
    fn fetch_rejects_garbage_manifest() {
        let err = get_fetch_files_script(&url(), &cfg(), "not a manifest", "/tmp/c").unwrap_err();
        assert!(err.to_string().contains("invalid manifest on stdin"));
    }
}
