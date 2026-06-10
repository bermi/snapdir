//! Self-spawned **loopback sshd** fixture (no docker, no root): a temp dir
//! holding ed25519 host + user keys, an `authorized_keys`, a per-instance
//! `known_hosts`, and one `sshd -D -e -f <abs config>` child per requested
//! flavor, listening on a probed high port of `127.0.0.1` and killed (and
//! waited) on drop.
//!
//! # Flavors
//!
//! - [`Flavor::Shell`] — a normal account: shell allowed, `Subsystem sftp
//!   internal-sftp`. `set_env_path` optionally pins the session `PATH` via
//!   the server-side `SetEnv` directive (how the accel tests expose — or
//!   deliberately hide — a `snapdir` binary to the "remote": the remote side
//!   of `ssh 127.0.0.1` inherits sshd's session env, NOT the test env).
//! - [`Flavor::SftpOnly`] — same + `ForceCommand internal-sftp`: every exec
//!   request is replaced by the in-process SFTP server, so there is NO
//!   usable shell (`ChrootDirectory` needs root, so no-shell IS the property
//!   under test).
//!
//! # Skip policy (house pattern, cf. `snapdir-stores/src/s3_store.rs` live
//! tests)
//!
//! If `sshd` / `ssh` / `ssh-keygen` are missing the suite `eprintln!`-skips —
//! UNLESS `SNAPDIR_SSH_TEST_REQUIRE=1` (CI sets it so the suite can't rot
//! into all-skips), in which case it panics with a clear message.
//!
//! `SNAPDIR_SSH_TEST_HOST` is reserved as a documented FUTURE override
//! (point the suite at an externally-provisioned ssh server instead of the
//! self-spawned loopback one); it is intentionally not implemented yet.

use std::fmt::Write as _;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::{test_require, TempDir};

/// How long to wait for a spawned sshd to accept TCP connections.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Server flavor — see the module docs.
pub enum Flavor {
    /// Shell allowed + sftp subsystem; `set_env_path` pins the session
    /// `PATH` via `SetEnv` when given.
    Shell { set_env_path: Option<String> },
    /// `ForceCommand internal-sftp`: pure-SFTP, no usable shell.
    SftpOnly,
}

/// Locates the sshd server binary: the conventional sbin locations first
/// (macOS ships `/usr/sbin/sshd`, which is rarely on `PATH`), then `PATH`.
pub fn find_sshd() -> Option<PathBuf> {
    for candidate in [
        "/usr/sbin/sshd",
        "/usr/local/sbin/sshd",
        "/opt/homebrew/sbin/sshd",
    ] {
        let path = Path::new(candidate);
        if path.is_file() {
            return Some(path.to_owned());
        }
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("sshd"))
            .find(|candidate| candidate.is_file())
    })
}

