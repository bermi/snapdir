//! Gate `loopback-sshd-suite` (phase 24, T2): REAL OpenSSH end-to-end — the
//! actual `ssh`/`sftp` clients (with the full security floor) against a
//! self-spawned loopback `sshd` (no docker, no root; see
//! `tests/common/sshd.rs`), driven through the real external-store contract
//! (`ExternalStore::with_binary` → store binary → emitted script → `eval`).
//!
//! Coverage: dumb `ssh://` + `sftp://` push→get-manifest→fetch round trips;
//! the restricted `ForceCommand internal-sftp` server (sftp:// works, ssh://
//! fails — no shell IS the property); host-key fail-closed INCLUDING the
//! behavioral un-weakenable-floor proof (`EXTRA_OPTS=StrictHostKeyChecking=no`
//! still fails); the accel round trip + byte-identical dumb-vs-accel oracle
//! over real sshd (remote `snapdir` exposed via the server-side `SetEnv
//! PATH`, wrapped in a logging shim so engagement is asserted, never
//! assumed); graceful fallback + the `SNAPDIR_SSH_FORCE_ACCEL` designed
//! error; idempotent re-push; and no leaked `ControlMaster` sockets / temp
//! dirs.
//!
//! **Isolation under parallel scheduling**: every mutable directory is
//! per-test — staging/cache/`TMPDIR` scratch are distinct
//! [`common::TempDir`]s on a FIXED `/tmp` base (never
//! `std::env::temp_dir()`, which would re-read the mutated `TMPDIR`), each
//! sshd kit is per-test, and [`loopback_env`] pins `TMPDIR` to the test's
//! OWN scratch inside the env-lock guard. No test ever reads or writes
//! another test's directories, so the suite is deterministic at any
//! `--test-threads` level; the leak assertion scans only its own scratch
//! and only for script-created `snapdir-ssh-store.*` artifacts.
//!
//! **Skip policy** (house pattern): missing OpenSSH tooling or a missing
//! real `snapdir` binary `eprintln!`-skips — unless `SNAPDIR_SSH_TEST_REQUIRE=1`
//! (CI sets it), which turns those skips into panics. The ONE allowed skip
//! under REQUIRE is an sshd whose `SetEnv` directive doesn't pin `PATH`
//! (pre-8.7 servers): only the two PATH-dependent accel/fallback tests skip,
//! with the decision printed (macOS and CI both ship OpenSSH ≥ 8.7, so in
//! practice everything runs).
//!
//! **probe-count decision**: the plan's "accel push = exactly 3 ssh round
//! trips" gate is asserted hermetically in `tests/accel.rs` (emitted-text:
//! exactly one capability probe, one diff, one stream). Over real sshd the
//! client is the REAL `ssh`, so counting its invocations would mean wrapping
//! the system client — re-proving what the emitted-text tests already pin.
//! This suite instead asserts accel ENGAGEMENT (logging `snapdir` wrapper)
//! and an empty-log no-op on the idempotent re-push.
//!
//! `SNAPDIR_SSH_TEST_HOST` (external server override) is documented in
//! `tests/common/sshd.rs` as a future knob; not implemented this gate.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use snapdir_core::store::{manifest_path, object_path, Store, StoreError};
use snapdir_stores::ExternalStore;

use common::sshd::{Flavor, SshKit};
use common::{
    files_under, install_logging_snapdir, manifest_bytes, relative_file_set, require_snapdir,
    stage_tree, EnvGuard, TempDir,
};

/// The default test files (several distinct objects).
const FILES: &[(&str, &[u8])] = &[
    ("a.txt", b"alpha payload\n"),
    ("b.txt", b"bravo bravo payload\n"),
    ("c.bin", b"charlie third object\n"),
];

fn ssh_store(base: &Path) -> ExternalStore {
    ExternalStore::with_binary(
        &format!("ssh://127.0.0.1{}", base.display()),
        env!("CARGO_BIN_EXE_snapdir-ssh-store"),
    )
}

fn sftp_store(base: &Path) -> ExternalStore {
    ExternalStore::with_binary(
        &format!("sftp://127.0.0.1{}", base.display()),
        env!("CARGO_BIN_EXE_snapdir-sftp-store"),
    )
}

