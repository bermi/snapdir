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
//! (`_snapdir_dumb_push` / `_snapdir_dumb_fetch`); the main dispatch probes
//! the remote for a wire-compatible `snapdir` **at script runtime** (emit
//! time has no connection) and branches into `_snapdir_accel_push` /
//! `_snapdir_accel_fetch` when the negotiation succeeds, falling back to the
//! dumb bodies otherwise. Both paths are always embedded.
//!
//! Acceleration (runtime-negotiated, ssh:// only):
//!
//! - **Push** probes manifest presence AND capabilities in ONE round trip
//!   (`test -f …; command -v snapdir && snapdir version --capabilities ||
//!   echo 'caps none'`), then either short-circuits (manifest present),
//!   streams a SNAPPACK (`snapdir objects-needed` diff → local `snapdir
//!   send-pack | ssh 'snapdir receive-pack --require-manifest <id>'`; the
//!   manifest rides the pack LAST, committed remotely only after the
//!   verified `end` trailer), or runs `_snapdir_dumb_push`.
//! - **Fetch** uses a caps-only probe and streams `ssh 'snapdir send-pack
//!   --ids -' | snapdir receive-pack --store file://<cache>` — the remote
//!   stream is untrusted, so the LOCAL receive-pack verifies every record.
//! - **Negotiation** keys on the exact ` wire=1` token (a literal baked at
//!   emit time from this module's [`WIRE_VERSION`], pinned by test to
//!   `snapdir_stores::WIRE_VERSION` — never parsed from the semver) plus the
//!   required entries of the `caps=` comma list.
//! - **Runtime env knobs** (read by the emitted script, not this binary):
//!   `SNAPDIR_SSH_NO_ACCEL=1` forces the dumb path;
//!   `SNAPDIR_SSH_FORCE_ACCEL=1` errors instead of falling back when the
//!   remote lacks the plumbing; `SNAPDIR_SSH_PULL_SENDALL=1` makes an
//!   accelerated fetch request the FULL object list (both id lists are
//!   baked, picked at runtime); `SNAPDIR_SSH_LOCAL_SNAPDIR=<abs path>` is
//!   test/debug plumbing overriding which LOCAL `snapdir` binary anchors the
//!   pipe ends (default: `snapdir` on `PATH`; a missing local binary
//!   gracefully degrades to the dumb path).
//! - **Fallback policy**: probe/diff failures (ssh reachable, plumbing not)
//!   fall back to the dumb path — nothing has been written, the dumb path is
//!   idempotent. A failure of the send|receive STREAM itself exits nonzero
//!   with a retry hint and NEVER silently retries dumb (the failure is
//!   likely environmental; a retry resumes incrementally for free).
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

/// The SNAPPACK wire version the emitted scripts negotiate on, baked into
/// the script text as the literal ` wire=1` token (an exact integer match —
/// NEVER derived from the remote's semver at runtime).
///
/// The shipped lib deliberately depends only on `snapdir-core`, so this is a
/// local constant; the `wire_version_matches_snapdir_stores` unit test pins
/// it to `snapdir_stores::WIRE_VERSION` (a dev-dependency) so a wire bump
/// cannot silently desync the probe.
const WIRE_VERSION: u32 = 1;

/// The caps the push dispatch requires of the remote `snapdir`.
const PUSH_CAPS: &str = "objects-needed,receive-pack";

/// The cap the fetch dispatch requires of the remote `snapdir`.
const FETCH_CAPS: &str = "send-pack";

/// The OPTIONAL capability token gating the additive SNAPPACK 1Z (zstd)
/// transport encoding. Unlike [`PUSH_CAPS`]/[`FETCH_CAPS`] it is NEVER part of
/// `FORCE_ACCEL` negotiation: a peer that lacks it simply receives a v1 stream
/// (the receiver sniffs the magic and accepts both forms forever), so accel is
/// still taken — only the on-wire encoding falls back. It is a LOCAL constant
/// (the shipped lib depends only on `snapdir-core`); the
/// `zstd_cap_matches_snapdir_stores` test pins it to the
/// `snapdir_stores::WIRE_CAPS` token so a rename cannot silently desync.
const ZSTD_CAP: &str = "snappack-zstd";