/// `true` when `tool` can be spawned (presence check only; the exit status
/// is irrelevant — `ssh-keygen` exits nonzero without arguments).
fn tool_spawns(tool: &str, arg: &str) -> bool {
    Command::new(tool)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// The sshd binary, or a skip marker (`None`) when the OpenSSH tooling is
/// missing. Under `SNAPDIR_SSH_TEST_REQUIRE=1` missing tooling PANICS.
pub fn require_loopback_tooling(test: &str) -> Option<PathBuf> {
    let sshd = find_sshd();
    let missing = if sshd.is_none() {
        Some("sshd (server)")
    } else if !tool_spawns("ssh", "-V") {
        Some("ssh (client)")
    } else if !tool_spawns("ssh-keygen", "-?") {
        Some("ssh-keygen")
    } else {
        None
    };
    if let Some(missing) = missing {
        let msg = format!("{test}: OpenSSH tooling missing: {missing}");
        assert!(
            !test_require(),
            "SNAPDIR_SSH_TEST_REQUIRE=1 forbids skipping — {msg}"
        );
        eprintln!("SKIP {msg}");
        return None;
    }
    sshd
}

/// The key material + config dir shared by every server a test spawns:
/// `0700` temp dir, ed25519 host + user keys, `authorized_keys` (0600), and
/// a `known_hosts` that grows one `[127.0.0.1]:<port>` line per spawned
/// server.
pub struct SshKit {
    root: TempDir,
    sshd_bin: PathBuf,
    host_key: PathBuf,
    host_pub_line: String,
    authorized_keys: PathBuf,
    /// The client's private key (`*_STORE_IDENTITY_FILE`).
    pub user_key: PathBuf,
    /// The fixture `known_hosts` (`*_STORE_KNOWN_HOSTS`).
    pub known_hosts: PathBuf,
    spawned: u32,
}

impl SshKit {
    /// Builds the kit, or `None` on an (allowed) environmental skip — see
    /// [`require_loopback_tooling`].
    pub fn new(test: &str) -> Option<Self> {
        let sshd_bin = require_loopback_tooling(test)?;
        let root = TempDir::new("sshd-kit");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))
            .expect("chmod 700 kit dir");

        let host_key = root.path().join("host_key");
        let user_key = root.path().join("user_key");
        keygen(&host_key);
        keygen(&user_key);
        let host_pub_line = fs::read_to_string(root.path().join("host_key.pub"))
            .expect("read host pubkey")
            .trim()
            .to_owned();

        let authorized_keys = root.path().join("authorized_keys");
        fs::copy(root.path().join("user_key.pub"), &authorized_keys)
            .expect("install authorized_keys");
        fs::set_permissions(&authorized_keys, fs::Permissions::from_mode(0o600))
            .expect("chmod 600 authorized_keys");

        let known_hosts = root.path().join("known_hosts");
        fs::write(&known_hosts, "").expect("create known_hosts");

        Some(Self {
            root,
            sshd_bin,
            host_key,
            host_pub_line,
            authorized_keys,
            user_key,
            known_hosts,
            spawned: 0,
        })
    }

    /// The kit's `0700` temp dir (also a convenient parent for per-test
    /// remote store bases).
    pub fn dir(&self) -> &Path {
        self.root.path()
    }

    /// Spawns one sshd of the given `flavor` on a freshly probed loopback
    /// port (retrying the bind race a few times), waits until it accepts
    /// TCP connections, and appends its host-key line to [`Self::known_hosts`].
    pub fn spawn(&mut self, flavor: &Flavor) -> Sshd {
        for attempt in 1..=4 {
            let port = probe_free_port();
            self.spawned += 1;
            let n = self.spawned;
            let config = self.root.path().join(format!("sshd_config_{n}"));
            fs::write(&config, self.config_text(port, flavor)).expect("write sshd_config");
            let log_path = self.root.path().join(format!("sshd_{n}.log"));
            let log = fs::File::create(&log_path).expect("create sshd log");

            let mut child = Command::new(&self.sshd_bin)
                .arg("-D")
                .arg("-e")
                .arg("-f")
                .arg(&config) // absolute (macOS sshd requires it)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .expect("spawn sshd");

            if wait_ready(port, &mut child) {
                let entry = format!("[127.0.0.1]:{port} {}\n", self.host_pub_line);
                let mut known = fs::read_to_string(&self.known_hosts).unwrap_or_default();
                known.push_str(&entry);
                fs::write(&self.known_hosts, known).expect("append known_hosts");
                return Sshd { port, child };
            }

            let _ = child.kill();
            let _ = child.wait();
            eprintln!(
                "sshd attempt {attempt} on port {port} did not become ready \
                 (log: {}); retrying",
                log_path.display()
            );
        }
        panic!(
            "sshd failed to become ready after 4 attempts (see {}/sshd_*.log)",
            self.root.path().display()
        );
    }

    /// Writes a `known_hosts` carrying a WRONG (freshly generated decoy)
    /// host key for `[127.0.0.1]:<port>` — the host-key-fail-closed test's
    /// poisoned input.
    pub fn wrong_known_hosts(&self, port: u16) -> PathBuf {
        let decoy = self.root.path().join(format!("decoy_key_{port}"));
        keygen(&decoy);
        let decoy_pub = fs::read_to_string(self.root.path().join(format!("decoy_key_{port}.pub")))
            .expect("read decoy pubkey")
            .trim()
            .to_owned();
        let path = self.root.path().join(format!("known_hosts_wrong_{port}"));
        fs::write(&path, format!("[127.0.0.1]:{port} {decoy_pub}\n")).expect("write wrong kh");
        path
    }

    /// Runs one command over a DIRECT ssh client invocation (explicit flags,
    /// no env) — used to probe server-side behavior (e.g. whether `SetEnv
    /// PATH` is honored) independently of the engines under test.
    pub fn ssh_exec(&self, port: u16, command: &str) -> Output {
        Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "ConnectTimeout=10",
            ])
            .arg("-o")
            .arg(format!("IdentityFile={}", self.user_key.display()))
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", self.known_hosts.display()))
            .arg("-o")
            .arg(format!("Port={port}"))
            .arg("--")
            .arg("127.0.0.1")
            .arg(command)
            .stdin(Stdio::null())
            .output()
            .expect("run ssh")
    }

    /// The `sshd_config` for `port`/`flavor`. Absolute paths everywhere; the
    /// kit dir is `0700` and the key files `0600` (`StrictModes no` keeps
    /// sshd from second-guessing temp-dir lineage anyway); `UsePAM no` +
    /// pubkey-only auth so the server runs as the current (non-root) user.
    fn config_text(&self, port: u16, flavor: &Flavor) -> String {
        let mut text = format!(
            "ListenAddress 127.0.0.1\n\
             Port {port}\n\
             HostKey {host_key}\n\
             AuthorizedKeysFile {authorized_keys}\n\
             PubkeyAuthentication yes\n\
             PasswordAuthentication no\n\
             KbdInteractiveAuthentication no\n\
             UsePAM no\n\
             StrictModes no\n\
             PidFile none\n\
             LogLevel VERBOSE\n\
             Subsystem sftp internal-sftp\n",
            host_key = self.host_key.display(),
            authorized_keys = self.authorized_keys.display(),
        );
        match flavor {
            Flavor::Shell { set_env_path } => {
                if let Some(path) = set_env_path {
                    let _ = writeln!(text, "SetEnv PATH={path}");
                }
            }
            Flavor::SftpOnly => text.push_str("ForceCommand internal-sftp\n"),
        }
        text
    }
}

/// One running `sshd -D` child; killed and reaped on drop.
pub struct Sshd {
    /// The loopback port the server listens on.
    pub port: u16,
    child: Child,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn keygen(path: &Path) {
    let output = Command::new("ssh-keygen")
        .args(["-q", "-N", "", "-t", "ed25519", "-f"])
        .arg(path)
        .output()
        .expect("run ssh-keygen");
    assert!(
        output.status.success(),
        "ssh-keygen -f {} failed: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Binds `127.0.0.1:0`, takes the kernel-assigned port, drops the listener
/// (the small reuse race is covered by [`SshKit::spawn`]'s retry loop).
fn probe_free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind 127.0.0.1:0")
        .local_addr()
        .expect("local_addr")
        .port()
}

/// Polls `127.0.0.1:<port>` until sshd accepts (true) or it exits / the
/// timeout lapses (false).
fn wait_ready(port: u16, child: &mut Child) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            return false; // sshd died (config error / port race)
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}
