# Handoff — gate `ssh-engine-dumb` (phase 24)

## Summary

Implemented the `ssh://` DUMB transport engine (`src/ssh_engine.rs`): emit-time
manifest parsing/validation in Rust (push from `--staging-dir/.manifests/<sharded>`,
fetch from the binary's stdin), baked sharded-relpath heredocs, and bash scripts
driving `_snapdir_ssh` remote POSIX-sh commands over the shared ControlMaster
skeleton.

- **get-manifest**: `test -f` probe with exit-code discipline (0=present,
  1=absent → exact `ID '<id>' not found on --store '<store>'.` wording + exit 1,
  anything else → "failed to reach the store (ssh exit N)" + exit N — connectivity
  NEVER maps to not-found), then `cat` (stdout = manifest bytes only).
- **get-push**: manifest probe → exact `Manifest already exists on store.` no-op;
  ONE batched remote existence probe (candidates heredoc → remote `while read`
  under `umask <U>` → `$snapdir_tmp/missing`); single
  `tar -C <staging> -cf - -T missing | ssh 'mktemp -d .snapdir-incoming + tar -x
  + per-object mkdir -p && mv -f'` pipeline (atomic per object via same-fs
  rename; failure propagates under the orchestrator's pipefail); manifest LAST in
  a separate call (`mktemp` sibling in the shard dir + `cat` + `mv -f`).
- **get-fetch-files**: emit-time cache filtering; baked `checksum relpath` pairs;
  batched remote existence check emitting exact `ERROR: missing object <sum>`
  lines → cat to stderr + exit 1 BEFORE any transfer; remote `tar -cf -` saved to
  `$snapdir_tmp/objects.tar`; **allowlist gate** — `tar -tf` output vs the
  expected list via `LC_ALL=C grep -vxF -f` — any unexpected entry name aborts
  with the entry named, NO extraction (closes `../`/absolute/symlink entry
  attacks by exact-match); extract into `mktemp -d <cache>/.snapdir-incoming.XXXXXX`
  + per-pair `mkdir -p` shard + `mv -f`; ensure-no-errors epilogue re-checking
  every expected object with the exact ERROR wording.
- Dumb bodies live in `_snapdir_dumb_push()` / `_snapdir_dumb_fetch()` functions
  invoked from a one-line dispatch — the accel gate adds its probe + branch there.
- Wired `Engine::Ssh` into the lib.rs dispatch (stub removed); fixture
  `tests/fixtures/fake-ssh` (bash-3.2-clean, executable) + 11-test hermetic gate
  suite `tests/fake_ssh_roundtrip.rs`.

## Files changed

- `crates/snapdir-ssh-store/src/ssh_engine.rs` — NEW: the dumb ssh engine.
- `crates/snapdir-ssh-store/tests/fixtures/fake-ssh` — NEW: hermetic `ssh`
  stand-in (`-o` ignored, `-O exit` no-op, remote command via `sh -c`,
  stdin/stdout passthrough; knobs: `FAKE_SSH_FAIL_MATCH` → exit 255,
  `FAKE_SSH_TRUNCATE_TAR=<bytes>` → truncated extract + exit 1,
  `FAKE_SSH_EVIL_TAR=1` → hostile `payload/evil` tar).
- `crates/snapdir-ssh-store/tests/fake_ssh_roundtrip.rs` — NEW: parity
  round-trip, not-found-mapping, connectivity-not-notfound, idempotency,
  atomicity (truncate → no manifest/no finals → retry completes),
  fetch-missing-object (fails pre-transfer, exact wording, empty cache),
  tar-allowlist (entry named, nothing extracted), emitted-text invariants
  (probe→transfer→commit ordering, dumb-function seam, floor on every
  `command ssh` line, no INT trap, exact wordings, emit-time cache skip).
- `crates/snapdir-ssh-store/src/lib.rs` — Engine::Ssh dispatch (subcommand-major
  match), module list + crate docs updated.
- `crates/snapdir-ssh-store/src/sftp_engine.rs` — `validate_id`,
  `validate_local_dir`, `file_checksums` promoted to `pub(crate)` (shared with
  ssh_engine; contract semantics are engine-independent). No behavior change.
- `crates/snapdir-ssh-store/tests/emitted_contract.rs` — the stale
  `run_engines_fail_closed_until_implemented` test repurposed to
  `run_ssh_engine_validates_the_id_before_emitting_anything` (engine now exists;
  invalid id → exit 1, pure stdout, "invalid snapshot id" on stderr).

## Local verification (last 30 lines)

```
$ cargo test -p snapdir-ssh-store --test fake_ssh_roundtrip --locked
running 11 tests
test emitted_get_manifest_script_has_exact_not_found_wording_and_exit_code_discipline ... ok
test emitted_push_script_probes_then_transfers_then_commits_inside_dumb_function ... ok
test emitted_fetch_script_gates_extraction_on_the_exact_match_allowlist ... ok
test emitted_fetch_script_skips_objects_already_cached ... ok
test ssh_get_manifest_missing_id_maps_to_manifest_not_found ... ok
test ssh_connectivity_failure_is_backend_error_not_not_found ... ok
test ssh_fetch_rejects_unexpected_tar_entries_without_extracting ... ok
test ssh_push_get_manifest_fetch_roundtrip ... ok
test ssh_push_is_noop_when_manifest_already_present ... ok
test ssh_push_atomicity_truncated_transfer_leaves_no_manifest_then_retry_completes ... ok
test ssh_fetch_missing_object_fails_before_any_transfer_with_exact_error ... ok
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.17s

$ cargo test -p snapdir-ssh-store --locked
unittests src/lib.rs:            ok. 6 passed
tests/emitted_contract.rs:       ok. 37 passed
tests/fake_sftp_roundtrip.rs:    ok. 10 passed
tests/fake_ssh_roundtrip.rs:     ok. 11 passed
Doc-tests:                       ok. 0 passed
(all suites: 0 failed)

$ cargo fmt --check -p snapdir-ssh-store
(clean)

$ cargo clippy -p snapdir-ssh-store --all-targets --locked -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.74s
(no warnings)

$ git status --porcelain   # lane purity
 M crates/snapdir-ssh-store/src/lib.rs
 M crates/snapdir-ssh-store/src/sftp_engine.rs
 M crates/snapdir-ssh-store/tests/emitted_contract.rs
?? crates/snapdir-ssh-store/src/ssh_engine.rs
?? crates/snapdir-ssh-store/tests/fake_ssh_roundtrip.rs
?? crates/snapdir-ssh-store/tests/fixtures/fake-ssh
```

## Reuse check

- **Skeleton + sftp patterns mirrored**: skeleton/heredoc/sh_quote/remote-path
  helpers from `script.rs` reused untouched; ssh_engine mirrors sftp_engine's
  structure (probe_block, emit-time validation, manifest-last, fetch cleanup
  trap extension, emit-time cache filtering); emit-time validation helpers
  shared via `pub(crate)` instead of duplicated. Test harness (TempDir,
  EnvGuard+ENV_LOCK, fake_remote_env with eval-shell pinned to /bin/bash via
  PATH shadow, stage_tree, files_under) mirrors `fake_sftp_roundtrip.rs`
  (deliberately duplicated per the crate's current per-suite approach; the
  existing sftp suite is untouched and green).
- **Fixture reconciliation**: the sftp suite never stubbed `ssh` (real
  `ssh -O exit` fails silently there); `fake-ssh` sits beside `fake-sftp` and
  handles `-O exit` as a no-op, so both suites coexist.
- **tar portability**: only the portable intersection used
  (`-c -x -t -f - -C -T file`); `-T` lists are emit-time-validated literal
  `[0-9a-f/.]` sharded relpaths (no wildcard/option-injection surface on GNU
  tar or bsdtar); fetch tar is NEVER piped into extraction — saved locally and
  gated on the `grep -vxF` exact-match allowlist first. Remote command strings
  are POSIX-sh (dash-safe): no arrays, no `[[`, no bashisms.
- **Accel seam**: dumb bodies are `_snapdir_dumb_push` / `_snapdir_dumb_fetch`
  shell functions invoked from a single trailing dispatch line — the
  `ssh-accel` gate inserts its capability probe + branch at that call site
  without touching the bodies (documented in the module docs).

## Blockers

None.

Ready for PM verification: YES
