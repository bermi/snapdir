//! Phase-30 adversarial spec-tests for the live-progress DX fix
//! (gate `dx-progress-spec-tests`, AUTHORING / BLACK-BOX).
//!
//! THE BUG BEING PINNED (operator's headline finding): `snapdir`'s live
//! progress is useless today — the "files" denominator is actually the total
//! BYTE count (e.g. "209715200 files" for a 200 MiB tree), the bar never fills,
//! and the percentage is frozen at 0%. Root cause: the walk's discovery phase
//! is silent and `objects_total` is never set to the FILE COUNT before hashing.
//!
//! THE FIX (NOT yet implemented) must:
//!   1. show a VISIBLE discovery/enumeration phase, then
//!   2. set the real FILE-COUNT total before hashing, so the hash phase renders
//!      a true determinate %/fraction that advances from 0 -> total.
//!
//! These tests are deliberately authored from the SPEC ALONE; the feature's
//! `src/` was NOT read. Clauses 1-3 are EXPECTED TO FAIL against the current
//! binary (progress is broken); clauses 5-7 (snapshot-id determinism + stdout
//! cleanliness) should already largely hold and are written honestly, not
//! weakened.
//!
//! ## How PTY frames are captured
//!
//! The live line renders ONLY when `stderr().is_terminal()` is true. We mirror
//! the `progress_e2e.rs` harness: `libc::openpty` makes a pty pair, the child is
//! spawned with `pre_exec` dup'ing the pty SLAVE onto fd 2 (stderr), so the
//! child sees a real terminal there and renders the live progress line. The
//! parent reads the pty MASTER (a bounded, non-blocking loop so a stuck child
//! can never hang the suite) — those bytes are the rendered stderr frames. We
//! ALSO set `SNAPDIR_PTY_TEST=1` in the child env (the documented hook that
//! forces progress rendering under test); the real pty already satisfies
//! `is_terminal()`, so the env is belt-and-suspenders.
//!
//! Frames are split on the in-place redraw boundaries (`\r` and `\n`), each
//! frame is ANSI-stripped (CSI sequences removed), and we scan the resulting
//! plain text. "Files-not-bytes" is asserted by extracting the numeric
//! denominator of any `done/total` fraction (or the standalone count behind a
//! files-ish label) and proving it is a plausible FILE count (< 100_000, near
//! the tree's ~2089) and NOT the multi-million BYTE total.
//!
//! ## Env gating (CI-safe)
//!
//! The PTY-rendering tests (clauses 1-3, 6-pty) run only when `SNAPDIR_PTY_TEST`
//! is set in the OUTER environment; unset, they skip-with-note (mirroring
//! `progress_e2e.rs`) so they never hang headless CI. The pure-stdout
//! determinism / cleanliness tests (clauses 5, 7, and the empty-dir exit check)
//! need no pty and run UNCONDITIONALLY.
//!
//! All fixtures live under `assert_fs` temp dirs (hermetic). The 2089-file
//! sandbox tree at `.gatesmith/evidence/dx-sandbox/tree` is used when present
//! (`sh utils/dx/build-sandbox.sh` builds it); otherwise an in-test tree of a
//! few hundred files is built so the suite is self-contained.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use assert_cmd::prelude::*;
use assert_fs::prelude::*;
use assert_fs::TempDir;

/// The known, frozen snapshot id of the dx-sandbox tree. Pinning this proves the
/// progress changes do NOT perturb the snapshot id (keystone, clause 5).
const SANDBOX_ID: &str = "ce2b0312911e5da75719a3fb3a23922252583a0a1b5c2044cea48ce0dcab8399";

/// The dx-sandbox tree really contains this many files (clause 1 sanity bound).
const SANDBOX_FILE_COUNT: u64 = 2089;

/// A fresh `snapdir` command with the cache pinned so tests never touch the
/// user's real `$HOME/.cache/snapdir`. `TERM` is set so the renderer has a
/// terminal type and `NO_COLOR` is cleared for determinism.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env_remove("NO_COLOR");
    cmd.env("TERM", "xterm-256color");
    cmd
}