/// Takes the env lock and points BOTH engine families (`SNAPDIR_SSH_STORE_*`
/// and `SNAPDIR_SFTP_STORE_*`) at the kit's identity / `known_hosts` / port,
/// scrubbing every other knob a developer shell might leak (env is
/// process-global and flows into the spawned store binary + eval shell +
/// every ssh client it runs — hence the serialization).
///
/// `tmp` is the test's OWN `TMPDIR` scratch: every emitted script this test
/// runs places its `mktemp -d` work dir (`ControlPath` socket included)
/// there.
/// The pin lives inside the guard (set under the lock, restored on drop) and
/// store operations only happen while the guard is held, so no other test's
/// scripts ever see — or write into — this test's scratch.
fn loopback_env(kit: &SshKit, port: u16, tmp: &Path) -> EnvGuard {
    let mut guard = EnvGuard::new();
    guard.set("TMPDIR", &tmp.display().to_string());
    let identity = kit.user_key.display().to_string();
    let known_hosts = kit.known_hosts.display().to_string();
    for prefix in ["SNAPDIR_SSH_STORE_", "SNAPDIR_SFTP_STORE_"] {
        guard.set(&format!("{prefix}IDENTITY_FILE"), &identity);
        guard.set(&format!("{prefix}KNOWN_HOSTS"), &known_hosts);
        guard.set(&format!("{prefix}PORT"), &port.to_string());
        guard.set(&format!("{prefix}CONNECT_TIMEOUT"), "10");
        guard.remove(&format!("{prefix}EXTRA_OPTS"));
        guard.remove(&format!("{prefix}JOBS"));
        guard.remove(&format!("{prefix}UMASK"));
        guard.remove(&format!("{prefix}CONTROL_PERSIST"));
    }
    guard.remove("SNAPDIR_STORE");
    guard.remove("SNAPDIR_JOBS");
    guard.remove("SNAPDIR_MAX_JOBS");
    guard.remove("SNAPDIR_SSH_NO_ACCEL");
    guard.remove("SNAPDIR_SSH_FORCE_ACCEL");
    guard.remove("SNAPDIR_SSH_PULL_SENDALL");
    guard.remove("SNAPDIR_SSH_LOCAL_SNAPDIR");
    guard
}

/// Asserts a full push → get-manifest → fetch round trip for `store` against
/// a real server, mirroring the hermetic suites' assertions: sharded remote
/// paths, no group/other permission bits, byte-equal objects + manifest, no
/// temp residue (`.snapdir-incoming.` / `.tmp.`) on either side.
fn assert_roundtrip(store: &ExternalStore, staging: &Path, base: &Path, cache: &Path) {
    let (manifest, id, sums) = stage_tree(staging, FILES);

    store.push(&manifest, staging).expect("push");
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        let obj = base.join(object_path(sum));
        assert!(obj.is_file(), "object {sum} should be on the remote");
        assert_eq!(&fs::read(&obj).unwrap(), content);
        let mode = fs::metadata(&obj).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "0600-class discipline on {sum}: {mode:o}");
    }
    let man = base.join(manifest_path(&id));
    assert!(man.is_file(), "manifest should be on the remote");
    assert_eq!(fs::read_to_string(&man).unwrap(), manifest_bytes(&manifest));
    let man_mode = fs::metadata(&man).unwrap().permissions().mode() & 0o777;
    assert_eq!(man_mode & 0o077, 0, "0600-class discipline on the manifest");
    assert_no_temp_residue(base, "remote");

    let fetched = store.get_manifest(&id).expect("get_manifest");
    assert_eq!(fetched.to_string(), manifest.to_string());

    store.fetch_files(&manifest, cache).expect("fetch_files");
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        let cached = cache.join(object_path(sum));
        assert!(cached.is_file(), "object {sum} should be in the cache");
        assert_eq!(&fs::read(&cached).unwrap(), content);
    }
    assert_no_temp_residue(cache, "cache");
}

fn assert_no_temp_residue(root: &Path, side: &str) {
    let residue: Vec<PathBuf> = files_under(root)
        .into_iter()
        .filter(|p| {
            let s = p.to_string_lossy();
            s.contains(".snapdir-incoming.") || s.contains(".tmp.") || s.contains(".snapdir-")
        })
        .collect();
    assert!(
        residue.is_empty(),
        "no temp residue on the {side}: {residue:?}"
    );
}

