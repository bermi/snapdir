//! The `sftp://` transport engine: emits bash scripts that drive
//! `sftp -b <batchfile>` over the shared `ControlMaster` (the
//! [`crate::script::skeleton`]'s `_snapdir_sftp` wrapper).
//!
//! **Pure SFTP protocol** — no remote shell, no `tar`, no acceleration — so
//! every emitted script works against restricted accounts (`ForceCommand
//! internal-sftp` chroots). `sftp -b` semantics drive the whole design: an
//! unprefixed batch command that fails aborts the batch with exit 1, while a
//! `-` prefix tolerates the failure and continues.
//!
//! All batchfiles are written by the **emitted script** into `$snapdir_tmp`
//! (content baked at emit time in Rust, after validation — unvalidated
//! strings are never interpolated). The only runtime-generated batch content
//! is local destination paths under `mktemp -d` results, assembled with
//! `printf` from emit-time-quoted remote tokens.
//!
//! Engine invariants (mirroring the built-in stores and the mock contract):
//!
//! - **Probe before trusting absence.** The manifest probe (`ls
//!   <manifest_path>`) is disambiguated by a `pwd` liveness batch over the
//!   live master: connectivity failure NEVER maps to "not found" and never
//!   triggers a silent re-push.
//! - **Objects before manifest.** Push uploads missing objects in `JOBS`
//!   parallel batches (each object: `-mkdir` ancestors, `put` to a
//!   `.tmp.<nonce>` sibling, `-rm` + `rename` into place, `chmod 600`), and
//!   commits the manifest LAST in its own batch with the same discipline. A
//!   killed transfer leaves only `.tmp.<nonce>` files and no manifest; a
//!   retry is idempotent (manifest probe short-circuits; the per-object
//!   `-ls` probe skips objects already present).
//! - **Degrade to upload-all on parse anomalies.** The `-ls` existence-probe
//!   stdout is parsed in the script (dropping `sftp> `-echoed lines; a line
//!   exactly matching a probed path means present). Any unrecognized listing
//!   line degrades to uploading everything — correctness over bandwidth.
//! - **Exact contract wordings.** Absence: `ID '<id>' not found on --store
//!   '<store>'.` on stderr + exit 1. Missing object after fetch:
//!   `ERROR: missing object <checksum>`. Push no-op: `Manifest already
//!   exists on store.`

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use snapdir_core::manifest::{Manifest, PathType};
use snapdir_core::store::{manifest_path, object_path};

use crate::config::Config;
use crate::script::{
    heredoc, remote_manifest_path, remote_object_path, sftp_quote, sh_quote, skeleton,
};
use crate::url::SshUrl;
use crate::Error;

/// Emits the `get-manifest-command` script: probe the manifest over the
/// master, print the exact not-found wording on absence, otherwise `get` it
/// into `$snapdir_tmp` (sftp chatter redirected away from stdout — stdout
/// carries ONLY manifest bytes) and `cat` it.
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
    let _ = write!(
        script,
        r#"if [ "$snapdir_manifest_present" -ne 1 ]; then
  printf '%s\n' {msg} >&2
  exit 1
fi
printf 'get %s "%s/manifest"\n' {remote} "$snapdir_tmp" >"$snapdir_tmp/manifest.batch"
_snapdir_sftp "$snapdir_tmp/manifest.batch" >/dev/null
cat "$snapdir_tmp/manifest"
"#,
        msg = sh_quote(&not_found),
        remote = sh_quote(&sftp_quote(&remote_manifest)),
    );
    Ok(script)
}