/// Resolve the committed dx-sandbox tree (2089 files). Returns `None` if absent
/// so the caller can fall back to an in-test tree.
fn sandbox_tree() -> Option<PathBuf> {
    // Tests run with CWD = crate dir (crates/snapdir-cli); the sandbox lives at
    // the repo root under .gatesmith/. Walk up to find it.
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".gatesmith/evidence/dx-sandbox/tree");
        if candidate.join("README.md").is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Build a multi-hundred-file tree in `dir` so the suite is self-contained when
/// the committed sandbox is absent. Returns the number of files written.
fn build_many_file_tree(dir: &TempDir) -> u64 {
    let n = 600u64;
    for i in 0..n {
        let sub = i % 12;
        dir.child(format!("d{sub:02}/f{i:05}.bin"))
            .write_str(&"payload-block-".repeat(48))
            .unwrap();
    }
    // a couple of empty files to exercise the 0-byte edge
    dir.child("edge/empty_a.bin").write_str("").unwrap();
    dir.child("edge/empty_b.bin").write_str("").unwrap();
    n + 2
}

/// Run `snapdir <args>` (cache pinned, all stdio piped => progress auto-off),
/// asserting success, returning captured output.
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

/// Trailing-newline-trimmed stdout.
fn stdout_str(out: &Output) -> String {
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .trim_end()
        .to_owned()
}

/// True if `bytes` contains an ANSI ESC (`0x1b`) — the progress line's CSI
/// sequences begin with it; on a scriptable (piped) stream it must be absent.
fn has_ansi(bytes: &[u8]) -> bool {
    bytes.contains(&0x1b)
}

/// True if `bytes` contains a bare CR (`\r`) — the in-place single-line redraw.
fn has_cr(bytes: &[u8]) -> bool {
    bytes.contains(&b'\r')
}

/// Assert `id` is a bare 64-char lowercase-hex snapshot id.
fn assert_is_id(id: &str, label: &str) {
    assert_eq!(id.len(), 64, "{label}: stdout must be a bare id: {id:?}");
    assert!(
        id.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{label}: id must be lowercase hex: {id:?}"
    );
}

/// Strip ANSI/CSI escape sequences (`\x1b[...m`, cursor moves, etc.) from a
/// frame, leaving plain text we can scan for numbers and phase words.
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC: skip an optional '[' then everything up to the final byte in
            // the CSI range 0x40..=0x7e (the command letter).
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1; // consume the final command byte
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Split a raw pty capture into the sequence of rendered frames. The live line
/// redraws by overwriting itself with `\r`; phases and the final line end with
/// `\n`. Splitting on BOTH yields the individual frame texts (ANSI-stripped,
/// trimmed). Empty frames are dropped.
fn frames(raw: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    text.split(['\r', '\n'])
        .map(strip_ansi)
        .map(|f| f.trim().to_owned())
        .filter(|f| !f.is_empty())
        .collect()
}

