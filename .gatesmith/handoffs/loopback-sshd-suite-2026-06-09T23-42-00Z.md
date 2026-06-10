# Handoff — gate `loopback-sshd-suite` (phase 24, T2)

## Summary

REAL-OpenSSH integration suite: a self-spawned loopback `sshd` fixture (no
docker, no root) + 7 end-to-end tests driving the actual `ssh`/`sftp` clients
with the full security floor through the real external-store contract
(`ExternalStore::with_binary` → store binary → emitted script → eval).

**Fixture** (`tests/common/sshd.rs`): per-test `SshKit` — a 0700 temp dir with
ed25519 host + user keys (`ssh-keygen -q -N "" -t ed25519`), `authorized_keys`
(0600), and a `known_hosts` that grows one `[127.0.0.1]:<port> ssh-ed25519 …`
line per spawned server. `SshKit::spawn(flavor)` probes a free port
(`TcpListener` bind-to-0, drop, retry ×4 on the reuse race), writes an
absolute-path `sshd_config` (`ListenAddress 127.0.0.1`, `PasswordAuthentication
no`, `KbdInteractiveAuthentication no`, `UsePAM no`, `StrictModes no`, `PidFile
none`, `Subsystem sftp internal-sftp`), runs `sshd -D -e -f <abs>` as a
`std::process::Child` (stderr → per-instance log file), polls TCP readiness
(≤10s, child-exit aware), and kills+waits on Drop (RAII). Flavors: **Shell**
(port A; optional server-side `SetEnv PATH=…` — how the accel tests expose or
hide a remote `snapdir`, since sshd sessions inherit sshd's env, not the test
env) and **SftpOnly** (port B; `ForceCommand internal-sftp` — no usable shell,
which IS the tested property; `ChrootDirectory` needs root). Each test spawns
its own kit/server(s) — RAII over a shared `OnceLock` (statics never drop ⇒
sshd children would outlive the test binary); total suite runtime ~5s, well
under budget. Validated the config interactively on this machine (OpenSSH
10.2 sshd as non-root, `SetEnv PATH` honored) before writing the fixture.

**Tests** (`tests/loopback_sshd.rs`), env passed via the house
`ENV_LOCK`+`EnvGuard` serialization pattern (mirrors accel.rs), setting BOTH
`SNAPDIR_{SSH,SFTP}_STORE_{IDENTITY_FILE,KNOWN_HOSTS,PORT,CONNECT_TIMEOUT}`
families per `src/config.rs`:

1. **ssh:// dumb round trip** (port A, `SNAPDIR_SSH_NO_ACCEL=1`): push →
   get-manifest → fetch; sharded paths, umask-077 modes, byte-equal objects +
   manifest, no `.snapdir-*`/`.tmp.` residue.
2. **sftp:// round trip** (port A): same assertions (chmod-600 discipline).
3. **restricted-sftp-only** (port B): sftp:// round-trips fully; ssh:// push
   FAILS and nothing lands.
4. **host-key-fail-closed**: `KNOWN_HOSTS` pointing at a decoy-key entry →
   push fails (Backend); AND with
   `SNAPDIR_SSH_STORE_EXTRA_OPTS="StrictHostKeyChecking=no"` it STILL fails —
   the behavioral un-weakenable-floor proof against a live server.
5. **accel oracle over real sshd** (port A + `SetEnv PATH` → logging-wrapper
   `snapdir`; local pipe ends via `SNAPDIR_SSH_LOCAL_SNAPDIR`): forced-dumb
   vs accel pushes byte-identical (file SET + bytes + manifest bytes);
   engagement asserted from the wrapper log (`objects-needed`,
   `receive-pack`), never assumed; idempotent re-push streams NOTHING (log
   empty of plumbing); accel fetch into a cold cache (`send-pack`) lands
   byte-equal objects; get-manifest round-trips.
6. **fallback over real sshd** (shell server, `SetEnv PATH=/usr/bin:/bin`,
   premise pre-checked via direct `ssh 'command -v snapdir'`): dumb path
   completes end-to-end; `SNAPDIR_SSH_FORCE_ACCEL=1` → designed error naming
   the host, `wire=1`, and both remedies, with nothing transferred.
7. **idempotent re-push + leak check**: `TMPDIR` pinned to a short `/tmp`
   scratch (`sun_path` limit) → after push ×2 + get-manifest + fetch, the
   scratch is EMPTY (no `cm` ControlMaster sockets, no `snapdir-ssh-store.*`
   work dirs — every EXIT trap ran `ssh -O exit` + `rm -rf`).