/// Emits the `get-push-command` script. The staged manifest is read **at
/// emit time** from `<staging_dir>/.manifests/<sharded id>`; its `F`-entry
/// checksums (deduped, sorted, hex64-validated) become the candidate object
/// set baked into the script.
///
/// Script flow: manifest probe (present → exact no-op wording, exit 0) →
/// ONE tolerated `-ls` batch probing every candidate → runtime parse
/// (anomalies degrade to upload-all) → missing objects distributed
/// round-robin over `JOBS` parallel chunk batches (each chunk: deduped
/// sorted `-mkdir` ancestors first, then per-object
/// `put`-tmp/`-rm`/`rename`/`chmod 600`) → backgrounded with explicit
/// per-pid `wait` status collection → manifest LAST in its own final batch.
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
    let remote_manifest = remote_manifest_path(base, id);
    let nonce = emit_nonce();

    let mut script = skeleton(url, cfg);
    script.push_str(&probe_block(&remote_manifest));
    // The exact no-op wording (`store` is irrelevant to it; the probe already
    // proved the manifest is present, which implies its objects are too).
    script.push_str(
        "if [ \"$snapdir_manifest_present\" -eq 1 ]; then\n  \
         echo 'Manifest already exists on store.'\n  exit 0\nfi\n",
    );

    if !checksums.is_empty() {
        script.push_str(&push_object_tables(&checksums, base, staging, &nonce));
        script.push_str(&push_objects_runtime(cfg.jobs));
    }
    script.push_str(&push_manifest_batch(
        &staged_manifest,
        &remote_manifest,
        &nonce,
    ));
    Ok(script)
}

/// Emits the `get-fetch-files-command` script. The manifest text arrives on
/// the **binary's stdin** (passed here already read); its `F` entries are
/// deduped by checksum and filtered against `--cache-dir` at emit time
/// (objects already cached are not re-fetched).
///
/// Script flow: `ltmp=$(mktemp -d <cache>/.snapdir-incoming.XXXXXX)` →
/// chunked batches of tolerated `-get <remote> <ltmp>/<checksum>` (flat
/// names), backgrounded and waited per-pid with per-chunk exit IGNORED (the
/// post-check decides) → epilogue loop over the full needed set: present →
/// `mkdir -p` shard + `mv -f` into the cache layout; missing → exact
/// `ERROR: missing object <checksum>` on stderr → `rm -rf $ltmp` → exit
/// nonzero if anything was missing.
///
/// # Errors
///
/// Rejects a `--cache-dir` containing control characters, `"` or `\`
/// (it is interpolated into batch lines at runtime, so the charset is the
/// quoting defense), an unparsable manifest, and non-hex64 checksums.
pub fn get_fetch_files_script(
    url: &SshUrl,
    cfg: &Config,
    manifest_text: &str,
    cache_dir: &str,
) -> Result<String, Error> {
    validate_local_dir("--cache-dir", cache_dir)?;
    if cache_dir.contains('"') || cache_dir.contains('\\') {
        return Err(Error::new(format!(
            "--cache-dir '{cache_dir}' contains '\"' or '\\', which cannot be \
             quoted safely inside sftp batchfiles"
        )));
    }
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
    // temp dir inside the user's cache.
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
snapdir_ltmp="$(mktemp -d "$snapdir_cache/.snapdir-incoming.XXXXXX")"
"#,
        cache_q = sh_quote(cache),
    );

    if !needed.is_empty() {
        let jobs = usize::try_from(cfg.jobs).unwrap_or(usize::MAX).max(1);
        let chunk_count = needed.len().min(jobs);
        let mut chunks: Vec<Vec<String>> = vec![Vec::new(); chunk_count];
        for (i, sum) in needed.iter().enumerate() {
            let remote = remote_object_path(&url.base, sum);
            // `<checksum> <sftp-quoted remote>`: the remote token is quoted at
            // emit time so the runtime printf only splices it between literals.
            chunks[i % chunk_count].push(format!("{sum} {}", sftp_quote(&remote)));
        }
        for (i, chunk) in chunks.iter().enumerate() {
            script.push_str(&heredoc(
                &format!("cat >\"$snapdir_tmp/want.{i}.list\""),
                chunk,
            ));
        }
        script.push_str(
            r#"snapdir_pids=""
for snapdir_want in "$snapdir_tmp"/want.*.list; do
  [ -e "$snapdir_want" ] || continue
  snapdir_batch="${snapdir_want%.list}.batch"
  while IFS= read -r snapdir_line; do
    snapdir_sum="${snapdir_line%% *}"
    snapdir_remote="${snapdir_line#* }"
    printf -- '-get %s "%s/%s"\n' "$snapdir_remote" "$snapdir_ltmp" "$snapdir_sum" >>"$snapdir_batch"
  done <"$snapdir_want"
  _snapdir_sftp "$snapdir_batch" >/dev/null &
  snapdir_pids="$snapdir_pids $!"
done
for snapdir_pid in $snapdir_pids; do
  wait "$snapdir_pid" || true
done
"#,
        );
    }

    // Epilogue: the post-check is the verify discipline — per-chunk exits
    // were ignored above; only the presence of every needed object decides.
    let needed_lines: Vec<String> = needed
        .iter()
        .map(|sum| format!("{sum} {}", object_path(sum)))
        .collect();
    script.push_str(&heredoc("cat >\"$snapdir_tmp/needed\"", &needed_lines));
    script.push_str(
        r#"snapdir_missing=0
while IFS= read -r snapdir_line; do
  snapdir_sum="${snapdir_line%% *}"
  snapdir_rel="${snapdir_line#* }"
  if [ -f "$snapdir_ltmp/$snapdir_sum" ]; then
    mkdir -p "$snapdir_cache/${snapdir_rel%/*}"
    mv -f "$snapdir_ltmp/$snapdir_sum" "$snapdir_cache/$snapdir_rel"
  else
    printf 'ERROR: missing object %s\n' "$snapdir_sum" >&2
    snapdir_missing=1
  fi
done <"$snapdir_tmp/needed"
rm -rf "$snapdir_ltmp"
snapdir_ltmp=""
if [ "$snapdir_missing" -ne 0 ]; then
  exit 1
fi
"#,
    );
    Ok(script)
}