/// All run of ASCII digits in `s`, parsed as u64.
fn numbers_in(s: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            cur.push(ch);
        } else if !cur.is_empty() {
            if let Ok(v) = cur.parse::<u64>() {
                out.push(v);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() {
        if let Ok(v) = cur.parse::<u64>() {
            out.push(v);
        }
    }
    out
}

/// Extract candidate FRACTION denominators from a frame: the `total` side of any
/// `done/total` pair (digits, optional spaces, `/`, digits). The fix renders the
/// file-count fraction here.
fn fraction_denominators(frame: &str) -> Vec<u64> {
    let bytes = frame.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            // walk left over the numerator (skip; we only need the denom),
            // walk right over the denominator digits.
            let mut j = i + 1;
            // allow a single space after the slash
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            // require a digit immediately before the slash too (it's a fraction,
            // not a path) — look left skipping one optional space.
            let mut k = i;
            if k > 0 && bytes[k - 1] == b' ' {
                k -= 1;
            }
            let left_is_digit = k > 0 && bytes[k - 1].is_ascii_digit();
            if left_is_digit && j > start {
                if let Ok(v) = frame[start..j].parse::<u64>() {
                    out.push(v);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Extract `done/total` pairs (both sides) from a frame.
fn fraction_pairs(frame: &str) -> Vec<(u64, u64)> {
    let bytes = frame.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            // numerator: digits immediately left (allow one space)
            let mut l = i;
            if l > 0 && bytes[l - 1] == b' ' {
                l -= 1;
            }
            let num_end = l;
            while l > 0 && bytes[l - 1].is_ascii_digit() {
                l -= 1;
            }
            let num_start = l;
            // denominator: digits immediately right (allow one space)
            let mut r = i + 1;
            while r < bytes.len() && bytes[r] == b' ' {
                r += 1;
            }
            let den_start = r;
            while r < bytes.len() && bytes[r].is_ascii_digit() {
                r += 1;
            }
            if num_end > num_start && r > den_start {
                if let (Ok(n), Ok(d)) = (
                    frame[num_start..num_end].parse::<u64>(),
                    frame[den_start..r].parse::<u64>(),
                ) {
                    out.push((n, d));
                }
            }
            i = r.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Extract every `NN%` percentage value in a frame.
fn percents(frame: &str) -> Vec<u64> {
    let bytes = frame.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let mut l = i;
            while l > 0 && bytes[l - 1].is_ascii_digit() {
                l -= 1;
            }
            if l < i {
                if let Ok(v) = frame[l..i].parse::<u64>() {
                    out.push(v);
                }
            }
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// PTY harness (mirrors progress_e2e.rs). Spawns `snapdir <args>` with the pty
// SLAVE dup'd onto child stderr (so it renders the live line) and STDOUT on a
// normal pipe; returns (raw_stderr_pty_bytes, child Output). Any *setup* failure
// returns Err so callers can skip-with-note rather than fail/hang.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[allow(clippy::borrow_as_ptr, clippy::too_many_lines)]
fn run_under_pty(cache: &Path, args: &[&str]) -> Result<(Vec<u8>, Output), String> {
    use std::io::Read;
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    // SAFETY: openpty writes two valid fds; null out-params for name/termios/winsize.
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
        let mut cmd = snapdir(cache);
        cmd.args(args);
        cmd.env("TERM", "xterm");
        // Documented under-test hook that forces progress rendering; harmless
        // here since the pty already makes stderr a terminal.
        cmd.env("SNAPDIR_PTY_TEST", "1");
        cmd.stdout(Stdio::piped());
        cmd.stdin(Stdio::null());
        let slave_for_child = slave;
        // SAFETY: in the forked child (pre-exec) we only dup the slave fd onto
        // stderr (fd 2) — async-signal-safe — then return.
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(slave_for_child, 2) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().map_err(|e| {
            unsafe {
                libc::close(master);
                libc::close(slave);
            }
            format!("spawn under pty: {e}")
        })?
    };

    // Parent drops the slave so the master sees EOF when the child exits.
    // SAFETY: slave is a valid fd owned by the parent here.
    unsafe {
        libc::close(slave);
    }

    // SAFETY: master is a valid fd we own; from_raw_fd takes ownership.
    let mut master_file = unsafe { std::fs::File::from_raw_fd(master) };
    set_nonblocking(master).map_err(|e| format!("set master nonblocking: {e}"))?;

    let mut pty_bytes: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut buf = [0u8; 8192];
    loop {
        match master_file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => pty_bytes.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err("timeout reading pty master".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
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
    Ok((pty_bytes, out))
}

#[cfg(not(unix))]
fn run_under_pty(_cache: &Path, _args: &[&str]) -> Result<(Vec<u8>, Output), String> {
    Err("pty harness is unix-only".into())
}

#[cfg(unix)]
fn set_nonblocking(fd: libc::c_int) -> std::io::Result<()> {
    // SAFETY: fd is a valid open fd; F_GETFL/F_SETFL read/modify its flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// True when the outer env opted into the (potentially hanging) live pty tests.
/// Unset => skip-with-note, mirroring `progress_e2e.rs`.
fn pty_enabled() -> bool {
    std::env::var_os("SNAPDIR_PTY_TEST").is_some()
}

/// Skip helper: prints a uniform note and signals the caller to early-return.
fn skip_unless_pty(test: &str) -> bool {
    if pty_enabled() {
        return false;
    }
    eprintln!(
        "{test}: SKIP (set SNAPDIR_PTY_TEST=1 to run the live pty progress tests). \
         Determinism/cleanliness clauses 5 & 7 still run headless."
    );
    true
}

/// Prepare a cache + the tree under test. Prefer the committed 2089-file sandbox
/// (and report its true file count); else build an in-test multi-hundred tree in
/// a TempDir the caller must keep alive. Returns (cache, tree_path, file_count,
/// _keepalive).
fn prepare_tree() -> (TempDir, PathBuf, u64, Option<TempDir>) {
    let cache = TempDir::new().unwrap();
    if let Some(tree) = sandbox_tree() {
        (cache, tree, SANDBOX_FILE_COUNT, None)
    } else {
        let dir = TempDir::new().unwrap();
        let n = build_many_file_tree(&dir);
        let path = dir.path().to_path_buf();
        (cache, path, n, Some(dir))
    }
}

// ===========================================================================
// Clause 1 — DENOMINATOR IS FILES, NOT BYTES (the precise bug).
// ===========================================================================

/// SPEC clause 1: during `id`/`stage` of the tree, the progress total/denominator
/// must equal the FILE COUNT (~2089), NOT the byte total (tens of millions).
/// We collect every fraction denominator and every standalone count rendered and
/// require that the dominant "total" is a plausible FILE count (< 100_000, and in
/// the same order of magnitude as the real file count) — and that NO frame ever
/// presents the byte total (>= 1_000_000) as the progress denominator/total.
#[test]
fn dx_progress_denominator_is_files_not_bytes() {
    if skip_unless_pty("dx_progress_denominator_is_files_not_bytes") {
        return;
    }
    let (cache, tree, file_count, _keep) = prepare_tree();
    let tree_str = tree.to_string_lossy().into_owned();

    let (pty, out) = match run_under_pty(cache.path(), &["id", &tree_str]) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("dx_progress_denominator_is_files_not_bytes: SKIP ({reason})");
            return;
        }
    };
    assert!(
        out.status.success(),
        "id under pty must succeed; stderr(pty): {}",
        String::from_utf8_lossy(&pty)
    );

    let fs = frames(&pty);
    assert!(
        !fs.is_empty(),
        "progress must render at least one frame on a pty; captured {} bytes",
        pty.len()
    );

    // The plausible-file-count ceiling: well above any tree we test, far below
    // the byte total of a multi-MB tree.
    const FILE_CEIL: u64 = 100_000;
    // The byte total of the tree is millions; treat any "total" >= this as the
    // smoking-gun bytes-as-files bug.
    const BYTES_FLOOR: u64 = 1_000_000;

    // Collect every fraction denominator across all frames.
    let mut denoms: Vec<u64> = Vec::new();
    for f in &fs {
        denoms.extend(fraction_denominators(f));
    }

    // There MUST be at least one determinate fraction whose denominator is a
    // plausible file count near the real count.
    let plausible_total = denoms.iter().copied().find(|&d| {
        d > 0 && d < FILE_CEIL && d >= file_count / 4 && d <= file_count.saturating_mul(4)
    });
    assert!(
        plausible_total.is_some(),
        "no frame showed a FILE-COUNT denominator near {file_count}; \
         denominators seen = {denoms:?}; frames = {fs:?}"
    );

    // And NO fraction denominator may be the byte total (the exact bug: bytes
    // rendered as the 'files' total).
    let byteish: Vec<u64> = denoms
        .iter()
        .copied()
        .filter(|&d| d >= BYTES_FLOOR)
        .collect();
    assert!(
        byteish.is_empty(),
        "progress denominator must be FILES, not BYTES — saw byte-sized totals \
         {byteish:?} (>= {BYTES_FLOOR}); frames = {fs:?}"
    );

    // Defense-in-depth: a frame that literally labels a byte-sized count as
    // "files" is the headline bug — forbid it outright.
    for f in &fs {
        let lower = f.to_lowercase();
        if lower.contains("file") {
            for n in numbers_in(f) {
                assert!(
                    n < BYTES_FLOOR,
                    "a 'files'-labelled frame shows a byte-sized count {n}: {f:?}"
                );
            }
        }
    }
}

// ===========================================================================
// Clause 2 — DETERMINATE, ADVANCING % (not stuck at 0%, not jump-to-100).
// ===========================================================================

/// SPEC clause 2: the rendered frame stream must show a DETERMINATE indicator
/// that advances — at least one frame with a real `NN%` (NN>0) OR a `done/total`
/// fraction with 0 < done <= total. The current bug is frozen at 0% / shows an
/// indeterminate "N files" only. We also reject a stream that ONLY ever shows 0%
/// or 100% with nothing in between when there are enough files to expect motion.
#[test]
fn dx_progress_determinate_and_advancing() {
    if skip_unless_pty("dx_progress_determinate_and_advancing") {
        return;
    }
    let (cache, tree, _file_count, _keep) = prepare_tree();
    let tree_str = tree.to_string_lossy().into_owned();

    let (pty, out) = match run_under_pty(cache.path(), &["id", &tree_str]) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("dx_progress_determinate_and_advancing: SKIP ({reason})");
            return;
        }
    };
    assert!(out.status.success(), "id under pty must succeed");

    let fs = frames(&pty);
    assert!(!fs.is_empty(), "progress must render frames on a pty");

    // (a) A real, positive percentage somewhere.
    let positive_pct = fs
        .iter()
        .flat_map(|f| percents(f))
        .any(|p| p > 0 && p <= 100);

    // (b) OR a determinate fraction with done>0 and done<=total.
    let advancing_fraction = fs
        .iter()
        .flat_map(|f| fraction_pairs(f))
        .any(|(done, total)| total > 0 && done > 0 && done <= total);

    assert!(
        positive_pct || advancing_fraction,
        "progress must be DETERMINATE and advancing — no frame showed a positive \
         percentage or a 0<done<=total fraction (the 0%-frozen bug). frames = {fs:?}"
    );

    // (c) Not frozen at exactly 0% for the whole run: if any percentage rendered,
    // at least one must exceed 0.
    let all_pcts: Vec<u64> = fs.iter().flat_map(|f| percents(f)).collect();
    if !all_pcts.is_empty() {
        assert!(
            all_pcts.iter().any(|&p| p > 0),
            "percentage rendered but stayed frozen at 0% throughout: {all_pcts:?}"
        );
    }
}

// ===========================================================================
// Clause 3 — VISIBLE DISCOVERY PHASE.
// ===========================================================================

/// SPEC clause 3: a discovery/enumeration phase must be visible BEFORE hashing
/// completes — either a frame containing a discovery-ish phase word
/// (discover/enumerat/scan/walk/index/count/find), OR a growing file count seen
/// during enumeration. The current bug leaves the long discovery phase silent.
#[test]
fn dx_progress_discovery_phase_is_visible() {
    if skip_unless_pty("dx_progress_discovery_phase_is_visible") {
        return;
    }
    let (cache, tree, _file_count, _keep) = prepare_tree();
    let tree_str = tree.to_string_lossy().into_owned();

    let (pty, out) = match run_under_pty(cache.path(), &["id", &tree_str]) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("dx_progress_discovery_phase_is_visible: SKIP ({reason})");
            return;
        }
    };
    assert!(out.status.success(), "id under pty must succeed");

    let fs = frames(&pty);
    assert!(!fs.is_empty(), "progress must render frames on a pty");

    // (a) An explicit discovery-ish phase word anywhere in the stream.
    const PHASE_WORDS: &[&str] = &[
        "discover",
        "enumerat",
        "scan",
        "walk",
        "index",
        "count",
        "finding",
        "find files",
    ];
    let phase_word = fs.iter().any(|f| {
        let l = f.to_lowercase();
        PHASE_WORDS.iter().any(|w| l.contains(w))
    });

    // (b) OR a count that grows over the stream (enumeration counting up) before
    // the final determinate hashing fraction appears.
    let counts: Vec<u64> = fs
        .iter()
        .map(|f| numbers_in(f).into_iter().max().unwrap_or(0))
        .collect();
    let growing = counts.windows(2).any(|w| w[1] > w[0]);

    assert!(
        phase_word || growing,
        "no VISIBLE discovery phase — no discovery-ish phase word and no growing \
         enumeration count before hashing. frames = {fs:?}"
    );
}

// ===========================================================================
// Clause 4 — --no-progress and --quiet SUPPRESS ALL PROGRESS (under a pty).
// ===========================================================================

/// SPEC clause 4: under the SAME pty stderr, `id --no-progress <tree>` and
/// `id --quiet <tree>` must emit NO progress frames — no ANSI, no CR redraw —
/// even though stderr IS a terminal. stdout stays exactly the id.
#[test]
fn dx_progress_no_progress_and_quiet_suppress_all() {
    if skip_unless_pty("dx_progress_no_progress_and_quiet_suppress_all") {
        return;
    }
    let (cache, tree, _file_count, _keep) = prepare_tree();
    let tree_str = tree.to_string_lossy().into_owned();

    for flag in ["--no-progress", "--quiet"] {
        let (pty, out) = match run_under_pty(cache.path(), &["id", flag, &tree_str]) {
            Ok(v) => v,
            Err(reason) => {
                eprintln!("dx_progress_no_progress_and_quiet_suppress_all: SKIP ({reason})");
                return;
            }
        };
        assert!(out.status.success(), "id {flag} under pty must succeed");

        assert!(
            !has_ansi(&pty),
            "id {flag}: progress suppressed => no ANSI on the pty stderr; got {:?}",
            String::from_utf8_lossy(&pty)
        );
        assert!(
            !has_cr(&pty),
            "id {flag}: progress suppressed => no CR redraw on the pty stderr; got {:?}",
            String::from_utf8_lossy(&pty)
        );

        let id = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
        assert_is_id(&id, &format!("id {flag} stdout"));
    }
}

// ===========================================================================
// Clause 5 — KEYSTONE DETERMINISM (snapshot-id freeze).
// ===========================================================================

/// SPEC clause 5 (KEYSTONE): `id` STDOUT is byte-identical with progress ON vs
/// `--no-progress`, and equals the frozen sandbox id — progress must never
/// perturb the snapshot id, and never leak a fragment into stdout. Progress-ON
/// is exercised via the pty (real terminal stderr); the `--no-progress` baseline
/// over a plain pipe. The stdout comparison is BYTE-EXACT.
#[test]
fn dx_progress_id_stdout_byte_identical_keystone() {
    // Baseline (--no-progress, plain pipes) always runs.
    let (cache, tree, _file_count, _keep) = prepare_tree();
    let tree_str = tree.to_string_lossy().into_owned();

    let baseline = run_ok(cache.path(), &["id", "--no-progress", &tree_str]);
    let baseline_stdout = baseline.stdout.clone();
    let baseline_id = stdout_str(&baseline);
    assert_is_id(&baseline_id, "id --no-progress");

    // Only the committed sandbox tree has the known frozen id; pin it there.
    if sandbox_tree().is_some() {
        assert_eq!(
            baseline_id, SANDBOX_ID,
            "sandbox id must equal the frozen keystone id"
        );
    }

    // Plain (piped) progress-ON run: stdout must still be byte-identical.
    let plain_on = run_ok(cache.path(), &["id", &tree_str]);
    assert_eq!(
        plain_on.stdout, baseline_stdout,
        "id stdout must be byte-identical with vs without --no-progress (piped)"
    );

    // Progress-ON under a pty (stderr is a real terminal => the live line
    // renders): stdout (a normal pipe) must STILL be byte-identical to the
    // baseline — progress is stderr-only and must never touch stdout.
    if !pty_enabled() {
        eprintln!(
            "dx_progress_id_stdout_byte_identical_keystone: pty leg SKIPPED \
             (set SNAPDIR_PTY_TEST=1); piped byte-identity leg ran."
        );
        return;
    }
    let (_pty, out) = match run_under_pty(cache.path(), &["id", &tree_str]) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("dx_progress_id_stdout_byte_identical_keystone: pty leg SKIP ({reason})");
            return;
        }
    };
    assert!(out.status.success(), "id under pty must succeed");
    assert_eq!(
        out.stdout, baseline_stdout,
        "id stdout under a TTY stderr must be byte-identical to the --no-progress baseline"
    );
    assert!(
        !has_ansi(&out.stdout) && !has_cr(&out.stdout),
        "stdout must never carry a progress fragment (ANSI/CR) even while the pty renders"
    );
}

