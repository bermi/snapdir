# cli handoff for transfer-verbose-reports-jobs @ 2026-06-04T03:06:39Z

## Summary
Added field observability for the transfer commands. New helper
`Cli::log_transfer_config(&self)` in `crates/snapdir-cli/src/cli.rs`: when (and
only when) `self.globals.verbose` is set, it resolves `transfer_config()?` and
prints exactly ONE line to **stderr** (stdout is never touched, so the
id-on-stdout contract stays byte-stable). If the config can't resolve it skips
the diagnostic rather than aborting (a bad `--limit-rate` still surfaces from the
command body).

Exact stderr line format:
- no limit:   `transfers: <N> concurrent`
- with limit: `transfers: <N> concurrent, limit <RATE>`

where `<N>` = `transfer_config()?.concurrency.get()` (the EFFECTIVE concurrency,
auto-resolved when `--jobs` is unset/0) and `<RATE>` = the user's original
`--limit-rate` flag string (raw, faithful rendering).

Wiring (one line per top-level command invocation):
- `run_push` and `run_stage` call `log_transfer_config()` at the top.
- `run_fetch` / `run_checkout` are now thin wrappers that log once, then call
  new private `fetch_inner` / `checkout_inner` (the original bodies, unchanged).
- `run_pull` logs ONCE itself, then calls `fetch_inner` + `checkout_inner`
  directly, so `pull --verbose` emits exactly one banner, not one per leg.

Non-verbose runs emit nothing new; non-verbose stdout/stderr are unchanged.
Printing also happens under `--dryrun` (informational, stderr) — consistent.

## Files changed
```
 crates/snapdir-cli/src/cli.rs   |  48 ++++++++++++-
 crates/snapdir-cli/tests/e2e.rs | 156 ++++++++++++++++++++++++++++++++++++++++
 2 files changed, 202 insertions(+), 2 deletions(-)
```
Tests added to `crates/snapdir-cli/tests/e2e.rs`:
`verbose_jobs_push_reports_concurrency`, `verbose_jobs_limit_rate_reported`,
`verbose_jobs_silent_without_verbose`, `verbose_jobs_pull_reports_once` (asserts
the banner appears exactly once for pull).

## Local verification result
```
cargo test -p snapdir-cli --locked verbose_jobs -- --nocapture
running 4 tests
test verbose_jobs_limit_rate_reported ... ok
test verbose_jobs_push_reports_concurrency ... ok
test verbose_jobs_silent_without_verbose ... ok
test verbose_jobs_pull_reports_once ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 16 filtered out

cargo test -p snapdir-cli --locked   # full suite, all green incl. cli_surface trycmd,
                                     # transfer_concurrency, dryrun, e2e, store_roundtrip
cli_surface_snapshots ... ok         # 22 trycmd snapshots unchanged (stdout byte-stable)
transfer_concurrency_jobs1_roundtrip ... ok  (20 passed in that bin)

cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)   # 0 warnings

cargo fmt -p snapdir-cli   # applied (reformatted new test arg lists); clean
```

## Reuse check / Blockers
- No stores/core/catalog edits — lane stayed inside `crates/snapdir-cli/` only.
- Reused the existing `transfer_config()` (the single source of truth for
  effective concurrency + rate); no business logic added to the CLI.
- stderr-only; stdout stays byte-stable (trycmd cli_surface snapshots unchanged).
- clippy + fmt clean. No cross-lane needs. No blockers.

Ready for PM verification: YES