/// The shell that aborts on any probe transport failure: a nonzero
/// `_snapdir_ssh` exit is connectivity (the probe's remote command itself
/// always exits 0), surfaced with the real exit code — it NEVER maps to
/// not-found or "no capabilities".
const PROBE_GUARD: &str = r#"if [ "$snapdir_probe_status" -ne 0 ]; then
  printf 'snapdir-ssh-store: failed to reach the store (ssh exit %s)\n' "$snapdir_probe_status" >&2
  exit "$snapdir_probe_status"
fi
"#;

/// The combined push probe — ONE round trip answering both "is the manifest
/// already there?" (`manifest=0|1` line) and "what plumbing does the remote
/// offer?" (a `snapdir <semver> wire=N caps=…` line, or `caps none` when
/// `snapdir` is absent or predates `version --capabilities`).
fn combined_probe_block(remote_manifest: &str) -> String {
    let probe_cmd = format!(
        "if test -f {man_q}; then echo manifest=1; else echo manifest=0; fi; \
         command -v snapdir >/dev/null 2>&1 && snapdir version --capabilities || echo 'caps none'",
        man_q = sh_quote(remote_manifest),
    );
    format!(
        "snapdir_probe_status=0\n\
         _snapdir_ssh {probe} >\"$snapdir_tmp/probe\" || snapdir_probe_status=$?\n\
         {PROBE_GUARD}",
        probe = sh_quote(&probe_cmd),
    )
}

/// The caps-only fetch probe (fetch has no manifest to test — the manifest
/// already arrived via `get-manifest-command`).
fn caps_probe_block() -> String {
    let probe_cmd = "command -v snapdir >/dev/null 2>&1 && \
                     snapdir version --capabilities || echo 'caps none'";
    format!(
        "snapdir_probe_status=0\n\
         _snapdir_ssh {probe} >\"$snapdir_tmp/probe\" || snapdir_probe_status=$?\n\
         {PROBE_GUARD}",
        probe = sh_quote(probe_cmd),
    )
}

/// The runtime accel prelude: resolves the LOCAL `snapdir` binary
/// (`SNAPDIR_SSH_LOCAL_SNAPDIR` override, else `snapdir` on `PATH`; absence
/// flips `snapdir_local_ok=0` → graceful dumb fallback) and defines
/// `_snapdir_caps_ok <cap>…`, the bash-3.2/POSIX capability check: the first
/// probe line starting `snapdir ` must carry the exact ` wire=<N>` token
/// (baked literal from [`WIRE_VERSION`]) and every required cap as a
/// word-bounded member of the `caps=` comma list.
fn accel_prelude() -> String {
    format!(
        r#"snapdir_local="${{SNAPDIR_SSH_LOCAL_SNAPDIR:-snapdir}}"
snapdir_local_ok=0
snapdir_local_zstd=0
if command -v "$snapdir_local" >/dev/null 2>&1; then
  snapdir_local_ok=1
  # Probe the LOCAL binary's caps for the zstd transport. An OLDER local
  # snapdir (e.g. 1.5.0) rejects `version --capabilities` nonzero; the
  # `|| true` keeps that from tripping `set -e`, and the empty/cap-less
  # output simply leaves snapdir_local_zstd=0 (clean v1 fallback).
  snapdir_local_caps="$("$snapdir_local" version --capabilities 2>/dev/null || true)"
  case " $snapdir_local_caps " in
  *' wire={WIRE_VERSION} '*)
    case "$snapdir_local_caps" in
    *caps=*{ZSTD_CAP}*) snapdir_local_zstd=1 ;;
    esac
    ;;
  esac
fi
_snapdir_caps_ok() {{
  snapdir_caps_line=''
  while IFS= read -r snapdir_probe_line; do
    case "$snapdir_probe_line" in
    'snapdir '*)
      snapdir_caps_line="$snapdir_probe_line"
      break
      ;;
    esac
  done <"$snapdir_tmp/probe"
  if [ -z "$snapdir_caps_line" ]; then
    return 1
  fi
  case " $snapdir_caps_line " in
  *' wire={WIRE_VERSION} '*) ;;
  *) return 1 ;;
  esac
  snapdir_caps_csv=''
  for snapdir_caps_tok in $snapdir_caps_line; do
    case "$snapdir_caps_tok" in
    caps=*) snapdir_caps_csv=",${{snapdir_caps_tok#caps=}}," ;;
    esac
  done
  for snapdir_cap in "$@"; do
    case "$snapdir_caps_csv" in
    *",$snapdir_cap,"*) ;;
    *) return 1 ;;
    esac
  done
  return 0
}}
"#
    )
}