/// SPEC clause 5 (KEYSTONE, manifest): `manifest <tree>` STDOUT is byte-identical
/// with progress ON vs `--no-progress` (piped), and under a pty stderr stays
/// byte-identical and ANSI/CR-free. The manifest is large, so a leaked progress
/// fragment would be obvious.
#[test]
fn dx_progress_manifest_stdout_byte_identical_keystone() {
    let (cache, tree, _file_count, _keep) = prepare_tree();
    let tree_str = tree.to_string_lossy().into_owned();

    let baseline = run_ok(cache.path(), &["manifest", "--no-progress", &tree_str]);
    let baseline_stdout = baseline.stdout.clone();
    assert!(
        !baseline_stdout.is_empty(),
        "manifest --no-progress must print a manifest"
    );

    let plain_on = run_ok(cache.path(), &["manifest", &tree_str]);
    assert_eq!(
        plain_on.stdout, baseline_stdout,
        "manifest stdout must be byte-identical with vs without --no-progress (piped)"
    );

    if !pty_enabled() {
        eprintln!(
            "dx_progress_manifest_stdout_byte_identical_keystone: pty leg SKIPPED \
             (set SNAPDIR_PTY_TEST=1); piped byte-identity leg ran."
        );
        return;
    }
    let (_pty, out) = match run_under_pty(cache.path(), &["manifest", &tree_str]) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!(
                "dx_progress_manifest_stdout_byte_identical_keystone: pty leg SKIP ({reason})"
            );
            return;
        }
    };
    assert!(out.status.success(), "manifest under pty must succeed");
    assert_eq!(
        out.stdout, baseline_stdout,
        "manifest stdout under a TTY stderr must be byte-identical to the --no-progress baseline"
    );
    assert!(
        !has_ansi(&out.stdout) && !has_cr(&out.stdout),
        "manifest stdout must never carry a progress fragment even while the pty renders"
    );
}

