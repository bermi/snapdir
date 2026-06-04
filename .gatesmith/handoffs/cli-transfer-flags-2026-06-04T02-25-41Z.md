# cli handoff for cli-transfer-flags @ 2026-06-04T02:25:41Z

## Summary

Added the transfer-tuning CLI surface and threaded a `snapdir_stores::TransferConfig`
to every store. All changes confined to `crates/snapdir-cli/`.

- **Two new global flags** on `GlobalArgs` (cli.rs):
  - `--jobs` / `-j <N>` — `Option<usize>`, `env = "SNAPDIR_JOBS"`. Unset or `0` => auto.
  - `--limit-rate <RATE>` — `Option<String>` (parsed later), `env = "SNAPDIR_LIMIT_RATE"`.
  - Confirmed `-j` collides with no existing short flag.
- **`parse_rate(&str) -> Result<u64>`** — bare integer = bytes; suffixes K/M/G with
  optional `i`/`B` (case-insensitive): K=1024, M=1024², G=1024³. Fractions supported
  (`1.5M`). Whitespace trimmed. Empty / unknown-unit / non-numeric / negative => clear Err.
- **`Cli::transfer_config(&self) -> Result<TransferConfig>`** — concurrency = `--jobs`
  when `Some(n>0)`, else `TransferConfig::default().concurrency` (auto). max_bytes_per_sec
  = `parse_rate(--limit-rate)` or `None`. Built via `TransferConfig::new`.
- **Threaded to every store**: `store_for_adapter` gained a `config: TransferConfig`
  param and now calls `FileStore::new_with_config`, `S3Store::connect_with`,
  `B2Store::connect_with`, `GcsStore::connect_with`. `resolve_store` builds the config
  via the helper and passes it. `cache_store` now uses `FileStore::from_root_with_config`
  (so cache copies honor `--jobs`); it returns `Result` and its 4 call sites add `?`.
  External shim store is unchanged (no transfer config).
- Additive / backward-compatible: with neither flag, concurrency is the stores' auto
  default and bandwidth is unlimited — identical to prior behavior.

No edits to snapdir-stores / snapdir-core / snapdir-catalog; only their public APIs called.

### trycmd snapshots regenerated

Regenerated via `TRYCMD=overwrite cargo test -p snapdir-cli --locked --test cli_surface`.
15 `.trycmd` files changed — every help snapshot that renders the shared global-options
block (the globals are `global = true`, so the block is propagated to each subcommand's
`--help`): help.trycmd plus help-{ancestors,checkout,defaults,fetch,flush-cache,id,
locations,manifest,pull,push,revisions,stage,verify-cache,verify}.trycmd. The ONLY change
in each is the two new option lines appended to the Options block:

    -j, --jobs <N>              Max concurrent object transfers (0/auto = number of CPUs, capped) [env: SNAPDIR_JOBS=]
        --limit-rate <RATE>     Limit total transfer bandwidth, e.g. 10M, 512K, 1G (wget-style; aggregate across all transfers) [env: SNAPDIR_LIMIT_RATE=]

Confirmed: `git diff` of the cmd/ dir adds exactly those 2 lines × 15 files, nothing else.
Re-ran without overwrite — green.

## Files changed

```
 crates/snapdir-cli/src/cli.rs                      | 221 +++++++++++++++++++--
 crates/snapdir-cli/tests/cmd/help-ancestors.trycmd |   2 +
 crates/snapdir-cli/tests/cmd/help-checkout.trycmd  |   2 +
 crates/snapdir-cli/tests/cmd/help-defaults.trycmd  |   2 +
 crates/snapdir-cli/tests/cmd/help-fetch.trycmd     |   2 +
 crates/snapdir-cli/tests/cmd/help-flush-cache.trycmd |   2 +
 crates/snapdir-cli/tests/cmd/help-id.trycmd        |   2 +
 crates/snapdir-cli/tests/cmd/help-locations.trycmd |   2 +
 crates/snapdir-cli/tests/cmd/help-manifest.trycmd  |   2 +
 crates/snapdir-cli/tests/cmd/help-pull.trycmd      |   2 +
 crates/snapdir-cli/tests/cmd/help-push.trycmd      |   2 +
 crates/snapdir-cli/tests/cmd/help-revisions.trycmd |   2 +
 crates/snapdir-cli/tests/cmd/help-stage.trycmd     |   2 +
 crates/snapdir-cli/tests/cmd/help-verify-cache.trycmd |   2 +
 crates/snapdir-cli/tests/cmd/help-verify.trycmd    |   2 +
 crates/snapdir-cli/tests/cmd/help.trycmd           |   2 +
 crates/snapdir-cli/tests/e2e.rs                    |  60 ++++++
 17 files changed, 295 insertions(+), 16 deletions(-)
```

## Local verification result

`cargo test -p snapdir-cli --locked transfer_flags` (last lines):

```
     Running tests/e2e.rs (target/debug/deps/e2e-aa4865dd668d7236)
running 1 test
test transfer_flags_push_pull_roundtrip ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out
```

Unit tests (src/main.rs): 7 passed —
transfer_flags_parse_rate, transfer_flags_jobs_explicit,
transfer_flags_jobs_one_is_sequential, transfer_flags_jobs_unset_is_auto,
transfer_flags_jobs_zero_is_auto, transfer_flags_limit_rate_threads_into_config,
transfer_flags_bad_limit_rate_errors. Plus the e2e push/pull roundtrip
(`--jobs 2 --limit-rate 1M` push, `-j 1 --limit-rate 512K` pull).

FULL suite `cargo test -p snapdir-cli --locked`: all green (cli_surface trycmd: 1 passed;
e2e: 13 passed; no regressions).

`cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings`: clean.
`cargo fmt -p snapdir-cli`: clean.
`grep -q 'limit.rate\|limit_rate' crates/snapdir-cli/src/cli.rs`: matches (GREP OK).

## Reuse check / Blockers

- No edits to snapdir-stores / snapdir-core / snapdir-catalog — only public APIs called
  (`TransferConfig::{new,default}`, `*_with_config` / `connect_with` ctors).
- Rate parsing + config construction are thin CLI helpers; the concurrency/rate-limit
  business logic lives in `snapdir-stores::transfer`.
- trycmd snapshots regenerated; only the two new global flag lines added (verified).
- clippy + fmt clean. Lane boundary respected (diff is `crates/snapdir-cli/` only).
- No commits made (PM commits). No blockers.

Ready for PM verification: YES