/// The `SNAPDIR_SSH_FORCE_ACCEL=1`-but-no-caps error body (emitted inside an
/// `elif … then` arm): names the host, the required wire/caps, echoes what
/// the probe actually returned, and lists the remedies. Exit 1.
fn force_accel_error_block(host: &str, required_caps: &str) -> String {
    format!(
        r#"  {{
    printf 'snapdir-ssh-store: SNAPDIR_SSH_FORCE_ACCEL=1, but %s does not offer the accelerated plumbing\n' {host_q}
    printf 'required: snapdir on the remote PATH with wire={WIRE_VERSION} and caps %s\n' '{required_caps}'
    printf 'the probe returned:\n'
    cat "$snapdir_tmp/probe"
    printf 'remedies: install or upgrade snapdir on %s, or unset SNAPDIR_SSH_FORCE_ACCEL\n' {host_q}
  }} >&2
  exit 1
"#,
        host_q = sh_quote(host),
    )
}

/// `_snapdir_accel_push`: round trips 2+3 of the accelerated push. The baked
/// want list (the manifest's deduped F-checksums) is diffed remotely via
/// `snapdir objects-needed` (failure here → `_snapdir_dumb_push`, nothing
/// written yet), then ONE local `send-pack | ssh 'receive-pack'` pipe
/// streams the missing objects with the manifest as the LAST record — the
/// remote commits it only after the verified `end` trailer. An EMPTY missing
/// set still streams the manifest-only pack (completes an interrupted push).
/// A stream failure exits nonzero with a retry hint — never a silent dumb
/// retry mid-stream.
fn accel_push_fn(base: &str, staging_abs: &str, id: &str, checksums: &[String]) -> String {
    let store_url_q = sh_quote(&format!("file://{base}"));
    let objects_needed_cmd = format!("snapdir objects-needed --store {store_url_q}");
    let receive_cmd = format!(
        "snapdir receive-pack --store {store_url_q} --require-manifest {}",
        sh_quote(id),
    );
    let mut body = heredoc("cat >\"$snapdir_tmp/want\"", checksums);
    let _ = write!(
        body,
        r#"if [ -s "$snapdir_tmp/want" ]; then
  snapdir_need_status=0
  _snapdir_ssh {objneed} <"$snapdir_tmp/want" >"$snapdir_tmp/need" || snapdir_need_status=$?
  if [ "$snapdir_need_status" -ne 0 ]; then
    _snapdir_dumb_push
    return 0
  fi
else
  : >"$snapdir_tmp/need"
fi
snapdir_stream_status=0
if [ "$snapdir_push_zstd" = "1" ]; then
  "$snapdir_local" send-pack --store {staging_url} --ids "$snapdir_tmp/need" --manifest-id {id_q} --pack-format zstd | _snapdir_ssh {recv} || snapdir_stream_status=$?
else
  "$snapdir_local" send-pack --store {staging_url} --ids "$snapdir_tmp/need" --manifest-id {id_q} | _snapdir_ssh {recv} || snapdir_stream_status=$?
fi
if [ "$snapdir_stream_status" -ne 0 ]; then
  printf 'snapdir-ssh-store: accelerated push stream failed (exit %s); retrying the push resumes incrementally\n' "$snapdir_stream_status" >&2
  exit "$snapdir_stream_status"
fi
"#,
        objneed = sh_quote(&objects_needed_cmd),
        staging_url = sh_quote(&format!("file://{staging_abs}")),
        id_q = sh_quote(id),
        recv = sh_quote(&receive_cmd),
    );
    format!("_snapdir_accel_push() {{\n{body}}}\n")
}

