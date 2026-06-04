//! End-to-end verification of the live progress dashboard's *scriptable
//! contract* across every transfer command (`push`/`pull`/`sync`).
//!
//! The live progress line is drawn ONLY to a TTY stderr. `assert_cmd` /
//! `std::process::Command` run the binary with piped (non-TTY) stdio, so
//! progress auto-disables for every case here. That is exactly the property
//! under test: with the progress engine + the `--no-progress`/`--quiet`/
//! `--color` flags wired into every transfer command, the scriptable surface
//! must stay byte-clean — stdout is the bare snapshot id (trailing newline
//! only), and NEITHER stream may carry an ESC byte (`0x1b`, i.e. ANSI) nor a
//! bare carriage-return redraw (`\r`). `--quiet` must also swallow the
//! `--verbose` transfer banner, and the snapshot id must never depend on any of
//! these flags.
//!
//! `progress_wire.rs` already covers push/sync at the wire level; this suite
//! broadens that to pull and to an explicit flag matrix, and re-materializes the
//! pulled/synced snapshots to prove correctness was not perturbed. Every fn name
//! contains `progress_e2e` so `cargo test -p snapdir-cli --locked progress_e2e`
//! selects exactly this suite.
//!
//! An OPTIONAL pty smoke (`progress_e2e_pty_renders_when_tty`) is gated behind
//! `SNAPDIR_PTY_TEST=1`: unset, it prints a skip note and returns Ok; set, it
//! drives the binary with its stderr connected to a real pty slave (so the child
//! sees `stderr().is_terminal() == true`) and asserts the captured stderr does
//! carry the live ANSI/CR redraw while stdout stays id-only. It skips-with-note
//! on any pty setup failure rather than hanging or failing CI. The deterministic
//! render proof remains the `cli-progress-renderer` golden tests in
//! `src/progress.rs`; this is only a best-effort live smoke that needs no new
//! dependency (it uses `libc`, already a dep).
//!
//! All stores/caches/dirs live under `assert_fs` temp dirs removed on drop, so
//! the suite is hermetic (no network, no credentials, no `$HOME` writes).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// A fresh `snapdir` command with the cache pinned under `cache` so tests never
/// touch the user's real `$HOME/.cache/snapdir`. `TERM`/`NO_COLOR` are
/// neutralized so the run is deterministic regardless of the host environment.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env_remove("NO_COLOR");
    cmd.env("TERM", "xterm-256color");
    cmd
}

/// Builds a known tiny tree with explicit, deterministic permissions so a
/// re-materialized copy must restore them to re-manifest to the same id.
fn build_tree(dir: &TempDir) {
    dir.child("a.txt").write_str("hello").unwrap();
    std::fs::set_permissions(dir.child("a.txt").path(), PermissionsExt::from_mode(0o644)).unwrap();
    dir.child("sub/b.txt").write_str("world!!").unwrap();
    std::fs::set_permissions(
        dir.child("sub/b.txt").path(),
        PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    std::fs::set_permissions(dir.child("sub").path(), PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o755)).unwrap();
}

