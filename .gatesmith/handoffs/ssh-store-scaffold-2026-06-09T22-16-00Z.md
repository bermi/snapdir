# Handoff: ssh-store-scaffold (phase 24)

## Summary

New workspace crate `crates/snapdir-ssh-store` (lib `snapdir_ssh_store` + two
3-line bin shims `snapdir-ssh-store` / `snapdir-sftp-store`) with ALL of the
Phase-1 pure logic implemented and table-tested; NO transport engines yet (the
three contract subcommands fail closed with a clear "transport engine is not
implemented yet" on stderr, exit 1, after fully validating args/URL/env
config):

- `src/args.rs` — std-only contract grammar (mock parity: subcommand token
  position-independent, `--k v` AND `--k=v`, `-v|--version|version`
  short-circuit; per-subcommand required-option validation). Deliberate
  divergence (documented in the module docs): unknown options / stray
  positionals / missing values / duplicates are **rejected with clear errors**
  where the mock silently skips / is last-wins.
- `src/url.rs` — `SshUrl { user, host, port, base }` for
  `<scheme>://[user@]host[:port]/abs/base`: scheme must match the engine;
  `user:pw@` rejected naming IdentityFile/agent; user `[A-Za-z0-9._-]+` and
  host `[A-Za-z0-9.-]+` (neither leading `-`) or bracketed IPv6
  (`[0-9a-fA-F:]+`, brackets stripped, `host_arg()` restores them); port
  1-65535; base = literal bytes from first `/`, trailing `/` trimmed, bare `/`
  + control chars (incl. NUL) rejected, NO percent-decoding.
- `src/config.rs` — env family per engine prefix (`SNAPDIR_SSH_STORE_*` /
  `SNAPDIR_SFTP_STORE_*`): IDENTITY_FILE, PORT, KNOWN_HOSTS,
  CONNECT_TIMEOUT (10), JOBS (prefix → SNAPDIR_JOBS → SNAPDIR_MAX_JOBS → 4),
  CONTROL_PERSIST (60), UMASK ("077", octal-validated), EXTRA_OPTS
  (whitespace-split Key=Value, shell-metacharacter allowlist). Pure injectable
  `from_lookup` core + thin `from_env`. `flag_args(&SshUrl)` builds the
  ordered argv list: full security floor FIRST (BatchMode, StrictHostKeyChecking,
  PasswordAuthentication=no, KbdInteractiveAuthentication=no,
  ClearAllForwardings, pinned Kex/Ciphers/HostKeyAlgorithms, ConnectTimeout),
  then config-derived (Port — URL beats env —, User, IdentityFile +
  IdentitiesOnly only-when-set, UserKnownHostsFile), then EXTRA_OPTS LAST.
  Module docs spell out the first-obtained-wins un-weakenability invariant and
  why MACs are deliberately omitted (AEAD-only cipher floor).
- `src/version.rs` — `ssh -V` banner parser → (major, minor), fail-closed
  `check_openssh_floor` (>= 8.5, no override env), table-tested; thin
  untested `detect_openssh_floor()` wrapper that actually spawns `ssh -V`
  (stderr).
- `src/script.rs` — shared emitted-script skeleton (0700 mktemp dir,
  `_snapdir_cleanup` = `ssh -O exit` + `rm -rf`, `trap _snapdir_cleanup EXIT
  TERM HUP` — never INT; `_snapdir_ssh`/`_snapdir_sftp` wrappers with
  ControlMaster=auto + ControlPath + ControlPersist + ordered flags +
  `-- <host>`), `sh_quote` (POSIX `'\''`), `sftp_quote` (batchfile
  double-quote/backslash), collision-safe `heredoc` emitter, and
  `remote_manifest_path`/`remote_object_path` delegating to
  `snapdir_core::store::{manifest_path, object_path}`. Bash-3.2-clean.
- `tests/emitted_contract.rs` — 37 tests: args grammar + rejection table; URL
  table (IPv6 w/ and w/o port, password→keys/agent wording, bare root,
  hostile users/hosts, control chars, percent-literal, port bounds); config
  defaults/prefix-disjointness/JOBS chain/validation-naming-the-var/EXTRA_OPTS;
  floor ordering incl. the headline `EXTRA_OPTS="StrictHostKeyChecking=no"`
  strictly-after-floor + last proof, URL-port-beats-env, IdentitiesOnly
  pairing; `ssh -V` table (8.4→Err, 8.5/9.x/10.x→Ok, garbage/Windows→Err);
  skeleton textual invariants (trap set, no INT trap on any trap line, mux
  opts + ordered floor in BOTH wrappers and the cleanup, IPv6 stays
  bracketed, bash-3.2 cleanliness); quoting/heredoc edges; run dispatcher
  (version line `snapdir-<scheme>-store 1.4.0`, stdout purity on failure,
  scheme-mismatch surfaced). Env-touching tests (from_env + the dispatcher,
  which calls it) serialize on a `static ENV_LOCK: Mutex<()>` (the
  snapdir-cli `RATELIMIT_ENV_LOCK` pattern); everything else uses the pure
  `from_lookup` injection seam.