/// Asserts the test's OWN `TMPDIR` scratch holds no script-created
/// artifacts: the emitted scripts `mktemp -d` their 0700 work dirs
/// (`ControlPath` `cm` socket included) as `snapdir-ssh-store.*` under
/// `$TMPDIR`, and every EXIT trap must have run `ssh -O exit` + `rm -rf`.
/// Scoped to that name pattern and to this test's private scratch only, so
/// it is deterministic regardless of what other tests are doing.
fn assert_no_script_leaks(tmp: &Path) {
    let leaked: Vec<String> = fs::read_dir(tmp)
        .expect("read TMPDIR scratch")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("snapdir-ssh-store."))
        .collect();
    assert!(
        leaked.is_empty(),
        "leaked ControlMaster sockets / script temp dirs under the test's TMPDIR: {leaked:?}"
    );
}

fn log_lines(log: &Path) -> String {
    fs::read_to_string(log).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 1. ssh:// dumb round trip over real sshd (port A)
// ---------------------------------------------------------------------------

#[test]
fn ssh_dumb_push_get_manifest_fetch_roundtrip_over_sshd() {
    let Some(mut kit) = SshKit::new("ssh_dumb_push_get_manifest_fetch_roundtrip_over_sshd") else {
        return;
    };
    let server = kit.spawn(&Flavor::Shell { set_env_path: None });
    let staging = TempDir::new("sd-stage");
    let cache = TempDir::new("sd-cache");
    let tmp = TempDir::new("sd-tmp");
    let base = kit.dir().join("remote-ssh-dumb");

    let mut env = loopback_env(&kit, server.port, tmp.path());
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");
    assert_roundtrip(&ssh_store(&base), staging.path(), &base, cache.path());
}

// ---------------------------------------------------------------------------
// 2. sftp:// round trip over real sshd (port A)
// ---------------------------------------------------------------------------

#[test]
fn sftp_push_get_manifest_fetch_roundtrip_over_sshd() {
    let Some(mut kit) = SshKit::new("sftp_push_get_manifest_fetch_roundtrip_over_sshd") else {
        return;
    };
    let server = kit.spawn(&Flavor::Shell { set_env_path: None });
    let staging = TempDir::new("sf-stage");
    let cache = TempDir::new("sf-cache");
    let tmp = TempDir::new("sf-tmp");
    let base = kit.dir().join("remote-sftp");

    let _env = loopback_env(&kit, server.port, tmp.path());
    assert_roundtrip(&sftp_store(&base), staging.path(), &base, cache.path());
}

// ---------------------------------------------------------------------------
// 3. restricted-sftp-only (port B): sftp:// succeeds, ssh:// fails
// ---------------------------------------------------------------------------

#[test]
fn restricted_sftp_only_server_allows_sftp_and_rejects_ssh() {
    let Some(mut kit) = SshKit::new("restricted_sftp_only_server_allows_sftp_and_rejects_ssh")
    else {
        return;
    };
    let server = kit.spawn(&Flavor::SftpOnly);
    let staging = TempDir::new("rs-stage");
    let cache = TempDir::new("rs-cache");
    let tmp = TempDir::new("rs-tmp");
    let sftp_base = kit.dir().join("remote-restricted-sftp");
    let ssh_base = kit.dir().join("remote-restricted-ssh");

    let mut env = loopback_env(&kit, server.port, tmp.path());

    // The sftp:// engine speaks pure SFTP — a ForceCommand internal-sftp
    // account (the restricted-hosting story) round-trips fully.
    assert_roundtrip(
        &sftp_store(&sftp_base),
        staging.path(),
        &sftp_base,
        cache.path(),
    );

    // The ssh:// engine needs a remote POSIX shell — the same server must
    // reject it (pinned to the dumb path so the dispatch is deterministic),
    // and nothing may land.
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");
    let (manifest, id, _sums) = stage_tree(staging.path(), FILES);
    ssh_store(&ssh_base)
        .push(&manifest, staging.path())
        .expect_err("ssh:// push must fail against a ForceCommand internal-sftp server");
    assert!(
        !ssh_base.join(manifest_path(&id)).exists(),
        "no manifest may land via ssh:// on the sftp-only server"
    );
}

// ---------------------------------------------------------------------------
// 4. host-key fail-closed + the behavioral un-weakenable-floor proof
// ---------------------------------------------------------------------------

#[test]
fn host_key_mismatch_fails_closed_and_extra_opts_cannot_weaken_the_floor() {
    let Some(mut kit) =
        SshKit::new("host_key_mismatch_fails_closed_and_extra_opts_cannot_weaken_the_floor")
    else {
        return;
    };
    let server = kit.spawn(&Flavor::Shell { set_env_path: None });
    let staging = TempDir::new("hk-stage");
    let tmp = TempDir::new("hk-tmp");
    let base = kit.dir().join("remote-hostkey");
    let wrong = kit.wrong_known_hosts(server.port);

    let mut env = loopback_env(&kit, server.port, tmp.path());
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");
    env.set(
        "SNAPDIR_SSH_STORE_KNOWN_HOSTS",
        &wrong.display().to_string(),
    );

    let (manifest, id, _sums) = stage_tree(staging.path(), FILES);

    // A known_hosts carrying the WRONG key for the server: fail closed.
    let err = ssh_store(&base)
        .push(&manifest, staging.path())
        .expect_err("push must fail on a host-key mismatch");
    assert!(
        matches!(err, StoreError::Backend { .. }),
        "host-key mismatch is a Backend (connectivity) error, got {err:?}"
    );

    // The BEHAVIORAL un-weakenable-floor proof: user extras trying to turn
    // host-key checking OFF are structurally inert (the floor's
    // StrictHostKeyChecking=yes was already first-obtained), so it STILL
    // fails — against a live server, with the real ssh client.
    env.set("SNAPDIR_SSH_STORE_EXTRA_OPTS", "StrictHostKeyChecking=no");
    ssh_store(&base)
        .push(&manifest, staging.path())
        .expect_err("EXTRA_OPTS=StrictHostKeyChecking=no must NOT weaken the floor");

    assert!(
        !base.join(manifest_path(&id)).exists(),
        "nothing may land on a server whose host key never verified"
    );
}

// ---------------------------------------------------------------------------
// 5. accel round trip + byte-identical dumb-vs-accel oracle over real sshd
// ---------------------------------------------------------------------------

#[test]
fn accel_oracle_roundtrip_and_idempotent_repush_over_sshd() {
    let test = "accel_oracle_roundtrip_and_idempotent_repush_over_sshd";
    let Some(real) = require_snapdir(test) else {
        return;
    };
    let Some(mut kit) = SshKit::new(test) else {
        return;
    };

    // Expose the real snapdir to the REMOTE side (sshd sessions inherit
    // sshd's env, not the test env) through SetEnv PATH, behind a logging
    // wrapper so accel engagement is asserted, never assumed.
    let bin = kit.dir().join("bin");
    fs::create_dir_all(&bin).expect("create remote bin dir");
    let log = kit.dir().join("invocations.log");
    install_logging_snapdir(&bin, &real, &log);
    let server = kit.spawn(&Flavor::Shell {
        set_env_path: Some(format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display())),
    });

    // The DOCUMENTED environmental skip: sshd honors SetEnv PATH only from
    // OpenSSH 8.7 on (macOS + CI both ship newer). Allowed even under
    // SNAPDIR_SSH_TEST_REQUIRE=1.
    let probe = kit.ssh_exec(server.port, "command -v snapdir");
    if !probe.status.success() {
        eprintln!(
            "SKIP {test}: this sshd does not honor `SetEnv PATH` \
             (need OpenSSH >= 8.7 server): {}",
            String::from_utf8_lossy(&probe.stderr).trim()
        );
        return;
    }

    let staging = TempDir::new("ao-stage");
    let cache = TempDir::new("ao-cache");
    let tmp = TempDir::new("ao-tmp");
    let base_dumb = kit.dir().join("remote-oracle-dumb");
    let base_accel = kit.dir().join("remote-oracle-accel");
    let (manifest, id, sums) = stage_tree(staging.path(), FILES);

    let mut env = loopback_env(&kit, server.port, tmp.path());
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());

    // Reference: forced-dumb push into root A.
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");
    ssh_store(&base_dumb)
        .push(&manifest, staging.path())
        .expect("forced-dumb push");

    // Accel push into root B; prove engagement from the remote-side log.
    env.remove("SNAPDIR_SSH_NO_ACCEL");
    let accel = ssh_store(&base_accel);
    accel.push(&manifest, staging.path()).expect("accel push");
    let pushed = log_lines(&log);
    assert!(
        pushed.contains("objects-needed"),
        "accel diff ran: {pushed}"
    );
    assert!(
        pushed.contains("receive-pack"),
        "accel stream ran: {pushed}"
    );

    // THE ORACLE: identical file SET, byte-equal contents, same committed
    // snapshot id — dumb vs accel over a real server.
    let set_dumb = relative_file_set(&base_dumb);
    assert_eq!(
        set_dumb,
        relative_file_set(&base_accel),
        "the .objects/** + manifest file sets must be identical"
    );
    assert!(
        set_dumb.contains(&manifest_path(&id)),
        "snapshot id committed on both"
    );
    for rel in &set_dumb {
        assert_eq!(
            fs::read(base_dumb.join(rel)).unwrap(),
            fs::read(base_accel.join(rel)).unwrap(),
            "byte-equal at {rel}"
        );
    }
    assert_eq!(
        fs::read_to_string(base_accel.join(manifest_path(&id))).unwrap(),
        manifest_bytes(&manifest),
        "manifest bytes are the staged manifest's bytes"
    );

    // Idempotent re-push: the combined probe sees manifest=1 and exits
    // before any transfer — no plumbing invocation reaches the remote.
    fs::write(&log, b"").unwrap();
    accel
        .push(&manifest, staging.path())
        .expect("idempotent re-push");
    let repushed = log_lines(&log);
    assert!(
        !repushed.contains("receive-pack") && !repushed.contains("objects-needed"),
        "a re-push of a present manifest must not stream anything: {repushed}"
    );
    assert_eq!(
        set_dumb,
        relative_file_set(&base_accel),
        "re-push must not change the remote"
    );

    // Accel fetch into a cold cache: the remote send-pack streamed, every
    // object landed byte-equal at its sharded cache path (the LOCAL
    // receive-pack verified each record).
    accel
        .fetch_files(&manifest, cache.path())
        .expect("accel fetch");
    assert!(log_lines(&log).contains("send-pack"), "fetch used accel");
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        assert_eq!(
            &fs::read(cache.path().join(object_path(sum)))
                .unwrap_or_else(|_| panic!("object {sum} in cache")),
            content
        );
    }

    // get-manifest round-trips byte-identically over the real server too.
    let fetched = accel.get_manifest(&id).expect("get_manifest");
    assert_eq!(fetched.to_string(), manifest.to_string());
}

