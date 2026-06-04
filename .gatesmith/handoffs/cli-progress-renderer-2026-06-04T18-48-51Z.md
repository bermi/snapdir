# cli handoff for cli-progress-renderer @ 2026-06-04T18:48:51Z

## Summary

Built the hand-rolled terminal progress rendering **engine** as a new
self-contained module `crates/snapdir-cli/src/progress.rs` (declared
`#[allow(dead_code)] mod progress;` in `main.rs`). It is NOT wired into any
command and adds no `--no-progress`/`--quiet`/`--color` flags — that's the next
gate. The module consumes `snapdir_core::{Meter, MeterSnapshot, Phase}` only.

PURE half (no TTY/IO, unit-/golden-tested):
- `ColorChoice {Auto,Always,Never}` (+ `parse`), `should_render(is_tty, no_progress, term)`
  (takes `is_tty` as a param so it's pure-testable; real caller passes
  `stderr().is_terminal()`), `use_color(choice, is_tty, no_color_env)`, and a
  `Style { color }` with hand-rolled ANSI (`dim/bold/cyan/green`, raw `\x1b[..m`).
- Humanizers `human_bytes` (base-1024) / `human_rate` / `human_eta`.
- `format_line(snap, &RenderMetrics, width, &Style, ascii)` (+ `format_line_named`
  with an optional command label) — the pure single-line formatter. MODERN braille
  spinner `⠋⠙…`, bar `█/░` inside `▕…▏`, arrows `↓ ↑`; FALLBACK ascii `|/-\`,
  `[#### ]`, `down/up`. Indeterminate (total==0) → spinner + counts, no bar/percent.
  Fits to `width` on a `char` count, dropping optionals in priority order
  (eta → cpu → mem → obj/s), then shrinking the bar, then ellipsis-truncating.
  ANSI escapes are width-neutral. The fit/assembly is split into a `LineFields`
  helper to keep each fn small.

IO half (thin libc wrappers, graceful):
- `term_width()` via `ioctl(STDERR_FILENO, TIOCGWINSZ)` → `COLUMNS` env → None.
- `sample_rss()`: Linux `/proc/self/statm` × `_SC_PAGESIZE`; macOS mach
  `task_info(MACH_TASK_BASIC_INFO).resident_size`. None on any error.
- `CpuSampler::poll()` via `getrusage(RUSAGE_SELF)` (ru_utime+ru_stime) deltas over
  wall-clock, normalized by `available_parallelism`; first poll primes the baseline.
- `ProgressReporter::start(meter, jobs, active, color, ascii)` spawns a ~100ms render
  thread (EWMA α=0.3 rates, width/rss/cpu sampling, `\r{line}\x1b[K` to a locked
  stderr; NEVER stdout) when `active`, else inert; `finish()` stops+joins+clears the
  line, no-op when inactive.

## Files changed

```
 Cargo.lock                     | 1 +     (libc edge only — no new package)
 crates/snapdir-cli/Cargo.toml  | 4 ++++  (libc = "0.2")
 crates/snapdir-cli/src/main.rs | 5 +++++ (mod progress;)
 crates/snapdir-cli/src/progress.rs | (new, ~880 lines incl. tests)
```

## Local verification result

```
$ cargo test -p snapdir-cli --locked progress_render -- --nocapture
test progress::tests::progress_render_term_width_no_panic ... ok
test progress::tests::progress_render_should_render_logic ... ok
test progress::tests::progress_render_humanizers ... ok
test progress::tests::progress_render_reporter_inactive_is_inert ... ok
test progress::tests::progress_render_format_line_fallback ... ok
test progress::tests::progress_render_format_line_indeterminate ... ok
test progress::tests::progress_render_format_line_modern ... ok
test progress::tests::progress_render_fits_width ... ok
test progress::tests::progress_render_metrics_best_effort ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 11 filtered out

$ cargo test -p snapdir-cli --locked          # full suite, no regressions
all targets: ok (bin 20 + integration suites all pass; 0 failed)

$ cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings
    Finished `dev` profile ... (0 warnings)

$ cargo fmt -p snapdir-cli --check            # fmt OK
```

## Reuse check / Blockers

- No stores/core/catalog edits — strictly `crates/snapdir-cli/`. Consumes the
  existing `snapdir_core::{Meter, MeterSnapshot, Phase}` (no duplication).
- Only new dependency is `libc = "0.2"`; Cargo.lock gains a single direct edge to
  the already-present `libc 0.2.186` (no new package → cooldown satisfied). No
  indicatif/console/anstyle/anstream/sysinfo/terminal_size/unicode-width pulled.
- All ANSI escapes hand-rolled as raw `\x1b[..m` constants; TTY policy via
  `std::io::IsTerminal` (the caller's job, passed in as `is_tty`).
- `format_line` is PURE and golden-tested (modern + fallback + indeterminate +
  width-fitting). Self-metrics are best-effort: `sample_rss`/`CpuSampler::poll`
  return Some(plausible)-or-None and never panic; the render thread never writes
  to stdout.
- The module is unused-by-commands this gate (carries `#[allow(dead_code)]` so
  clippy stays green); the next gate wires it into run_push/run_sync and adds the
  `--no-progress`/`--quiet`/`--color` flags.

Ready for PM verification: YES
