# cli handoff for cli-sync-command @ 2026-06-04T17:35:31Z

## Summary
Added the 15th subcommand, `snapdir sync --id <id> --from <store> --to <store>`,
a direct store→store copy of a snapshot (manifest + every referenced object)
that streams through memory with no local staging.

- **Command enum** (`crates/snapdir-cli/src/cli.rs`, after `Defaults`): new
  `Sync { from: String, to: String }` (both `#[arg(long, value_name = "STORE")]`).
  `--id` comes from the existing GLOBAL `--id` (via `self.require_id()`), not a
  sync-local arg.
- **Dispatch**: `Command::Sync { from, to } => self.run_sync(from, to),`.
- **`stream_store_for_adapter(adapter, url, config) -> Result<Box<dyn StreamStore + Sync>>`**
  — new free fn mirroring `store_for_adapter`'s endpoint/region env handling
  (`SNAPDIR_S3_STORE_ENDPOINT_URL`, `SNAPDIR_B2_REGION`/`AWS_REGION`) for the
  in-process stores only (File/S3/B2/Gcs). `Adapter::External` is rejected with
  `sync requires in-process stores (file/s3/b2/gcs); external `snapdir-*-store`
  URLs are not supported: {url}`. Imported `snapdir_stores::StreamStore`.
- **`run_sync(&self, from_url, to_url) -> Result<()>`**: `require_id()`; rejects
  `from_url == to_url` (`sync --from and --to must differ`); resolves each url
  via `resolve_adapter` + `stream_store_for_adapter`; passes the SAME
  `TransferConfig` (cloned) to both stores and to `sync_snapshot`; calls
  `self.log_transfer_config()` (verbose banner); calls
  `snapdir_stores::sync_snapshot(&*from, &*to, id, &config, self.globals.dryrun)`.

### Output convention
- **Real sync**: prints `{id}` to STDOUT (scriptable id-on-stdout, matches
  push/stage) + STDERR summary
  `synced {id}: {copied} copied, {skipped} skipped ({bytes} bytes)`.
- **--dryrun**: NO stdout; STDERR `dry-run: would copy {N} object(s) for {id}`.
  `sync_snapshot` performs no writes when `dry_run=true`.

### trycmd snapshots changed
- `tests/cmd/help.trycmd`: ONE added line — the new `sync` subcommand listing.
- `tests/cmd/help-sync.trycmd`: NEW per-subcommand help page (matches the
  existing `help-<cmd>.trycmd` convention). No other surface snapshots changed.
  (`--from`/`--to` help text wrapped the store-URI example in backticks to match
  the existing `--store` help and satisfy `clippy::doc_markdown`.)

## Files changed
```
 crates/snapdir-cli/src/cli.rs            | 111 ++++++++++++++++++++++++++++++-
 crates/snapdir-cli/tests/cmd/help.trycmd |   1 +
 2 files changed, 111 insertions(+), 1 deletion(-)
```
Untracked (new):
```
?? crates/snapdir-cli/tests/cmd/help-sync.trycmd
?? crates/snapdir-cli/tests/sync_command.rs
```

## Local verification result
```
$ grep -q 'Sync' crates/snapdir-cli/src/cli.rs && echo OK
OK

$ cargo test -p snapdir-cli --locked sync_cmd
running 5 tests
test sync_cmd_rejects_external ... ok
test sync_cmd_requires_id ... ok
test sync_cmd_rejects_same_from_to ... ok
test sync_cmd_dryrun_writes_nothing ... ok
test sync_cmd_mirrors_between_file_stores ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p snapdir-cli --locked   # FULL — all test binaries
... all green (cli_surface incl. regenerated help/help-sync, e2e, store_roundtrip,
    sync_command, manifest, dryrun, catalog, cache, etc.) — 0 failed.

$ cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)   # clean

$ cargo fmt -p snapdir-cli   # applied, no drift
```

Tests added (all `sync_cmd*` in `tests/sync_command.rs`):
- `sync_cmd_rejects_external` — `--from rsync://...` → non-zero + "sync requires
  in-process stores" / "not supported".
- `sync_cmd_rejects_same_from_to` — same `file://` url both sides → error.
- `sync_cmd_requires_id` — no `--id` → "missing --id option".
- `sync_cmd_mirrors_between_file_stores` (e2e) — push→A, sync A→B, pull from B
  re-materializes byte-identically and `id` == source id; 2nd sync reports
  `0 copied` (skip-present).
- `sync_cmd_dryrun_writes_nothing` — `--dryrun` prints no stdout id, STDERR
  "dry-run: would copy", destination store stays empty.

## Reuse check / Blockers
- No stores/core/catalog edits — lane is strictly `crates/snapdir-cli/` (the only
  out-of-lane untracked file, `.claude/ralph-loop.local.md`, is unrelated harness
  state, not mine).
- Reused `snapdir_stores::{StreamStore, sync_snapshot, SyncReport,
  resolve_adapter, Adapter}` and `Cli::{transfer_config, log_transfer_config,
  require_id}` — no business logic added in the CLI.
- External and same-url rejected; `--dryrun` honored (no writes).
- trycmd regenerated: only the `sync` subcommand line added + the new
  `help-sync.trycmd`; re-run without overwrite is green.
- clippy clean; fmt applied.

Ready for PM verification: YES