// ===========================================================================
// Clause 6 — EMPTY / ZERO-FILE DIR: no panic, no divide-by-zero.
// ===========================================================================

/// SPEC clause 6: `id <empty-dir>` and a dir of only empty files complete
/// cleanly (exit 0, valid id) WITH progress on — no panic from a 0-total
/// fraction. The piped leg always runs (exit + id check); the pty leg (progress
/// actually rendering over a 0/near-0 total) runs when enabled.
#[test]
fn dx_progress_empty_dir_no_divide_by_zero() {
    let cache = TempDir::new().unwrap();

    // (a) Truly empty directory.
    let empty = TempDir::new().unwrap();
    let empty_str = empty.path().to_string_lossy().into_owned();
    let out = run_ok(cache.path(), &["id", &empty_str]);
    let id = stdout_str(&out);
    assert_is_id(&id, "id empty-dir");

    // (b) Directory of ONLY empty (0-byte) files — zero total bytes, nonzero
    // file count: another way to hit a 0-total fraction in a broken renderer.
    let zeros = TempDir::new().unwrap();
    zeros.child("a").write_str("").unwrap();
    zeros.child("b").write_str("").unwrap();
    zeros.child("sub/c").write_str("").unwrap();
    let zeros_str = zeros.path().to_string_lossy().into_owned();
    let zout = run_ok(cache.path(), &["id", &zeros_str]);
    assert_is_id(&stdout_str(&zout), "id zero-byte-files dir");

    if !pty_enabled() {
        eprintln!(
            "dx_progress_empty_dir_no_divide_by_zero: pty leg SKIPPED (set SNAPDIR_PTY_TEST=1); \
             piped exit/id legs ran."
        );
        return;
    }
    // With progress actively rendering, the empty/zero cases must still exit 0
    // (no panic, no divide-by-zero) and keep stdout id-clean.
    for dir in [&empty_str, &zeros_str] {
        let (pty, out) = match run_under_pty(cache.path(), &["id", dir]) {
            Ok(v) => v,
            Err(reason) => {
                eprintln!("dx_progress_empty_dir_no_divide_by_zero: pty leg SKIP ({reason})");
                return;
            }
        };
        assert!(
            out.status.success(),
            "id {dir} under pty must exit 0 (no panic/divide-by-zero); stderr(pty): {}",
            String::from_utf8_lossy(&pty)
        );
        // A panic would print 'panicked at' to stderr; forbid it.
        let stderr = String::from_utf8_lossy(&pty).to_lowercase();
        assert!(
            !stderr.contains("panic"),
            "id {dir} under pty must not panic; stderr(pty): {stderr}"
        );
        assert_is_id(
            &String::from_utf8_lossy(&out.stdout).trim_end().to_owned(),
            &format!("id {dir} under pty"),
        );
    }
}