Workspace registration (root `Cargo.toml`): `members += crates/snapdir-ssh-store`
— exactly ONE changed line. Cargo.lock regenerated (+7 lines, only the new
member; zero new external crates).

## Resolved flag (PM decision 2026-06-10)

The originally-instructed `[workspace.dependencies] snapdir-ssh-store` entry
tripped the blocking `cargo shear` gate (`unused_workspace_dependency`): the
crate is a leaf — two bins, no workspace member will ever consume it as a
lib (its future integration harness dev-deps on snapdir-stores, not the
reverse). Per PM decision, BOTH that entry AND the interim
`[workspace.metadata.cargo-shear]` suppression were REMOVED; the root
Cargo.toml diff is now only the `members` addition. All checks re-run green
(see verification).

## Files changed (git diff --stat + new files)

```
 Cargo.lock              |  7 +++++++
 Cargo.toml              |  1 +
 (untracked) crates/snapdir-ssh-store/
   Cargo.toml, README.md (minimal — siblings ship one; `cargo package --list` confirmed)
   src/lib.rs (189) src/args.rs (190) src/url.rs (187) src/config.rs (283)
   src/version.rs (87) src/script.rs (139)
   src/bin/snapdir-ssh-store.rs (10) src/bin/snapdir-sftp-store.rs (10)
   tests/emitted_contract.rs (850)
```

(`.gatesmith/PM_PROMPT.md` + `journal.md` were already dirty in the worktree —
PM-owned, untouched by this lane.)

## Local verification

```
$ cargo build -p snapdir-ssh-store                       # regenerates lock
    Finished `dev` profile ... in 3.84s
$ cargo test -p snapdir-ssh-store --test emitted_contract --locked
test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
$ cargo test -p snapdir-ssh-store --locked
... 37 passed across lib/bins/integration/doc targets; 0 failed
$ cargo test --workspace --locked
exit 0; 42 suites "test result: ok"; total passed: 508; 0 failed
$ cargo fmt --check -p snapdir-ssh-store
FMT-OK
$ cargo clippy -p snapdir-ssh-store --all-targets --locked -- -D warnings
    Finished `dev` profile ... (clean; pedantic on via [lints] workspace = true)
$ cargo shear
  ✓ no issues found
$ typos crates/snapdir-ssh-store Cargo.toml
TYPOS-OK   (after s/unparseable/unparsable/ — typos is a CI gate)

# Re-run after the PM-decided removal of the workspace.dependencies entry
# + shear suppression (2026-06-10):
$ cargo test -p snapdir-ssh-store --test emitted_contract --locked
test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
$ cargo shear
  ✓ no issues found
$ cargo fmt --check -p snapdir-ssh-store
FMT-OK
$ cargo clippy -p snapdir-ssh-store --all-targets --locked -- -D warnings
    Finished `dev` profile ... (clean)
```

## Reuse check

- **Sharding reused, never reimplemented:** `script::remote_manifest_path` /
  `remote_object_path` delegate to `snapdir_core::store::{manifest_path,
  object_path}`; the test pins them to core's documented golden values AND to
  the live function output.
- **Zero deps beyond snapdir-core:** `[dependencies]` is exactly
  `snapdir-core.workspace = true`; std-only arg parsing (no clap, no
  thiserror — hand-rolled `Error(String)` newtype). Lock delta = the new
  member only.
- **Mock grammar parity:** subcommands, `--k v`/`--k=v`, `-v|--version|version`
  short-circuit, `--staging-dir`/`--cache-dir` requirements mirror
  crates/snapdir-stores/tests/snapdir-mock-store. **Divergences (deliberate,
  documented in src/args.rs module docs + test comments):** unknown
  options/stray positionals/missing values/duplicate options/duplicate
  subcommands are rejected-with-clear-error instead of the mock's silent
  skip / `true`-default / last-wins.
- **Frozen surfaces untouched:** no edits outside crates/snapdir-ssh-store +
  root Cargo.toml/Cargo.lock; oracle scripts and sibling crates unchanged.
- Spec deviations beyond the above: none. Engines (`ssh_engine.rs` /
  `sftp_engine.rs`, fake-ssh fixtures) intentionally absent — Phases 2/3/5.

Ready for PM verification: YES
