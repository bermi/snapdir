# SSH + SFTP stores for snapdir

## Context

snapdir (v1.4.0) routes any unknown `--store` scheme to an external binary `snapdir-<scheme>-store` via the emit-command contract, so `sftp://` and `ssh://` stores are drop-ins with no router change. The goal: a standards-compatible `sftp://` store that works against any modern SSH server (including `ForceCommand internal-sftp` chroots), and an `ssh://` store that adds a remote manifest-diff acceleration (O(1) round trips instead of O(N) existence probes) when a compatible `snapdir` exists on the remote, falling back gracefully. Transport is the **system OpenSSH client** with a modern-only security floor — no SSH reimplementation, zero new crypto deps. Correctness, security, performance, and verification each get explicit gates with concrete tests.

All researched facts were verified against source. **One material divergence found** (see Phase 0) and one framing correction: the fetch manifest arrives on the *emitting binary's* stdin (shim.rs:294-300), not the script's stdin — object lists are therefore parsed/validated in Rust at emit time and baked into emitted scripts.

## Decisions (explicit, as requested)

1. **`sync` over ssh: NO for v1.** `stream_store_for_adapter` (cli.rs:1559) already rejects external adapters with a clear error. A native russh `StreamStore` would import a large crypto tree into a deny.toml posture deliberately pinned to ring-backed rustls. Phased: ship the OpenSSH shim (covers push/fetch/pull), defer native russh until a sync-to-ssh use case exists. Documented limitation.
2. **Core CLI plumbing: YES** — add `snapdir objects-needed`, `snapdir send-pack`, `snapdir receive-pack` (all `#[command(hide = true)]`, like `Completions`/`Man`) and a hidden `--capabilities` flag on the existing `Version` subcommand. Justification: remote BLAKE3 verify-then-rename and manifest-last commit are impossible from a dumb shell pipeline (raw `tar -x` would honor attacker paths and skip hashing). Negotiation keys on a `wire=<int>` independent of semver. snapdir-cli is binary-only (no lib), and CI semver-checks only watches snapdir-core (untouched) → no semver friction. Workspace bumps to 1.5.0 (minor).
3. **`StreamStore::objects_needed`: add now as a defaulted trait method** (loop over `has_object`; preserves input order) in stream.rs — the `objects-needed` subcommand routes through it, so file/s3/gcs/b2 all work immediately. Batched S3/GCS overrides are a marked follow-up, not v1.
4. **Packaging: one new cargo workspace member** `crates/snapdir-ssh-store` — a lib with two `[[bin]]` targets (`snapdir-ssh-store`, `snapdir-sftp-store`), sole dependency `snapdir-core` (sharding + manifest parsing), **std-only arg parsing** (contract grammar is 3 subcommands × ≤3 flags; mirrors the mock's grammar incl. `--k=v` and `--version`). Rationale over bash: script *emission* is pure text → hermetic Rust unit tests; inherits clippy/fmt/coverage/MSRV/musl/crates.io pipeline automatically; bash-3.2 portability only matters for emitted text (which uses only bash-3.2-safe constructs).
5. **OpenSSH floor: ≥ 8.5**, checked at emit time via `ssh -V` (fail closed on older/unparseable; no override env). 8.5 carries every algorithm on the floor list (incl. `sntrup761x25519-sha512@openssh.com`). **`ssh://` and `sftp://` are distinct schemes, not aliases**: ssh:// requires a POSIX shell remotely (batched shell probes, tar pipeline, remote umask, accel hook); sftp:// speaks pure SFTP protocol and works against shell-less chroot accounts. Shared core (URL/env/floor/skeleton/quoting), two transfer engines.

## Phase 0 — prerequisite: fix CLI ↔ external-store wiring (pre-existing bug)

**Divergence from the researched facts:** the emit-command contract expects `--staging-dir`/`--cache-dir` to be **sharded store-root layouts** (mock reads `${staging_dir}/.manifests/<sharded>`, writes `${cache_dir}/.objects/<sharded>`), but the CLI passes **trees**: `run_push` passes the source tree (cli.rs:610) or a materialized scratch tree (cli.rs:565) as `Store::push`'s `source`, and `fetch_inner` does `store.fetch_files(scratch)` then `cache.push(scratch-as-tree)` (cli.rs:692-698). `FileStore` interprets both as trees (file_store.rs:416-417, 337-339), so native stores work — but **`snapdir push/fetch --store <external>://` is broken end-to-end today**. The shim tests pass only because they hand-build sharded staging dirs; no CLI test drives `mock://`.

Fix in cli.rs, adapter-aware (matches the bash oracle, where staging/cache dir IS the local cache):
- **push, `Adapter::External`**: stage into the local cache first (`cache.push(&manifest, &root)` — existing stage semantics, idempotent), then `store.push(&manifest, cache_root)`. The cache root is a valid (superset) staging dir.
- **fetch, `Adapter::External`**: `store.fetch_files(&manifest, cache_root)` directly (objects land sharded in the cache), then commit the manifest to the cache **last** via `FileStore::put_manifest` — preserves manifest-last locally and skips the scratch double-copy.

**Gate `external-cli-roundtrip`**: new test in crates/snapdir-cli/tests/ driving the real `snapdir` binary (`env!("CARGO_BIN_EXE_snapdir")`, pattern from store_roundtrip.rs) against `mock://` with tests/snapdir-mock-store on PATH: push → fetch → checkout round-trip + push idempotency. This must land before any ssh work; the ssh stores inherit the same wiring.

## Phase 1 — crate skeleton + pure logic (T1 green before any transport)

```
crates/snapdir-ssh-store/
  Cargo.toml                  # lib + [[bin]] snapdir-ssh-store + [[bin]] snapdir-sftp-store
  src/lib.rs                  # pub run(Engine::{Ssh,Sftp}, args, stdin) -> ExitCode
  src/args.rs                 # std-only contract arg parsing (mock grammar parity)
  src/url.rs                  # SshUrl { user, host, port, base }
  src/config.rs               # env family + security-floor flag builder
  src/version.rs              # `ssh -V` parse + >=8.5 fail-closed check
  src/script.rs               # shared skeleton, sh_quote(), sftp_quote(), heredoc helpers
  src/ssh_engine.rs           # ssh:// emitters (Phase 2) + accel branch (Phase 5)
  src/sftp_engine.rs          # sftp:// emitters (Phase 3)
  src/bin/snapdir-ssh-store.rs   # 3-line shim into lib
  src/bin/snapdir-sftp-store.rs
  tests/emitted_contract.rs   # T1 text assertions
  tests/fake_ssh_roundtrip.rs # T1 hermetic execution (Phases 2-3)
  tests/loopback_sshd.rs      # T2 (Phase 6)
  tests/fixtures/fake-ssh, fake-sftp
  tests/common/sshd.rs
```

Workspace Cargo.toml: members += the crate; workspace.dependencies += `snapdir-ssh-store = { path, version }`. Dev-deps: `snapdir-stores` (for the `ExternalStore::with_binary` parity harness — lives here because `CARGO_BIN_EXE_*` only resolves in the defining crate; helpers lifted from crates/snapdir-stores/tests/shim_external_store.rs:63-96).

**URL grammar** `<scheme>://[user@]host[:port]/abs/base/path`: reject embedded passwords (`user:pw@`), empty host, bare-`/` base; user/host charsets `[A-Za-z0-9._-]` (no leading `-`) double as shell-injection defense; bracketed IPv6 required; port → `-o Port=<n>` (uniform across ssh/sftp); path literal bytes, control chars rejected, shell-quoted at emission.

**Security floor** — first on argv of EVERY ssh/sftp invocation (OpenSSH is first-obtained-value-wins, so floor-first beats both `~/.ssh/config` and user extras):
```
-o BatchMode=yes -o StrictHostKeyChecking=yes
-o PasswordAuthentication=no -o KbdInteractiveAuthentication=no
-o ClearAllForwardings=yes
-o KexAlgorithms=sntrup761x25519-sha512@openssh.com,curve25519-sha256,curve25519-sha256@libssh.org
-o Ciphers=chacha20-poly1305@openssh.com,aes256-gcm@openssh.com,aes128-gcm@openssh.com
-o HostKeyAlgorithms=ssh-ed25519-cert-v01@openssh.com,ssh-ed25519,rsa-sha2-512-cert-v01@openssh.com,rsa-sha2-256-cert-v01@openssh.com,rsa-sha2-512,rsa-sha2-256,ecdsa-sha2-nistp256-cert-v01@openssh.com,…,ecdsa-sha2-nistp521
-o ConnectTimeout=<cfg|10>
```
MACs omitted (all floor ciphers are AEAD — pinning MACs adds breakage for zero gain). ECDSA kept at tail (not broken; floor is un-weakenable, so exclusion would strand ecdsa-only hosts). `ssh-rsa`/SHA-1/dss excluded. Hard kex list, no `ssh -Q` probing (deterministic emission; all names exist in 8.5+). User config/agent/ProxyJump/known_hosts keep working for everything unset.

**Env family** (`SNAPDIR_SSH_STORE_*` / `SNAPDIR_SFTP_STORE_*`, matching `SNAPDIR_S3_STORE_ENDPOINT_URL` convention): `IDENTITY_FILE` (adds `IdentitiesOnly=yes`), `PORT` (URL wins), `KNOWN_HOSTS` (UserKnownHostsFile), `CONNECT_TIMEOUT` (10), `JOBS` (fallback `SNAPDIR_JOBS`→`SNAPDIR_MAX_JOBS`→4), `CONTROL_PERSIST` (60), `UMASK` (077; sftp engine uses `chmod 600` instead), `EXTRA_OPTS` (appended LAST → structurally cannot weaken the floor). `SNAPDIR_SSH_NO_ACCEL` / `SNAPDIR_SSH_FORCE_ACCEL` / `SNAPDIR_SSH_PULL_SENDALL` read at **script runtime**.

**Shared script skeleton** (composes with the wrapper's `set -eEuo pipefail; trap 'kill 0' INT; …; wait`; never traps INT):
```bash
snapdir_tmp="$(mktemp -d ...)"; chmod 700 "$snapdir_tmp"
_snapdir_cleanup() { ssh <floor> -o ControlPath="$snapdir_tmp/cm" -O exit <host> 2>/dev/null || true; rm -rf "$snapdir_tmp"; }
trap _snapdir_cleanup EXIT TERM HUP
_snapdir_ssh()  { command ssh  -o ControlMaster=auto -o ControlPath="$snapdir_tmp/cm" -o ControlPersist=<60> <floor> <cfg> <extra> -- <host> "$@"; }
_snapdir_sftp() { command sftp <same mux/floor/cfg/extra> -b "$1" -- <host>; }
```
`ControlMaster=auto` (first call creates the master implicitly; sftp multiplexes over it even against internal-sftp servers); `ControlPersist=60` is the leak backstop; 0700 /tmp dir keeps ControlPath short (sun_path limit) and private. One TCP+auth handshake per operation.

**Tests (T1, tests/emitted_contract.rs)** — gates `emitted-contract`, `security-floor` (textual), `version-floor`: floor flags present in order on every invocation; `EXTRA_OPTS="StrictHostKeyChecking=no"` appears strictly AFTER the floor flag (first-wins proof); exact not-found wording `ID '<id>' not found on --store '<store>'.`; exact `ERROR: missing object <checksum>`; manifest-script stdout purity; push ordering probe→objects→manifest-commit; URL/`ssh -V` parser tables.

## Phase 2 — ssh:// dumb engine

The binary reads the staged manifest (push: from `--staging-dir/.manifests/<sharded>`; fetch: from its own stdin) at emit time via `snapdir_core::manifest::Manifest::parse`, validates every checksum (`^[0-9a-f]{64}$` implied by parse + explicit regex gate), and bakes sharded relpath lists into the script as heredocs.

- **Push**: (1) runtime manifest probe — `_snapdir_ssh "test -e <base>/<manifest_rel>"`, exit 0 → `echo "Manifest already exists on store."; exit 0`; exit 1 → continue; other → fail (connectivity never masquerades as absence). (2) ONE batched existence probe: heredoc candidate list piped to `ssh 'umask <U> && mkdir -p <base> && cd <base> && while IFS= read -r p; do [ -e "$p" ] || printf "%s\n" "$p"; done'` → missing set. (3) one `tar -C staging -cf - -T missing | ssh 'cd base && t=$(mktemp -d .snapdir-incoming.XXXXXX) && tar -C "$t" -xf - && …mv -f into sharded paths… && rm -rf "$t"'` — remote temp + `mv` rename = atomic per object. (4) manifest LAST, separate call: `mkdir -p && cat > tmp && mv -f tmp <manifest_path>` (atomic; manifest-last preserved).
- **Fetch**: emit-time local cache check drops already-present objects; call 1 = batched remote existence check emitting exact `ERROR: missing object <sum>` lines (fail before transfer); call 2 = remote `tar -cf -` of needed paths streamed back to a local file; **exact-match allowlist** (`tar -tf` output must `grep -xF` against the client-generated path list — closes the entire tar entry-name attack surface from a malicious remote: `..`, absolute paths, symlinks can't match `^\.objects/[0-9a-f/]+$`); extract into temp dir under cache dir; `mv -f` per object; ensure-no-errors epilogue re-asserting every object landed.

**Tests (T1, tests/fake_ssh_roundtrip.rs)** with `fixtures/fake-ssh` (executes the remote command via `sh -c` against `$FAKE_REMOTE_ROOT`; `FAKE_SSH_FAIL_*` knobs for fault injection), driven through `ExternalStore::with_binary(env!("CARGO_BIN_EXE_snapdir-ssh-store"))`:
- parity round-trip (mirror of the 5 mock-store tests), **gate `not-found-mapping`** (→ `StoreError::ManifestNotFound`)
- **gate `atomicity`**: injected mid-transfer failure → remote has some objects, NO manifest, no non-temp partials; retry completes (mirror push.rs `concurrent_upload_all_or_nothing_on_failure` / sync.rs `FailingPutStore`)
- **gate `idempotency`**: second push no-ops on present manifest
- **gate `fetch-missing-object`**: deleted remote object → `StoreError::Backend` + exact ERROR wording
- **gate `tar-allowlist`**: fake-ssh emits a tar with an `../evil` entry → fetch fails before extraction

## Phase 3 — sftp:// engine (pure SFTP, no remote shell)

All operations are `sftp -b` batchfiles written to `$snapdir_tmp` (heredocs baked at emit time), run over the shared master. `-` prefix tolerates per-command failure; unprefixed failure aborts the batch (exit 1).

- **Probe**: single-command `ls <manifest_path>` batch; on failure a `pwd` liveness batch over the live master disambiguates missing vs unreachable (network errors never map to not-found/re-push).
- **Push**: per-object existence probe via one batch of tolerated `-ls <objpath>` lines (stdout parsed; parse failure degrades safely to upload-all); missing objects chunked into `JOBS` parallel batchfiles, each: `-mkdir` ancestor chain → `put` to `<obj>.tmp.<pid>` → `-rm <obj>` → `rename tmp obj` (posix-rename@openssh.com when offered) → `chmod 600 obj`; backgrounded with explicit per-pid `wait`/status collection; manifest LAST in its own final batch with the same put-tmp/rename/chmod discipline.
- **Fetch**: chunked `-get` of needed objects into a temp dir under cache-dir (flat checksum names), then local move-into-shards + ensure-no-errors epilogue with exact wording.

**Tests**: sftp half of fake_ssh_roundtrip.rs via `fixtures/fake-sftp` (interprets exactly the batch verbs emitted, with `-` tolerance, against the same fake root); same gate set as Phase 2 (atomicity/idempotency/not-found/missing-object).

## Phase 4 — wire protocol + CLI plumbing (acceleration foundation)

**`crates/snapdir-stores/src/pack.rs`** (new; zero new deps — tar crate rejected: both stream ends are the Rust binary, so a custom format avoids tar semantics entirely):
```
stream   := "SNAPPACK 1\n" record* "end\n"
record   := "obj " hex64 " " len "\n" payload(len)
          | "manifest " hex64 " " len "\n" payload(len)   ; at most one, must be last record
hex64    := ^[0-9a-f]{64}$ (validated on read AND write); len := decimal u64
```
Header lines ≤128 bytes (memory bound); manifest payload capped 64 MiB (buffered); obj payloads **streamed with incremental BLAKE3** — on `file://` sinks bytes stream to a `temp_sibling` and rename only on hash match (reuses file_store.rs discipline); mismatch ⇒ temp deleted, stream aborted. EOF before `end` = hard error, manifest NEVER committed (manifest-last preserved server-side even against truncation). Duplicate obj records idempotent (still hash-verified from the stream). `pub const WIRE_VERSION: u32 = 1` + `WIRE_CAPS` live here.

**`StreamStore::objects_needed`** defaulted method in stream.rs (order-preserving has_object loop).

**CLI (crates/snapdir-cli/src/cli.rs)**, all hidden plumbing:
- `snapdir version --capabilities` (hidden flag on existing `Version`; older remotes error on the flag → probe treats as "no accel" — clean degradation): prints `snapdir <semver> wire=1 caps=objects-needed,send-pack,receive-pack`. Grammar: space-separated `key=value`, unknown fields ignored, negotiation on exact `wire` integer match only.
- `snapdir objects-needed --store <url>`: stdin checksums; ANY malformed/empty line → hard error before any output (fail-closed); dedupe preserving first-occurrence order; print exact absent subset in order. Routes through `stream_store_for_adapter` → works for file/s3/gcs/b2.
- `snapdir send-pack --store <url> --ids <FILE|-> [--manifest-id <id>]`: emits SNAPPACK; missing requested id → abort before `end` (receiver fails too — no silent partial). Two-pass streaming on file stores (O(1) memory).
- `snapdir receive-pack --store <url> [--require-manifest <id>]`: consumes SNAPPACK as above; path ALWAYS derived from the validated claimed checksum via `object_path()` — there is no entry-name concept, the traversal class is structurally absent.

**Tests (T1)** — gates: **`pack-roundtrip`** (encode N objects+manifest from store A → decode into empty B → byte-equal subset; 0-byte object; header-cap edges); **`objects-needed-correctness`** (pre-seed subset, feed full list with dupes, stdout == exact complement in first-occurrence order; malformed line → exit≠0 + empty stdout); **`receive-pack-security`** (record claiming X with bytes hashing Y → exit≠0, nothing filed at X, no manifest; non-hex/`../`-bearing header → rejected; truncated stream → objects filed, manifest ABSENT; duplicates → idempotent success); **`mid-stream-failure`** (writer closed after N records, no `end` → partial objects, no manifest; full re-push completes).

## Phase 5 — ssh:// acceleration (runtime branch in emitted scripts)

Emit-time probing is impossible (no connection exists when the script is emitted), so the emitted ssh:// script embeds BOTH paths and branches at runtime. The local end of accel pipes uses the **local `snapdir` binary** (it's necessarily installed — it's orchestrating). One combined probe round trip:

```bash
__sd_probe: ssh "if test -f '<base>/<man_rel>'; then echo manifest=1; else echo manifest=0; fi; \
                 command -v snapdir >/dev/null 2>&1 && snapdir version --capabilities || echo 'caps none'"
# dispatch:
manifest=1                            -> "Manifest already exists on store."; exit 0
SNAPDIR_SSH_NO_ACCEL=1                -> dumb path
caps line has wire=1 (baked literal from pack::WIRE_VERSION) + needed caps -> accel
SNAPDIR_SSH_FORCE_ACCEL=1 (no caps)   -> error: names host, required wire/caps, what the probe
                                         returned, remedies (upgrade remote / unset var); exit 1
else                                  -> dumb path
```

- **Accel push** (3 round trips: probe, diff, stream): baked F-checksum heredoc → `ssh 'snapdir objects-needed --store file://<base>'` → missing; `snapdir send-pack --store file://<staging> --ids missing --manifest-id <id> | ssh 'snapdir receive-pack --store file://<base> --require-manifest <id>'`. Manifest rides the pack as last record; remote commits only after the verified `end` trailer → atomic commit, no 4th trip. Empty missing set still streams the manifest-only pack (completes interrupted pushes).
- **Accel fetch** (2 round trips): emit-time local cache diff (or full set under `SNAPDIR_SSH_PULL_SENDALL=1`) → needed heredoc → `ssh 'snapdir send-pack --store file://<base> --ids -' < needed | snapdir receive-pack --store file://<cache-dir>` (no `--require-manifest`; remote stream fully untrusted → every record incrementally verified locally, O(1) memory).
- **Fallback policy**: probe/diff failure (ssh works, snapdir doesn't) → dumb path (nothing written yet; idempotent anyway). **Stream failure → fail the push** (no silent dumb retry: failure is likely environmental and would hit dumb too; user retry resumes incrementally for free). Capability memoization beyond one script run: skipped deliberately — one probe per operation is already O(1) and ControlMaster amortizes the connection; cross-run caching adds invalidation surface for negligible win.
- Exit codes: the pipe runs under the orchestrator's `set -eEuo pipefail`; ssh propagates remote exit; Rust EPIPE → non-zero.

**Tests** — gates: **`accel-oracle`** (T1, the headline correctness gate): push identical source via dumb path and via send-pack|receive-pack into two roots; assert identical snapshot id, identical `.objects` file SET, byte-equal blobs (mirrors sync.rs `sync_snapshot_adaptive_mirrors_same_snapshot`); **`fallback`** (T1 with fake-ssh: remote with no snapdir / fake printing `wire=99` → dumb path completes byte-identically; FORCE_ACCEL → exit≠0 with the designed message); probe-shape assertions (single combined probe text).

## Phase 6 — T2: loopback sshd integration

`tests/common/sshd.rs` spawns a real sshd (no docker): temp dir, ed25519 host+user keys, two configs for the current user — port A normal shell + `Subsystem sftp internal-sftp`; port B + `ForceCommand internal-sftp` (sftp-only; ChrootDirectory needs root so no-shell is the tested property); `ListenAddress 127.0.0.1`, probed high ports, `PasswordAuthentication no`, `UsePAM no`, `StrictModes no`, `sshd -D -e -f <config>`; killed on drop. **Skip policy** (house pattern, cf. s3_store.rs live tests): eprintln-skip when sshd is absent — unless `SNAPDIR_SSH_TEST_REQUIRE=1` (CI sets it so the gate can't rot). `SNAPDIR_SSH_TEST_HOST` optional external-server override. Remote snapdir for accel tests = `CARGO_BIN_EXE_snapdir` exposed on the sshd account's PATH via wrapper/environment.

Gates: full push→get-manifest→fetch round-trips for BOTH schemes (dumb), accel round-trip + **`accel-oracle`** over real sshd, **`fallback`** (remote PATH without snapdir), **`host-key-fail-closed`** (wrong known_hosts entry → fails; STILL fails with `EXTRA_OPTS="StrictHostKeyChecking=no"` — behavioral floor proof), **`restricted-sftp-only`** (sftp:// succeeds against port B; ssh:// fails), idempotent re-push, no leaked ControlMaster sockets, **probe-count** (counting ssh wrapper asserts accel push = exactly 3 invocations).

## Phase 7 — CI, packaging, docs

- **CI**: no new job — T1+T2 ride `cargo test --workspace` in the existing test matrix (local pre-push hook mirrors it automatically, satisfying the all-checks-local rule). Two ci.yaml edits: Linux-only step `apt-get install openssh-server`; `SNAPDIR_SSH_TEST_REQUIRE: "1"` in test job env (macOS ships `/usr/sbin/sshd`).
- **Packaging**: crates.io publish wiring for `snapdir-ssh-store` (release.yml idempotent publish loop), cargo-dist inclusion of both bins, trusted publishing registration noted as a release-time step.
- **Docs**: root README store table (+`ssh://`/`sftp://` rows + "use ssh:// when you have a shell; sftp:// for restricted accounts"), crate README (env family, security floor, OpenSSH ≥8.5 policy, accel behavior), `docs/rust-port/` wire-protocol page (SNAPPACK grammar + capability line), ADR for wire-versioned plumbing + system-OpenSSH decision. CHANGELOG; version 1.5.0.

## Execution model (per operator decision)

Work is driven through the **gatesmith ledger**: register the gates below in `.gatesmith/gates.yaml` as a new phase (phase 24, kebab-case ids, `verification_cmd` = the concrete cargo test invocations, `depends_on` encoding the phase order above — Phase 0 gate first, accel gates depending on pack/CLI gates), then build via `/gatesmith:loop` ticks following the established dev-branch model (ledger commits split from code commits; `.gatesmith` never reaches main).

## Gate → test summary

| Gate | Tier | Test |
|---|---|---|
| external-cli-roundtrip (Phase 0 prereq) | T1 | snapdir-cli e2e vs mock:// via real binary |
| emitted-contract, security-floor (textual), version-floor | T1 | emitted_contract.rs |
| atomicity, idempotency, not-found-mapping, fetch-missing-object | T1+T2 | fake_ssh_roundtrip.rs / loopback_sshd.rs |
| tar-allowlist (malicious remote tar) | T1 | fake-ssh hostile fixture |
| pack-roundtrip, objects-needed-correctness, receive-pack-security, mid-stream-failure | T1 | pack.rs units + assert-style CLI tests |
| accel-oracle (byte-identical dumb vs accel) | T1+T2 | two-root push compare |
| fallback + FORCE_ACCEL error | T1+T2 | no-snapdir / wire=99 remotes |
| host-key-fail-closed, restricted-sftp-only, probe-count | T2 | loopback_sshd.rs |

## Key risks

- sftp `-b` error reporting is coarse → single-command probes + `pwd` liveness batch + local post-checks own the exact ERROR wording; `-ls` parse failure degrades to upload-all (never wrong, only slower).
- GNU vs bsdtar: only the portable intersection used (`-c -x -t -f - -C -T file`); exotic entries fail the exact-match allowlist (fail-closed).
- bash EXIT-trap skipped on some signal paths → TERM/HUP traps + ControlPersist=60 self-reap; worst case an empty 0700 temp dir.
- macOS vs Linux sshd quirks in T2 (UsePAM, key perms, port probing) handled in the fixture; macOS verified first since pre-push runs there.
- OpenSSH 8.0-era clients (RHEL8) excluded by the floor — documented policy.
- `while read | pipe` subshell scoping in emitted scripts → temp files, not pipelines, for sets that must persist.

## Verification (end-to-end)

1. `cargo test --workspace --locked` — all T1 gates + T2 (auto-skip without sshd; `SNAPDIR_SSH_TEST_REQUIRE=1` to force).
2. Manual smoke: `snapdir push --store ssh://user@host/tmp/snap ./tree` against a real host with and without remote snapdir on PATH; `SNAPDIR_SSH_FORCE_ACCEL=1` error check; `sftp://` against an internal-sftp-only account.
3. Full local pre-push gate suite (mirrors ci.yaml) before push; coverage stays ≥75% (emission logic is highly testable Rust).