/// The shared manifest probe: `ls <manifest>` in a single-command batch; on
/// failure a `pwd` liveness batch over the live master disambiguates
/// "missing" (pwd ok → `snapdir_manifest_present=0`) from "unreachable"
/// (pwd fails → the probe's stderr is surfaced and the script exits with the
/// failure — connectivity NEVER maps to not-found).
fn probe_block(remote_manifest: &str) -> String {
    let probe = heredoc(
        "cat >\"$snapdir_tmp/probe.batch\"",
        &[format!("ls {}", sftp_quote(remote_manifest))],
    );
    let liveness = heredoc("cat >\"$snapdir_tmp/liveness.batch\"", &["pwd".to_owned()]);
    format!(
        r#"{probe}{liveness}snapdir_manifest_present=0
if _snapdir_sftp "$snapdir_tmp/probe.batch" >/dev/null 2>"$snapdir_tmp/probe.err"; then
  snapdir_manifest_present=1
elif ! _snapdir_sftp "$snapdir_tmp/liveness.batch" >/dev/null 2>&1; then
  cat "$snapdir_tmp/probe.err" >&2
  exit 1
fi
"#
    )
}

/// Emits the baked push tables: the raw candidate remote paths (for the
/// anomaly check), the `<idx> <remote>` spool (for distribution), the single
/// tolerated `-ls` probe batch, and per-object `-mkdir`/transfer command
/// files assembled into chunks at runtime.
fn push_object_tables(checksums: &[String], base: &str, staging: &str, nonce: &str) -> String {
    let mut out = String::new();
    let remotes: Vec<String> = checksums
        .iter()
        .map(|sum| remote_object_path(base, sum))
        .collect();

    out.push_str(&heredoc("cat >\"$snapdir_tmp/candidates\"", &remotes));
    let spool: Vec<String> = remotes
        .iter()
        .enumerate()
        .map(|(i, remote)| format!("{i} {remote}"))
        .collect();
    out.push_str(&heredoc("cat >\"$snapdir_tmp/spool\"", &spool));
    let probe: Vec<String> = remotes
        .iter()
        .map(|remote| format!("-ls {}", sftp_quote(remote)))
        .collect();
    out.push_str(&heredoc(
        "cat >\"$snapdir_tmp/probe-objects.batch\"",
        &probe,
    ));

    for (i, (sum, remote)) in checksums.iter().zip(&remotes).enumerate() {
        out.push_str(&heredoc(
            &format!("cat >\"$snapdir_tmp/o.{i}.mkdir\""),
            &mkdir_chain(remote),
        ));
        let staged = format!("{staging}/{}", object_path(sum));
        let tmp = format!("{remote}.tmp.{nonce}");
        out.push_str(&heredoc(
            &format!("cat >\"$snapdir_tmp/o.{i}.cmds\""),
            &[
                format!("put {} {}", sftp_quote(&staged), sftp_quote(&tmp)),
                format!("-rm {}", sftp_quote(remote)),
                format!("rename {} {}", sftp_quote(&tmp), sftp_quote(remote)),
                format!("chmod 600 {}", sftp_quote(remote)),
            ],
        ));
    }
    out
}