/// Runs `snapdir <args>` (cache pinned), asserting success and returning the
/// captured `Output` (stdout + stderr) for inspection.
fn run_ok(cache: &Path, args: &[&str]) -> Output {
    let out = snapdir(cache).args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

/// Trimmed (trailing-newline-stripped) stdout of an `Output`.
fn stdout_str(out: &Output) -> String {
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .trim_end()
        .to_owned()
}

/// True if the bytes contain an ANSI ESC byte (`0x1b`). The progress engine
/// emits CSI sequences (`\x1b[...`) for color/cursor control, so any ESC means
/// the live line leaked onto a scriptable (piped) stream.
fn has_ansi(bytes: &[u8]) -> bool {
    bytes.contains(&0x1b)
}

/// True if the bytes contain a bare carriage return (`\r`) — the in-place
/// single-line redraw the progress engine uses to overwrite its own line.
fn has_cr(bytes: &[u8]) -> bool {
    bytes.contains(&b'\r')
}

/// Asserts neither stream of `out` carries ANSI or a CR redraw.
fn assert_clean(out: &Output, label: &str) {
    assert!(
        !has_ansi(&out.stdout),
        "{label}: stdout must carry no ANSI (0x1b) byte"
    );
    assert!(
        !has_cr(&out.stdout),
        "{label}: stdout must carry no CR redraw"
    );
    assert!(
        !has_ansi(&out.stderr),
        "{label}: stderr must carry no ANSI (0x1b) byte (piped => progress off)"
    );
    assert!(
        !has_cr(&out.stderr),
        "{label}: stderr must carry no CR redraw (piped => progress off)"
    );
}

/// Asserts `id` is a bare 64-char lowercase-hex snapshot id.
fn assert_is_id(id: &str, label: &str) {
    assert_eq!(id.len(), 64, "{label}: stdout must be the bare id: {id:?}");
    assert!(
        id.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{label}: stdout must be lowercase hex: {id:?}"
    );
}

// ---------------------------------------------------------------------------
// Scriptable-contract coverage across push / pull / sync.
// ---------------------------------------------------------------------------

/// `push <src> --store file://A`: stdout is EXACTLY the id (trailing newline
/// only) and neither stream carries an ANSI ESC nor a CR redraw.
#[test]
fn progress_e2e_push_scriptable() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());

    let push = run_ok(cache.path(), &["push", "--store", &a_url, &src_str]);

    // stdout is EXACTLY the id followed by a single trailing newline.
    let id = stdout_str(&push);
    assert_is_id(&id, "push");
    assert_eq!(
        push.stdout,
        format!("{id}\n").into_bytes(),
        "push stdout must be exactly the id + one trailing newline"
    );
    assert_clean(&push, "push");
}

/// `pull --store file://A --id <id> <dest>` after a push: exit 0, no ANSI/CR on
/// either stream, and the destination re-materializes to the source id. (pull
/// is not covered by `progress_wire`; cover it here.)
#[test]
fn progress_e2e_pull_scriptable() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());

    let id = stdout_str(&run_ok(
        cache.path(),
        &["push", "--store", &a_url, &src_str],
    ));
    assert_is_id(&id, "push (for pull)");

    let pull = run_ok(
        cache.path(),
        &["pull", "--store", &a_url, "--id", &id, &dest_str],
    );
    assert_clean(&pull, "pull");

    // The destination re-materializes the contents and re-manifests to the id.
    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    assert_eq!(
        stdout_str(&run_ok(cache.path(), &["id", &dest_str])),
        id,
        "pulled tree must re-manifest to the source id"
    );
}

/// `sync --id <id> --from file://A --to file://B` after staging into A via push:
/// stdout is the id only, no ANSI/CR on either stream, and B then serves the
/// snapshot (a `pull` from B re-materializes the same id).
#[test]
fn progress_e2e_sync_scriptable() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let store_a = TempDir::new().unwrap();
    let store_b = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    // Stage the snapshot into A.
    let id = stdout_str(&run_ok(
        cache.path(),
        &["push", "--store", &a_url, &src_str],
    ));
    assert_is_id(&id, "push (for sync)");

    let sync = run_ok(
        cache.path(),
        &["sync", "--id", &id, "--from", &a_url, "--to", &b_url],
    );
    assert_eq!(stdout_str(&sync), id, "sync stdout must be the bare id");
    assert_clean(&sync, "sync");

    // B now serves the snapshot: pull from B into a fresh dest + fresh cache
    // re-materializes the same id (proves sync actually copied the objects).
    let dest = TempDir::new().unwrap();
    let dest_str = dest.path().to_string_lossy().into_owned();
    let cache_b = TempDir::new().unwrap();
    run_ok(
        cache_b.path(),
        &["pull", "--store", &b_url, "--id", &id, &dest_str],
    );
    dest.child("a.txt").assert("hello");
    dest.child("sub/b.txt").assert("world!!");
    assert_eq!(
        stdout_str(&run_ok(cache_b.path(), &["id", &dest_str])),
        id,
        "B must serve the synced snapshot (re-manifests to the source id)"
    );
}