// ---------------------------------------------------------------------------
// 5b. wire2-compat-matrix (phase 27): dumb vs accel-ZSTD byte-identical oracle
//     over real sshd. Both peers are the REAL binary (caps include
//     snappack-zstd), so the accel path negotiates the 1Z transport; the
//     resulting store must be byte-identical to a forced-dumb (v1) push.
// ---------------------------------------------------------------------------

#[test]
fn accel_zstd_oracle_byte_identical_to_dumb_over_sshd() {
    let test = "accel_zstd_oracle_byte_identical_to_dumb_over_sshd";
    let Some(real) = require_snapdir(test) else {
        return;
    };
    let Some(mut kit) = SshKit::new(test) else {
        return;
    };

    // Expose the real snapdir to the REMOTE side behind a logging wrapper.
    let bin = kit.dir().join("bin");
    fs::create_dir_all(&bin).expect("create remote bin dir");
    let remote_log = kit.dir().join("remote-invocations.log");
    install_logging_snapdir(&bin, &real, &remote_log);
    let server = kit.spawn(&Flavor::Shell {
        set_env_path: Some(format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display())),
    });

    // DOCUMENTED environmental skip: sshd honors SetEnv PATH only from OpenSSH
    // 8.7 on (macOS + CI ship newer). Allowed even under REQUIRE.
    let probe = kit.ssh_exec(server.port, "command -v snapdir");
    if !probe.status.success() {
        eprintln!(
            "SKIP {test}: this sshd does not honor `SetEnv PATH` \
             (need OpenSSH >= 8.7 server): {}",
            String::from_utf8_lossy(&probe.stderr).trim()
        );
        return;
    }

    // A LOCAL logging shim so the on-wire `--pack-format zstd` flag is asserted
    // from the LOCAL send-pack invocation, never assumed. It execs the REAL
    // binary, whose caps include snappack-zstd → the negotiation picks 1Z.
    let local_bin = kit.dir().join("local-bin");
    fs::create_dir_all(&local_bin).expect("create local bin dir");
    let local_log = local_bin.join("local.log");
    let local_snapdir = local_bin.join("snapdir");
    install_logging_snapdir(&local_bin, &real, &local_log);

    // A COMPRESSIBLE fixture so the zstd transport is genuinely engaged.
    let staging = TempDir::new("az-stage");
    let cache = TempDir::new("az-cache");
    let tmp = TempDir::new("az-tmp");
    let base_dumb = kit.dir().join("remote-zstd-dumb");
    let base_accel = kit.dir().join("remote-zstd-accel");
    let repeat = vec![b'A'; 64 * 1024];
    let compressible: &[(&str, &[u8])] = &[
        ("repeat.txt", repeat.as_slice()),
        ("small-a.txt", b"distinct small payload a\n"),
        ("small-b.txt", b"distinct small payload b\n"),
    ];
    let (manifest, id, sums) = stage_tree(staging.path(), compressible);

    let mut env = loopback_env(&kit, server.port, tmp.path());
    env.set(
        "SNAPDIR_SSH_LOCAL_SNAPDIR",
        &local_snapdir.display().to_string(),
    );

    // Reference: forced-dumb (v1) push into root A.
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");
    ssh_store(&base_dumb)
        .push(&manifest, staging.path())
        .expect("forced-dumb push");
    env.remove("SNAPDIR_SSH_NO_ACCEL");

    // Accel-ZSTD push into root B; prove the 1Z transport engaged.
    let accel = ssh_store(&base_accel);
    accel
        .push(&manifest, staging.path())
        .expect("accel-zstd push");
    assert!(
        log_lines(&local_log).contains("--pack-format zstd"),
        "the LOCAL send-pack must opt into zstd over real sshd: {}",
        log_lines(&local_log)
    );
    assert!(
        log_lines(&remote_log).contains("receive-pack"),
        "the remote receive-pack ran (magic-sniffed the 1Z stream): {}",
        log_lines(&remote_log)
    );

    // THE ORACLE: byte-identical store (set + contents + committed id) between
    // the dumb v1 push and the accel 1Z push over a real server.
    let set_dumb = relative_file_set(&base_dumb);
    assert_eq!(
        set_dumb,
        relative_file_set(&base_accel),
        "the .objects/** + manifest file sets must be identical (v1 vs 1Z)"
    );
    assert!(
        set_dumb.contains(&manifest_path(&id)),
        "snapshot id committed on both"
    );
    for rel in &set_dumb {
        assert_eq!(
            fs::read(base_dumb.join(rel)).unwrap(),
            fs::read(base_accel.join(rel)).unwrap(),
            "byte-equal at {rel} across v1 vs 1Z transports"
        );
    }
    assert_eq!(
        fs::read_to_string(base_accel.join(manifest_path(&id))).unwrap(),
        manifest_bytes(&manifest),
        "manifest bytes are the staged manifest's bytes"
    );

    // Accel-ZSTD fetch into a cold cache: every object lands byte-correctly.
    fs::write(&remote_log, b"").unwrap();
    accel
        .fetch_files(&manifest, cache.path())
        .expect("accel-zstd fetch");
    assert!(
        log_lines(&remote_log).contains("--pack-format zstd"),
        "the remote send-pack must opt into zstd on fetch: {}",
        log_lines(&remote_log)
    );
    for (sum, (_, content)) in sums.iter().zip(compressible) {
        assert_eq!(
            &fs::read(cache.path().join(object_path(sum)))
                .unwrap_or_else(|_| panic!("object {sum} in cache")),
            content
        );
    }
}