/// The runtime half of the push object transfer: parse the `-ls` probe
/// output, distribute missing objects round-robin over `jobs` chunk batches
/// (deduped sorted `-mkdir` ancestors first), run the chunks backgrounded
/// over the master, and collect every chunk's exit status with explicit
/// per-pid `wait` (the orchestrator's trailing bare `wait` finds nothing
/// unchecked).
fn push_objects_runtime(jobs: u32) -> String {
    format!(
        r#"_snapdir_sftp "$snapdir_tmp/probe-objects.batch" >"$snapdir_tmp/probe-objects.out"
grep -v '^sftp> ' "$snapdir_tmp/probe-objects.out" >"$snapdir_tmp/probe-objects.listing" || true
snapdir_upload_all=0
while IFS= read -r snapdir_line; do
  [ -n "$snapdir_line" ] || continue
  if ! grep -qxF -- "$snapdir_line" "$snapdir_tmp/candidates"; then
    snapdir_upload_all=1
    break
  fi
done <"$snapdir_tmp/probe-objects.listing"
snapdir_chunk=0
while IFS= read -r snapdir_line; do
  snapdir_idx="${{snapdir_line%% *}}"
  snapdir_remote="${{snapdir_line#* }}"
  if [ "$snapdir_upload_all" -ne 1 ] && grep -qxF -- "$snapdir_remote" "$snapdir_tmp/probe-objects.listing"; then
    continue
  fi
  cat "$snapdir_tmp/o.$snapdir_idx.mkdir" >>"$snapdir_tmp/chunk.$snapdir_chunk.mkdir"
  cat "$snapdir_tmp/o.$snapdir_idx.cmds" >>"$snapdir_tmp/chunk.$snapdir_chunk.cmds"
  snapdir_chunk=$(( (snapdir_chunk + 1) % {jobs} ))
done <"$snapdir_tmp/spool"
snapdir_pids=""
for snapdir_cmds in "$snapdir_tmp"/chunk.*.cmds; do
  [ -e "$snapdir_cmds" ] || continue
  snapdir_batch="${{snapdir_cmds%.cmds}}.batch"
  sort -u "${{snapdir_cmds%.cmds}}.mkdir" >"$snapdir_batch"
  cat "$snapdir_cmds" >>"$snapdir_batch"
  _snapdir_sftp "$snapdir_batch" >/dev/null &
  snapdir_pids="$snapdir_pids $!"
done
snapdir_failed=0
for snapdir_pid in $snapdir_pids; do
  wait "$snapdir_pid" || snapdir_failed=1
done
if [ "$snapdir_failed" -ne 0 ]; then
  echo 'snapdir-sftp-store: object upload failed' >&2
  exit 1
fi
"#
    )
}

/// The final manifest commit batch (manifest LAST, objects-before-manifest):
/// `-mkdir` ancestors, `put` to a `.tmp.<nonce>` sibling, `-rm` + `rename`
/// into place, `chmod 600`.
fn push_manifest_batch(staged_manifest: &str, remote_manifest: &str, nonce: &str) -> String {
    let tmp = format!("{remote_manifest}.tmp.{nonce}");
    let mut lines = mkdir_chain(remote_manifest);
    lines.push(format!(
        "put {} {}",
        sftp_quote(staged_manifest),
        sftp_quote(&tmp)
    ));
    lines.push(format!("-rm {}", sftp_quote(remote_manifest)));
    lines.push(format!(
        "rename {} {}",
        sftp_quote(&tmp),
        sftp_quote(remote_manifest)
    ));
    lines.push(format!("chmod 600 {}", sftp_quote(remote_manifest)));
    let mut out = heredoc("cat >\"$snapdir_tmp/manifest.batch\"", &lines);
    out.push_str("_snapdir_sftp \"$snapdir_tmp/manifest.batch\" >/dev/null\n");
    out
}