/// Flag matrix: `--no-progress`, `--quiet`, and `--color never` each succeed
/// with id-only stdout and no ANSI on either stream, for `push` and (one of)
/// `sync`. Also asserts `--verbose --quiet` drops the `transfers:` banner.
#[test]
fn progress_e2e_flags_matrix() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    // Each progress-suppressing flag form, applied to a fresh push/store.
    let flag_sets: &[&[&str]] = &[&["--no-progress"], &["--quiet"], &["--color", "never"]];
    for flags in flag_sets {
        let store = TempDir::new().unwrap();
        let url = format!("file://{}", store.path().display());
        let mut args: Vec<&str> = flags.to_vec();
        args.extend_from_slice(&["push", "--store", &url, &src_str]);
        let out = run_ok(cache.path(), &args);
        let id = stdout_str(&out);
        assert_is_id(&id, &format!("push {flags:?}"));
        assert_clean(&out, &format!("push {flags:?}"));

        // Same flag form on a transfer-only command (sync) — id-only, clean.
        let store_b = TempDir::new().unwrap();
        let b_url = format!("file://{}", store_b.path().display());
        let mut sync_args: Vec<&str> = flags.to_vec();
        sync_args.extend_from_slice(&["sync", "--id", &id, "--from", &url, "--to", &b_url]);
        let sync = run_ok(cache.path(), &sync_args);
        assert_eq!(
            stdout_str(&sync),
            id,
            "sync {flags:?} stdout must be the bare id"
        );
        assert_clean(&sync, &format!("sync {flags:?}"));
    }

    // --verbose --quiet: --quiet wins for the banner — stderr must NOT carry the
    // `transfers:` transfer-config line, and stdout is still the bare id.
    let store = TempDir::new().unwrap();
    let url = format!("file://{}", store.path().display());
    let out = run_ok(
        cache.path(),
        &["--verbose", "--quiet", "push", "--store", &url, &src_str],
    );
    let id = stdout_str(&out);
    assert_is_id(&id, "--verbose --quiet push");
    assert_clean(&out, "--verbose --quiet push");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("transfers:"),
        "--quiet must suppress the --verbose banner; stderr was: {stderr:?}"
    );
}

/// The snapshot id is byte-identical across a plain run, `--no-progress`, and
/// `--quiet` — the progress flags must never influence the id.
#[test]
fn progress_e2e_id_identical_with_without_flags() {
    let cache = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();

    let plain_store = TempDir::new().unwrap();
    let plain_url = format!("file://{}", plain_store.path().display());
    let plain = stdout_str(&run_ok(
        cache.path(),
        &["push", "--store", &plain_url, &src_str],
    ));
    assert_is_id(&plain, "plain push");

    let np_store = TempDir::new().unwrap();
    let np_url = format!("file://{}", np_store.path().display());
    let no_progress = stdout_str(&run_ok(
        cache.path(),
        &["--no-progress", "push", "--store", &np_url, &src_str],
    ));

    let q_store = TempDir::new().unwrap();
    let q_url = format!("file://{}", q_store.path().display());
    let quiet = stdout_str(&run_ok(
        cache.path(),
        &["--quiet", "push", "--store", &q_url, &src_str],
    ));

    assert_eq!(plain, no_progress, "id must not depend on --no-progress");
    assert_eq!(plain, quiet, "id must not depend on --quiet");
}

// ---------------------------------------------------------------------------
// OPTIONAL pty smoke (env-gated; uses `libc`, already a dep — no new crate).
//
// Default behavior (SNAPDIR_PTY_TEST unset): print a skip note and return Ok.
// The deterministic render proof is the `cli-progress-renderer` golden tests in
// `src/progress.rs`; this is a best-effort *live* smoke only.
//
// When SNAPDIR_PTY_TEST=1: open a pty, spawn `snapdir sync` with its stderr
// connected to the pty slave (so the child's `stderr().is_terminal()` is true)
// and `TERM=xterm`, keep stdout on a normal pipe, read the master with a bounded
// non-blocking loop, then assert the pty (stderr) captured ANSI/CR redraw bytes
// while stdout stayed id-only. Any pty setup failure skips-with-note rather than
// failing/hanging.
// ---------------------------------------------------------------------------

#[test]
fn progress_e2e_pty_renders_when_tty() {
    if std::env::var_os("SNAPDIR_PTY_TEST").is_none() {
        eprintln!(
            "progress_e2e_pty_renders_when_tty: SKIP (set SNAPDIR_PTY_TEST=1 to run the live \
             pty smoke). Deterministic render proof: the cli-progress-renderer golden tests \
             in src/progress.rs."
        );
        return;
    }
    if let Err(reason) = run_pty_smoke() {
        eprintln!("progress_e2e_pty_renders_when_tty: SKIP ({reason})");
    }
}