/// `_snapdir_accel_fetch`: one round trip streaming the runtime-chosen id
/// list (`$snapdir_ids`, picked by the dispatch — the emit-time cache diff,
/// or the full list under `SNAPDIR_SSH_PULL_SENDALL=1`) through `ssh
/// 'snapdir send-pack --ids -' | snapdir receive-pack --store
/// file://<cache>`. The remote stream is UNTRUSTED: the local receive-pack
/// hash-verifies every record. Stream failure exits nonzero (no silent dumb
/// fallback mid-stream).
fn accel_fetch_fn(base: &str, cache_abs: &str) -> String {
    let store_q = sh_quote(&format!("file://{base}"));
    // TWO statically-baked, fully-quoted remote send-pack variants (same
    // ids/ids_all pattern). The trailing ` --pack-format zstd` is a literal,
    // NEVER a runtime env value interpolated into the baked remote string —
    // the dispatch only CHOOSES which constant variant to send. The local
    // receive-pack sniffs the incoming magic, so v1 and 1Z are both accepted.
    let send_cmd = format!("snapdir send-pack --store {store_q} --ids -");
    let send_cmd_zstd = format!("snapdir send-pack --store {store_q} --ids - --pack-format zstd");
    format!(
        r#"_snapdir_accel_fetch() {{
snapdir_stream_status=0
if [ "$snapdir_fetch_zstd" = "1" ]; then
  _snapdir_ssh {send_zstd} <"$snapdir_ids" | "$snapdir_local" receive-pack --store {cache_url} || snapdir_stream_status=$?
else
  _snapdir_ssh {send} <"$snapdir_ids" | "$snapdir_local" receive-pack --store {cache_url} || snapdir_stream_status=$?
fi
if [ "$snapdir_stream_status" -ne 0 ]; then
  printf 'snapdir-ssh-store: accelerated fetch stream failed (exit %s); retrying the fetch resumes incrementally\n' "$snapdir_stream_status" >&2
  exit "$snapdir_stream_status"
fi
}}
"#,
        send = sh_quote(&send_cmd),
        send_zstd = sh_quote(&send_cmd_zstd),
        cache_url = sh_quote(&format!("file://{cache_abs}")),
    )
}

/// Returns `dir` made absolute against the current working directory (the
/// accel pipe ends hand it to `--store file://…`, which must be absolute;
/// the orchestrator already passes absolute staging/cache dirs — this is
/// defensive).
fn absolute_dir(dir: &str) -> String {
    let path = Path::new(dir);
    if path.is_absolute() {
        dir.to_owned()
    } else {
        std::env::current_dir().map_or_else(
            |_| dir.to_owned(),
            |cwd| cwd.join(path).display().to_string(),
        )
    }
}

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
/// Script flow: ONE combined probe round trip (manifest presence + remote
/// capabilities; present → exact no-op wording, exit 0), then the runtime
/// dispatch (see the module docs) into either `_snapdir_accel_push`
/// ([`accel_push_fn`]) or `_snapdir_dumb_push`: (a) ONE batched remote
/// existence probe (candidate relpath heredoc piped to a remote `while
/// read` loop under `umask <U>`, missing paths captured locally); (b) if
/// anything is missing, one `tar -C <staging> -cf - -T missing | ssh 'tar
/// -x into temp + mv -f'` pipeline (atomic per object via same-filesystem
/// rename; failure propagates under the orchestrator's `pipefail`); (c)
/// manifest LAST in a separate call (`mktemp` sibling + `cat` + `mv -f`).
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
    let staging_abs = absolute_dir(staging);

    let mut script = skeleton(url, cfg);
    // ONE combined round trip: manifest presence + remote capabilities.
    script.push_str(&combined_probe_block(&remote_manifest));
    // The exact no-op wording (the probe already proved the manifest is
    // present, which implies its objects are too).
    script.push_str(
        "if grep -q '^manifest=1$' \"$snapdir_tmp/probe\"; then\n  \
         echo 'Manifest already exists on store.'\n  exit 0\nfi\n",
    );
    script.push_str(&accel_prelude());

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
    script.push_str("}\n");
    script.push_str(&accel_push_fn(base, &staging_abs, id, &checksums));
    // Dispatch: NO_ACCEL (or no usable local snapdir — accel is impossible
    // without one, so degrade gracefully) → dumb; negotiated caps → accel;
    // FORCE_ACCEL without caps → designed error; else → dumb.
    let _ = write!(
        script,
        r#"snapdir_push_zstd=0
if [ "${{SNAPDIR_SSH_NO_ACCEL:-0}}" = "1" ] || [ "$snapdir_local_ok" -ne 1 ]; then
  _snapdir_dumb_push
elif _snapdir_caps_ok objects-needed receive-pack; then
  if [ "$snapdir_local_zstd" = "1" ] && _snapdir_caps_ok {zstd_cap}; then
    snapdir_push_zstd=1
  fi
  _snapdir_accel_push
elif [ "${{SNAPDIR_SSH_FORCE_ACCEL:-0}}" = "1" ]; then
{err}else
  _snapdir_dumb_push
fi
"#,
        zstd_cap = ZSTD_CAP,
        err = force_accel_error_block(&url.host_arg(), PUSH_CAPS),
    );
    Ok(script)
}