// ---------------------------------------------------------------------------
// 6. fallback over real sshd: no remote snapdir → dumb; FORCE_ACCEL → error
// ---------------------------------------------------------------------------

#[test]
fn fallback_without_remote_snapdir_and_force_accel_designed_error() {
    let test = "fallback_without_remote_snapdir_and_force_accel_designed_error";
    let Some(real) = require_snapdir(test) else {
        return;
    };
    let Some(mut kit) = SshKit::new(test) else {
        return;
    };

    // A shell server whose session PATH deliberately carries NO snapdir.
    let server = kit.spawn(&Flavor::Shell {
        set_env_path: Some("/usr/bin:/bin".to_owned()),
    });
    let probe = kit.ssh_exec(server.port, "command -v snapdir");
    if probe.status.success() {
        // Either SetEnv is unsupported (pre-8.7) and the default session
        // PATH carries a system-installed snapdir, or one lives in
        // /usr/bin:/bin — the "no remote snapdir" premise doesn't hold here.
        eprintln!(
            "SKIP {test}: a `snapdir` is visible on the remote session PATH ({})",
            String::from_utf8_lossy(&probe.stdout).trim()
        );
        return;
    }

    let staging = TempDir::new("fb-stage");
    let tmp = TempDir::new("fb-tmp");
    let base = kit.dir().join("remote-fallback");
    let base_forced = kit.dir().join("remote-forced");
    let (manifest, id, sums) = stage_tree(staging.path(), FILES);

    let mut env = loopback_env(&kit, server.port, tmp.path());
    // A local snapdir IS available, so the dispatch genuinely reaches the
    // capability check before degrading.
    env.set("SNAPDIR_SSH_LOCAL_SNAPDIR", &real.display().to_string());

    // Graceful fallback: the dumb path completes the push end-to-end.
    ssh_store(&base)
        .push(&manifest, staging.path())
        .expect("push must fall back to the dumb path and succeed");
    for (sum, (_, content)) in sums.iter().zip(FILES) {
        assert_eq!(&fs::read(base.join(object_path(sum))).unwrap(), content);
    }
    assert_eq!(
        fs::read_to_string(base.join(manifest_path(&id))).unwrap(),
        manifest_bytes(&manifest)
    );

    // FORCE_ACCEL without remote caps: the designed error, before any
    // transfer.
    env.set("SNAPDIR_SSH_FORCE_ACCEL", "1");
    let err = ssh_store(&base_forced)
        .push(&manifest, staging.path())
        .expect_err("FORCE_ACCEL without remote caps must fail");
    match &err {
        StoreError::Backend { message, .. } => {
            assert!(message.contains("127.0.0.1"), "names the host: {message}");
            assert!(message.contains("wire=1"), "names the wire: {message}");
            assert!(
                message.contains("unset SNAPDIR_SSH_FORCE_ACCEL"),
                "names the remedies: {message}"
            );
            assert!(
                message.contains("install or upgrade snapdir"),
                "names the remedies: {message}"
            );
        }
        other => panic!("expected Backend error, got {other:?}"),
    }
    assert!(
        !base_forced.join(manifest_path(&id)).exists(),
        "the FORCE_ACCEL error must abort before any transfer"
    );
}