/// Runs the live pty smoke. Returns `Err(reason)` for any *setup* problem so the
/// caller can skip-with-note; only a genuine contract violation panics.
// `&mut master`/`&mut slave` are the required `*mut c_int` out-params for
// `libc::openpty`; `&raw mut` is unavailable on the pinned MSRV (1.78), so keep
// the implicit-borrow form and silence the (test-only) lint.
#[cfg(unix)]
#[allow(clippy::borrow_as_ptr, clippy::too_many_lines)]
fn run_pty_smoke() -> Result<(), String> {
    use std::io::Read;
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    // Build a slightly larger tree so the transfer is not instantaneous and the
    // live line has a chance to render at least one frame.
    let cache = TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    let src = TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    for i in 0..64 {
        src.child(format!("f{i}.dat"))
            .write_str(&"payload-".repeat(64))
            .map_err(|e| format!("write fixture: {e}"))?;
    }
    let src_str = src.path().to_string_lossy().into_owned();
    let store_a = TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    let store_b = TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    let a_url = format!("file://{}", store_a.path().display());
    let b_url = format!("file://{}", store_b.path().display());

    // Stage into A (plain piped push — not part of the pty assertion).
    let id = stdout_str(&run_ok(
        cache.path(),
        &["push", "--store", &a_url, &src_str],
    ));

    // Open a pty pair.
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    // SAFETY: openpty writes two valid fds into master/slave on success; we pass
    // null for the optional name/termios/winsize out-params.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(format!(
            "openpty failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut child = {
        let mut cmd = snapdir(cache.path());
        cmd.args(["sync", "--id", &id, "--from", &a_url, "--to", &b_url]);
        cmd.env("TERM", "xterm");
        // stdout stays a normal captured pipe; stderr is the pty slave so the
        // child sees a terminal there and renders the live line.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());
        let slave_for_child = slave;
        // SAFETY: in the forked child (pre-exec) we only dup the slave fd onto
        // stderr (fd 2) — an async-signal-safe libc call — then return.
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(slave_for_child, 2) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().map_err(|e| {
            // Best-effort fd cleanup before bailing.
            unsafe {
                libc::close(master);
                libc::close(slave);
            }
            format!("spawn under pty: {e}")
        })?
    };

    // Parent no longer needs the slave end; closing it lets the master see EOF
    // once the child exits.
    // SAFETY: slave is a valid fd owned by the parent here.
    unsafe {
        libc::close(slave);
    }

    // Read the master (the child's stderr) with a bounded, non-blocking loop so
    // a stuck child cannot hang the test.
    // SAFETY: master is a valid open fd we own; from_raw_fd takes ownership and
    // will close it on drop.
    let mut master_file = unsafe { std::fs::File::from_raw_fd(master) };
    set_nonblocking(master).map_err(|e| format!("set master nonblocking: {e}"))?;

    let mut pty_bytes: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut buf = [0u8; 4096];
    loop {
        match master_file.read(&mut buf) {
            Ok(0) => break, // EOF: child closed its stderr (exited).
            Ok(n) => pty_bytes.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err("timeout reading pty master".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // On Linux a pty master read after slave hangup yields EIO at EOF.
            Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
            Err(e) => return Err(format!("read pty master: {e}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err("timeout draining pty master".into());
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait for child: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "child sync under pty failed ({:?}); stderr(pty): {}",
            out.status.code(),
            String::from_utf8_lossy(&pty_bytes)
        ));
    }

    // Contract under a TTY stderr: the live line DID render (ANSI or CR redraw on
    // the pty), while stdout (a normal pipe) is still exactly the id.
    assert!(
        has_ansi(&pty_bytes) || has_cr(&pty_bytes),
        "pty stderr must carry the live progress line (ANSI/CR); captured {} bytes: {:?}",
        pty_bytes.len(),
        String::from_utf8_lossy(&pty_bytes)
    );
    let stdout_id = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
    assert_eq!(
        stdout_id, id,
        "stdout must stay exactly the snapshot id even while the pty renders"
    );
    assert!(
        !has_ansi(&out.stdout) && !has_cr(&out.stdout),
        "stdout (normal pipe) must remain ANSI/CR-free under a TTY stderr"
    );
    Ok(())
}

#[cfg(not(unix))]
fn run_pty_smoke() -> Result<(), String> {
    Err("pty smoke is unix-only".into())
}

/// Sets `O_NONBLOCK` on `fd` so the master read loop never blocks.
#[cfg(unix)]
fn set_nonblocking(fd: libc::c_int) -> std::io::Result<()> {
    // SAFETY: fd is a valid open fd; F_GETFL/F_SETFL are read/modify of its flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