/// The tolerated `-mkdir` ancestor chain of a remote **file** path, shortest
/// prefix first (every directory component including the store base, so a
/// fresh base is created level by level — sftp `mkdir` is single-level).
fn mkdir_chain(remote_file: &str) -> Vec<String> {
    let components: Vec<&str> = remote_file.split('/').filter(|c| !c.is_empty()).collect();
    let mut prefix = String::new();
    let mut lines = Vec::new();
    for dir in &components[..components.len().saturating_sub(1)] {
        prefix.push('/');
        prefix.push_str(dir);
        lines.push(format!("-mkdir {}", sftp_quote(&prefix)));
    }
    lines
}

/// Collects the manifest's `F`-entry checksums, deduped and sorted
/// (`BTreeSet`), each validated as 64 lowercase hex characters — checksums
/// become remote path segments inside emitted text, so the charset is the
/// injection defense.
fn file_checksums(manifest: &Manifest) -> Result<Vec<String>, Error> {
    let mut set = BTreeSet::new();
    for entry in manifest.entries() {
        if entry.path_type == PathType::File {
            if !is_hex64(&entry.checksum) {
                return Err(Error::new(format!(
                    "invalid object checksum '{}' in manifest (expected 64 \
                     lowercase hex characters)",
                    entry.checksum
                )));
            }
            set.insert(entry.checksum.clone());
        }
    }
    Ok(set.into_iter().collect())
}

/// An emit-time uniqueness token for `.tmp.<nonce>` siblings: pid + clock
/// nanos. A killed transfer's orphaned tmp files never collide with a retry
/// (which mints a fresh nonce) and are trivially identifiable.
fn emit_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}.{nanos:x}", std::process::id())
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_id(id: &str) -> Result<(), Error> {
    if is_hex64(id) {
        Ok(())
    } else {
        Err(Error::new(format!(
            "invalid snapshot id '{id}' (expected 64 lowercase hex characters)"
        )))
    }
}

/// Local directories are embedded in emitted text (via `sh_quote` /
/// `sftp_quote`, which handle any byte but newlines break heredoc lines), so
/// control characters are rejected outright.
fn validate_local_dir(option: &str, value: &str) -> Result<(), Error> {
    if value.is_empty() || value.trim_end_matches('/').is_empty() {
        return Err(Error::new(format!(
            "{option} must be a non-root directory path"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(Error::new(format!(
            "{option} '{}' contains control characters",
            value.escape_debug()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mkdir_chain_walks_every_ancestor_shortest_first() {
        let sum = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
        let chain = mkdir_chain(&remote_object_path("/srv/snap", sum));
        assert_eq!(
            chain,
            vec![
                "-mkdir \"/srv\"",
                "-mkdir \"/srv/snap\"",
                "-mkdir \"/srv/snap/.objects\"",
                "-mkdir \"/srv/snap/.objects/49d\"",
                "-mkdir \"/srv/snap/.objects/49d/c87\"",
                "-mkdir \"/srv/snap/.objects/49d/c87/0df\"",
            ]
        );
    }

    #[test]
    fn hex64_validation_is_strict() {
        assert!(is_hex64(&"a".repeat(64)));
        assert!(!is_hex64(&"A".repeat(64)));
        assert!(!is_hex64(&"a".repeat(63)));
        assert!(!is_hex64(&format!("{}g", "a".repeat(63))));
        assert!(validate_id("not-an-id").is_err());
    }

    #[test]
    fn local_dir_validation_rejects_control_chars_and_root() {
        assert!(validate_local_dir("--cache-dir", "/tmp/cache").is_ok());
        assert!(validate_local_dir("--cache-dir", "").is_err());
        assert!(validate_local_dir("--cache-dir", "/").is_err());
        assert!(validate_local_dir("--cache-dir", "/tmp/\ncache").is_err());
    }
}