/// Emits the `get-fetch-files-command` script. The manifest text arrives on
/// the **binary's stdin** (passed here already read); its `F` entries are
/// deduped by checksum and filtered against `--cache-dir` at emit time
/// (objects already cached are not re-fetched).
///
/// Script flow: the runtime dispatch (see the module docs; the caps-only
/// probe round trip is skipped entirely when there is nothing to fetch)
/// branches into `_snapdir_accel_fetch` ([`accel_fetch_fn`], fed the
/// runtime-chosen baked id list) or `_snapdir_dumb_fetch`.
///
/// The dumb path (`_snapdir_dumb_fetch`): (a) baked `checksum relpath` pair
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
    script.push_str("}\n");

    // Accel plumbing: BOTH id lists are baked (the emit-time cache diff and
    // the full F-checksum list); the dispatch picks one at runtime.
    let cache_abs = absolute_dir(cache);
    let needed_ids: Vec<String> = needed.iter().map(|sum| (*sum).clone()).collect();
    script.push_str(&accel_prelude());
    script.push_str(&heredoc("cat >\"$snapdir_tmp/ids\"", &needed_ids));
    script.push_str(&heredoc("cat >\"$snapdir_tmp/ids_all\"", &checksums));
    script.push_str(&accel_fetch_fn(&url.base, &cache_abs));
    // Dispatch: pick the id list (SENDALL → full), then NO_ACCEL / no local
    // snapdir / nothing-to-fetch → dumb (the dumb body with an empty needed
    // set makes no remote call and its epilogue passes trivially, so the
    // probe round trip is skipped entirely); else probe caps and branch.
    let _ = write!(
        script,
        r#"snapdir_ids="$snapdir_tmp/ids"
if [ "${{SNAPDIR_SSH_PULL_SENDALL:-0}}" = "1" ]; then
  snapdir_ids="$snapdir_tmp/ids_all"
fi
snapdir_fetch_zstd=0
if [ "${{SNAPDIR_SSH_NO_ACCEL:-0}}" = "1" ] || [ "$snapdir_local_ok" -ne 1 ] || [ ! -s "$snapdir_ids" ]; then
  _snapdir_dumb_fetch
else
{probe}if _snapdir_caps_ok send-pack; then
  if [ "$snapdir_local_zstd" = "1" ] && _snapdir_caps_ok {zstd_cap}; then
    snapdir_fetch_zstd=1
  fi
  _snapdir_accel_fetch
elif [ "${{SNAPDIR_SSH_FORCE_ACCEL:-0}}" = "1" ]; then
{err}else
  _snapdir_dumb_fetch
fi
fi
"#,
        zstd_cap = ZSTD_CAP,
        probe = caps_probe_block(),
        err = force_accel_error_block(&url.host_arg(), FETCH_CAPS),
    );
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
    fn wire_version_matches_snapdir_stores() {
        // The shipped lib depends only on snapdir-core, so the wire version
        // is a local constant — this pin (against the dev-dependency that
        // OWNS the protocol) is what keeps a future wire bump from silently
        // desyncing the emitted probes.
        assert_eq!(WIRE_VERSION, snapdir_stores::WIRE_VERSION);
    }

    #[test]
    fn required_caps_are_advertised_by_the_cli_constants() {
        for cap in PUSH_CAPS.split(',').chain(FETCH_CAPS.split(',')) {
            assert!(
                snapdir_stores::WIRE_CAPS.contains(&cap),
                "required cap {cap:?} must be one the CLI advertises"
            );
        }
    }

    #[test]
    fn zstd_cap_matches_snapdir_stores() {
        // The optional zstd transport token is a local constant (the shipped
        // lib depends only on snapdir-core); this pin against the
        // dev-dependency that OWNS the cap keeps a future rename from silently
        // desyncing the runtime zstd negotiation.
        assert!(
            snapdir_stores::WIRE_CAPS.contains(&ZSTD_CAP),
            "ZSTD_CAP {ZSTD_CAP:?} must be a token the CLI advertises in WIRE_CAPS"
        );
        assert_eq!(ZSTD_CAP, "snappack-zstd");
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