// ---------------------------------------------------------------------------
// 7. idempotent re-push + no leaked ControlMaster sockets / temp dirs
// ---------------------------------------------------------------------------

#[test]
fn repush_is_idempotent_and_no_control_sockets_or_temp_dirs_leak() {
    let Some(mut kit) =
        SshKit::new("repush_is_idempotent_and_no_control_sockets_or_temp_dirs_leak")
    else {
        return;
    };
    let server = kit.spawn(&Flavor::Shell { set_env_path: None });
    let staging = TempDir::new("lk-stage");
    let cache = TempDir::new("lk-cache");
    // This test's OWN TMPDIR scratch (pinned by loopback_env, like every
    // test): all 4 emitted scripts below mktemp their work dirs here.
    let tmp = TempDir::new("lk-tmp");
    let base = kit.dir().join("remote-leak");
    let (manifest, id, _sums) = stage_tree(staging.path(), FILES);

    let mut env = loopback_env(&kit, server.port, tmp.path());
    env.set("SNAPDIR_SSH_NO_ACCEL", "1");

    let store = ssh_store(&base);
    store.push(&manifest, staging.path()).expect("push");
    let after_first = relative_file_set(&base);
    assert!(after_first.contains(&manifest_path(&id)), "manifest landed");

    // Idempotent re-push: present manifest → no-op success, remote unchanged.
    store.push(&manifest, staging.path()).expect("re-push");
    assert_eq!(
        after_first,
        relative_file_set(&base),
        "a re-push must not change the remote"
    );

    store.get_manifest(&id).expect("get_manifest");
    store.fetch_files(&manifest, cache.path()).expect("fetch");

    // Every emitted script placed its 0700 work dir (ControlPath socket
    // included) under this test's private TMPDIR, and each EXIT trap ran
    // `ssh -O exit` + `rm -rf`: no live `cm` master socket and no
    // `snapdir-ssh-store.*` dir may remain.
    assert_no_script_leaks(tmp.path());
}
