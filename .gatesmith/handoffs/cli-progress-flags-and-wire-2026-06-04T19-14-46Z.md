# cli handoff for cli-progress-flags-and-wire @ 2026-06-04T19:14:46Z

## Summary
Turned the live progress dashboard ON by adding the control flags and wiring the
`Meter` + `ProgressReporter` (from the existing `progress.rs` engine) into every
transfer command and the walk.

- **Flags** (in `GlobalArgs`, after `debug`, all `global=true`):
  - `--no-progress` (`env="SNAPDIR_NO_PROGRESS"`) — disables the live line only.
  - `--quiet` / `-q` — suppresses BOTH the stderr banners AND the live line;
    wins over `--verbose` for banners.
  - `--color <WHEN>` — `auto|always|never`, parsed via the engine's
    `ColorChoice::parse` (kept the field a plain `String` rather than a clap
    `ValueEnum`: a derived enum forced clap into the multi-line "Possible values"
    help layout, which would have rewritten EVERY option in every `--help`; the
    `String` keeps the help compact so the only trycmd delta is the 3 new lines).
- **`start_progress(jobs)` helper on `Cli`** returns `(Option<Arc<Meter>>,
  ProgressReporter)`: active only when `stderr` is a TTY, not
  `--no-progress`/`--quiet`, and `TERM != dumb` (via `should_render`); colorizes
  via `use_color(color_choice, is_tty, NO_COLOR)`; ascii fallback only when
  `TERM=dumb`. Inactive ⇒ `None` + an inert reporter (no thread).
- **Meter → boxed store:** threaded an `Option<Arc<Meter>>` param into
  `store_for_adapter` / `stream_store_for_adapter` (mirroring how `config`
  already flows), so the meter is set on the CONCRETE store via the
  `with_meter(self, Option<Arc<Meter>>)` builder *before* it is boxed as
  `Box<dyn Store>` / `Box<dyn StreamStore + Sync>`. `resolve_store` and a new
  `cache_store_with_meter` take/forward the same `Option<Arc<Meter>>`. The
  external `snapdir-*-store` shim has no in-process meter hook, so progress is
  simply absent there. `sync` threads the meter straight into
  `sync_snapshot(..., meter.as_deref())` rather than onto the endpoint stores.
- **Walk:** `build_manifest` gained an `Option<&Meter>` param and now calls
  `walk_with_meter`; `walk_with` forwards it. `manifest`/`id` pass `None`;
  `push`/`stage` pass the active meter (set `Phase::Hashing` before the walk,
  then `set_total(total_object_bytes(manifest))` + `Phase::Transfer` before the
  store push). Added a small `total_object_bytes` helper (sum of File entry
  sizes).
- **Per-command wiring:** push/stage (Hashing→Transfer), fetch/checkout/pull
  (Transfer; pull builds ONE reporter spanning both legs — `fetch_inner` /
  `checkout_inner` now take `Option<&Arc<Meter>>`), sync. Every path calls
  `reporter.finish()` BEFORE any stdout write so the id stays clean.
- **`--quiet` banner gating:** `log_transfer_config` early-returns on quiet, and
  every `dry-run:`/`CACHED:`/`SAVED:`/`synced`/`would …` eprintln is guarded by
  `!self.globals.quiet`.

Stdout stays id-only; nothing progress-related is ever written to stdout.

## Files changed
```
 crates/snapdir-cli/src/cli.rs                      | 325 +++++++++++++++++----
 crates/snapdir-cli/src/main.rs                     |   6 +-
 crates/snapdir-cli/src/progress.rs                 |   1 -
 crates/snapdir-cli/tests/cmd/help-*.trycmd (16 files) | 3 + each
 crates/snapdir-cli/tests/progress_wire.rs          | (new)
```
All 16 `help-*.trycmd` snapshots changed by exactly the same 3 added lines
(`--no-progress`, `-q/--quiet`, `--color <WHEN>`); no other surface drift
(verified `git diff | grep '^+[^+]'` matches only those 3 flags). main.rs
dropped the `#[allow(dead_code)] mod progress`; progress.rs dropped one
now-redundant `#[allow(dead_code)]` on `ColorChoice::parse`.

## Local verification result
```
grep -qE 'no.progress|no_progress' crates/snapdir-cli/src/cli.rs  => present
cargo test -p snapdir-cli --locked progress_wire
  running 5 tests
  test progress_wire_no_progress_silent ... ok
  test progress_wire_color_never_no_ansi ... ok
  test progress_wire_piped_stdout_is_id_only ... ok
  test progress_wire_quiet_silent ... ok
  test progress_wire_id_unchanged ... ok
  test result: ok. 5 passed; 0 failed

cargo test -p snapdir-cli --locked  => ALL green (trycmd regenerated + passing
  without overwrite; no regressions to sync/dryrun/e2e/store_roundtrip/
  cache_commands/catalog/list_options/manifest):
  unittests 20 ok; cache_commands 4; catalog_commands 3; catalog_logging 7;
  cli_surface 1; completions_man 4; defaults 6; dryrun 5; e2e 20;
  list_options 7; manifest 7; progress_wire 5; store_roundtrip 4;
  sync_command 5; sync_e2e 5  — all ok, 0 failed.

cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings
  => Finished, no warnings
cargo fmt -p snapdir-cli -- --check => clean
```

## Reuse check / Blockers
- No stores/core edits: reused the existing `progress.rs` engine, the stores'
  `with_meter` builders, and core `walk_with_meter` / `Meter` / `Phase` as-is.
- Stdout id-only preserved byte-for-byte; piped runs emit no ANSI (`\x1b[`) or
  carriage-return redraw on stdout or stderr (asserted by the new tests).
- clippy `-D warnings` + fmt clean. All changes confined to
  `crates/snapdir-cli/`.

Ready for PM verification: YES