**probe-count (item 8) — documented decision:** asserted hermetically in
`tests/accel.rs` (emitted-text pins exactly one capability probe / diff /
stream); over real sshd the client is the REAL `ssh`, so counting invocations
would mean wrapping the system client and re-proving the same emitted text.
This suite asserts accel ENGAGEMENT + the empty-log no-op re-push instead.
Rationale in the module docs.

**Skip policy** (house pattern, cf. s3_store.rs live tests): missing
sshd/ssh/ssh-keygen tooling or a missing real `snapdir` target-dir binary →
`eprintln!` skip — but PANIC under `SNAPDIR_SSH_TEST_REQUIRE=1` (CI sets it;
the suite cannot rot). The ONE allowed skip under REQUIRE: an sshd without
working `SetEnv PATH` (pre-8.7 server) skips only the two PATH-dependent
tests (5, 6) with the decision printed — macOS and CI both ship ≥8.7, so in
practice everything runs. `SNAPDIR_SSH_TEST_HOST` is documented in the
fixture docs as a reserved future external-host override (not implemented
this gate, per the gate spec).

## Files changed (lane: crates/snapdir-ssh-store/tests/ ONLY)

- `crates/snapdir-ssh-store/tests/loopback_sshd.rs` (new)
- `crates/snapdir-ssh-store/tests/common/mod.rs` (new)
- `crates/snapdir-ssh-store/tests/common/sshd.rs` (new)

No src/, no other crate, no CI files touched. Existing suites untouched.

## Local verification (this machine: macOS, OpenSSH 10.2, /usr/sbin/sshd)

`SNAPDIR_SSH_TEST_REQUIRE=1 cargo test -p snapdir-ssh-store --test loopback_sshd --locked` (final run):

```
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.40s
     Running tests/loopback_sshd.rs (target/debug/deps/loopback_sshd-a812799062b13df1)

running 7 tests
test sftp_push_get_manifest_fetch_roundtrip_over_sshd ... ok
test ssh_dumb_push_get_manifest_fetch_roundtrip_over_sshd ... ok
test host_key_mismatch_fails_closed_and_extra_opts_cannot_weaken_the_floor ... ok
test restricted_sftp_only_server_allows_sftp_and_rejects_ssh ... ok
test repush_is_idempotent_and_no_control_sockets_or_temp_dirs_leak ... ok
test fallback_without_remote_snapdir_and_force_accel_designed_error ... ok
test accel_oracle_roundtrip_and_idempotent_repush_over_sshd ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.73s

exit=0
no stray sshd children
```

- **All 7 tests RAN — zero skips** (confirmed with `--nocapture` + grep for
  `SKIP`: none). The real `snapdir` was present at `target/debug/snapdir`.
- `cargo test -p snapdir-ssh-store --locked` under REQUIRE=1: all 9 targets
  green (8 lib + 10 accel + 37 contract + 10 fake-sftp + 11 fake-ssh + 7
  loopback), run twice for flake confidence.
- `cargo fmt --check -p snapdir-ssh-store` clean;
  `cargo clippy -p snapdir-ssh-store --all-targets --locked -- -D warnings`
  clean (pedantic).
- No leaked `sshd -D` children after runs (pgrep clean).

## Reuse check

- Fixture skeleton/env/staging helpers mirror `tests/accel.rs` /
  `tests/fake_ssh_roundtrip.rs` — extracted ONCE into `tests/common/mod.rs`
  for this suite + its fixture; the three existing hermetic suites keep their
  deliberate private copies (refactoring them would churn green suites for no
  behavioral gain — documented in the module docs).
- Frozen layout via `snapdir_core::store::{object_path, manifest_path}`;
  harness via `snapdir_stores::ExternalStore::with_binary` (already a
  dev-dep); `sh_quote` reused from the crate's own `script` module. No new
  dependencies, no Cargo.toml change.
- Skip pattern follows the s3_store.rs live-test house pattern, hardened with
  the REQUIRE panic per the gate spec.

## Blockers

None. No src change was needed.

Notes for downstream gates (informational, not blockers):
- **ssh-ci-wire**: on Debian/Ubuntu runners, non-root `sshd` may require the
  privilege-separation dir (`/run/sshd`) to exist; `apt-get install
  openssh-server` normally creates it. macOS needs nothing.
- The two PATH-dependent tests document their (REQUIRE-exempt) skip if a
  server lacks `SetEnv PATH` support (OpenSSH < 8.7) — irrelevant on macOS/CI.

Ready for PM verification: YES
