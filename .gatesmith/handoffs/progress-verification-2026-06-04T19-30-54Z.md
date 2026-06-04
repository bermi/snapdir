# cli handoff for progress-verification @ 2026-06-04T19:30:54Z

## Summary

Added `crates/snapdir-cli/tests/progress_e2e.rs` — a TEST-ONLY suite proving the
live progress dashboard's *scriptable contract* across every transfer command.
No production code was changed; the feature was already implemented (single-line
stderr indicator that auto-activates only on a TTY; flags
`--no-progress`/`--quiet`/`--color`; `libc` already a dep). No bug found.

Helpers per spec: `has_ansi(&[u8])` (contains `0x1b`) and `has_cr(&[u8])`
(contains `\r`), plus an `assert_clean` wrapper and an `assert_is_id` helper.

Required `progress_e2e*` tests (all pass):
- `progress_e2e_push_scriptable` — `push --store file://A`: stdout is EXACTLY
  the id + one trailing newline; no ANSI/CR on either stream.
- `progress_e2e_pull_scriptable` — push to A, then `pull --store file://A --id
  <id> <dest>`: exit 0, no ANSI/CR, dest re-materializes (contents) and
  re-manifests to the id. (pull was uncovered by `progress_wire`.)
- `progress_e2e_sync_scriptable` — stage into A via push, `sync --id --from
  file://A --to file://B`: stdout id-only, no ANSI/CR, and B is proven to serve
  the snapshot (a fresh-cache pull from B re-manifests to the id).
- `progress_e2e_flags_matrix` — `--no-progress`, `--quiet`, `--color never` each
  succeed with id-only stdout and no ANSI for push AND sync; `--verbose --quiet`
  drops the `transfers:` banner.
- `progress_e2e_id_identical_with_without_flags` — id is byte-identical across
  plain, `--no-progress`, and `--quiet`.

OPTIONAL pty smoke `progress_e2e_pty_renders_when_tty`: REAL, env-gated on
`SNAPDIR_PTY_TEST=1`. Unset → prints a skip note and returns Ok (never fails
CI). Set → opens a pty via `libc::openpty`, spawns `snapdir sync` with stderr
`dup2`'d onto the pty slave (so the child sees `stderr().is_terminal()==true`)
and `TERM=xterm`, keeps stdout on a normal pipe, drains the master with a
bounded non-blocking loop (20s deadline, EIO/EOF handled), and asserts the pty
captured ANSI/CR redraw bytes while stdout stayed exactly the id. Any pty setup
failure skips-with-note (returns Err → eprintln skip) rather than hanging or
failing. Verified it actually renders (passes with the env var set) and is
non-flaky across 5+ consecutive runs. No new dependency: uses `libc` (already a
dep) — no portable-pty/rexpect, no new crate. The deterministic render proof
remains the `cli-progress-renderer` golden tests in `src/progress.rs`.

## Files changed

```
 crates/snapdir-cli/tests/progress_e2e.rs | 532 +++++++++++++++++++++
 1 file changed, 532 insertions(+)
```
(new file, untracked; crates/snapdir-cli/ only — tests only.)

## Local verification result

`cargo test -p snapdir-cli --locked progress_e2e -- --nocapture` (last lines):
```
test progress_e2e_push_scriptable ... ok
test progress_e2e_pull_scriptable ... ok
test progress_e2e_pty_renders_when_tty ... ok   (env unset => SKIP note, Ok)
test progress_e2e_id_identical_with_without_flags ... ok
test progress_e2e_sync_scriptable ... ok
test progress_e2e_flags_matrix ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Live pty smoke with the gate set:
```
SNAPDIR_PTY_TEST=1 cargo test -p snapdir-cli --locked progress_e2e_pty
test progress_e2e_pty_renders_when_tty ... ok   (renders: ANSI/CR on pty stderr,
                                                 stdout still id-only)
```

Full suite (`cargo test -p snapdir-cli --locked`): all 16 test binaries report
`ok`, 0 failures — no regressions.

`cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings`:
Finished, no warnings. (One `#[allow(clippy::borrow_as_ptr, clippy::too_many_lines)]`
on the test-only `run_pty_smoke` fn — `&mut master/slave` are the required
`*mut c_int` out-params for `libc::openpty` and `&raw mut` is unavailable on the
pinned MSRV 1.78.)

`cargo fmt -p snapdir-cli -- --check`: fmt clean.

## Reuse check / Blockers

- No production edits — TEST-ONLY; the progress feature was already implemented.
- No new dependency — pty smoke uses `libc` (already declared in Cargo.toml).
- Scriptable contract proven across push / pull / sync (id-only stdout, no
  ANSI/CR, `--quiet` suppresses the verbose banner, id flag-independent).
- pty smoke is env-gated, real (verified it renders), and non-flaky; skips-with-
  note by default so CI never depends on it.
- Lane respected: only `crates/snapdir-cli/tests/` touched. No bug to report.

Ready for PM verification: YES