// ===========================================================================
// Clause 7 — STDOUT CLEANLINESS (stdout pipe, stderr pty).
// ===========================================================================

/// SPEC clause 7: when stdout is a pipe (redirected) but stderr is the pty, the
/// stdout of `id`/`manifest` contains EXACTLY the id/manifest bytes — never a
/// progress fragment (no ANSI, no CR). This is the precise "progress is
/// stderr-only" contract under a live render. The harness already keeps stdout
/// on a normal pipe while stderr is the pty, so this is a direct check.
#[test]
fn dx_progress_stdout_clean_under_live_render() {
    if skip_unless_pty("dx_progress_stdout_clean_under_live_render") {
        return;
    }
    let (cache, tree, _file_count, _keep) = prepare_tree();
    let tree_str = tree.to_string_lossy().into_owned();

    // id: stdout must be exactly the bare id + single newline, ANSI/CR-free.
    let (pty, out) = match run_under_pty(cache.path(), &["id", &tree_str]) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("dx_progress_stdout_clean_under_live_render: SKIP ({reason})");
            return;
        }
    };
    assert!(out.status.success(), "id under pty must succeed");
    let id = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
    assert_is_id(&id, "id stdout under pty");
    assert_eq!(
        out.stdout,
        format!("{id}\n").into_bytes(),
        "id stdout under a live render must be EXACTLY the id + one newline (no progress fragment)"
    );
    assert!(
        !has_ansi(&out.stdout) && !has_cr(&out.stdout),
        "id stdout must carry no ANSI/CR even though stderr(pty) does ({} pty bytes)",
        pty.len()
    );

    // manifest: stdout must be exactly the manifest bytes, ANSI/CR-free, and
    // byte-identical to the piped --no-progress manifest.
    let baseline = run_ok(cache.path(), &["manifest", "--no-progress", &tree_str]);
    let (_pty2, mout) = match run_under_pty(cache.path(), &["manifest", &tree_str]) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("dx_progress_stdout_clean_under_live_render (manifest): SKIP ({reason})");
            return;
        }
    };
    assert!(mout.status.success(), "manifest under pty must succeed");
    assert!(
        !has_ansi(&mout.stdout) && !has_cr(&mout.stdout),
        "manifest stdout must carry no ANSI/CR under a live render"
    );
    assert_eq!(
        mout.stdout, baseline.stdout,
        "manifest stdout under a live render must equal the --no-progress manifest byte-for-byte"
    );
}
