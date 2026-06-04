# cli handoff for transfer-concurrency-verification @ 2026-06-04T03:00:02Z

## Summary
Added end-to-end, CLI-boundary verification that `--jobs` / `--limit-rate` are
wired through the binary and that concurrency does not change the materialized
result. All new tests are TEST-ONLY — no production-code changes. Fn names start
with `transfer_concurrency` so `cargo test -p snapdir-cli --locked
transfer_concurrency` selects exactly them.

New tests in `crates/snapdir-cli/tests/e2e.rs` (all hermetic via a temp `file://`
store + temp cache, removed on drop; a new `build_multi_tree` helper builds an
8-file tree with nested dirs and pinned file/dir permissions so the concurrent
path has real fan-out and perms are part of the id):

- `transfer_concurrency_jobs4_roundtrip` — `push --jobs 4` a multi-file tree to a
  `file://` store, then `pull --jobs 4` into a fresh dest: exit 0, pushed id ==
  source id, pulled dest re-manifests to the same id (byte-identical through the
  concurrent FileStore path).
- `transfer_concurrency_jobs1_roundtrip` — same with `--jobs 1` (sequential path):
  identical id/result, and additionally pushes the same tree to a second store
  with `--jobs 4` and asserts the printed ids match, proving concurrency does not
  change the snapshot id.
- `transfer_concurrency_limit_rate_accepted` — `push --jobs 2 --limit-rate 1M` +
  `pull --limit-rate 512K` round-trip succeeds and re-manifests to the same id
  (proves the flag parses + threads into `TransferConfig` + doesn't break
  correctness). NO timing assertion — FileStore does not throttle local copies.

### Hermetic vs MinIO-gated
- All shipped tests are HERMETIC (`file://`, no network/credentials), so CI does
  not depend on any external endpoint.
- The optional `transfer_concurrency_limit_rate_throttle_minio` (real
  throughput/throttle test) was deliberately NOT added: the aggregate
  `RateLimiter` is network-only, FileStore does not throttle, and the
  deterministic throttle proof already lives in transfer-config's `RateLimiter`
  timing unit test. A MinIO/S3-gated throughput test would be the only way to
  observe real throttling end-to-end; flagged as an optional future add (skip
  cleanly when the env var is unset) rather than introducing an env-gated test
  with no current CI signal.

### Verbose / effective-concurrency
The CLI does NOT currently emit effective concurrency / jobs on `--verbose`
(verbose output is limited to `CACHED:` / `SAVED:` lines in fetch and a few purge
notices; `grep` for job/concur/rate/transfer in eprintln/println paths returns
nothing). Per the test-only scope, `transfer_concurrency_verbose_reports_jobs`
was SKIPPED — adding production code to emit it is out of scope for this gate.
**Possible follow-up:** have `push`/`pull`/`fetch` print the effective concurrency
(and limit-rate) under `--verbose` for operator observability.

## Files changed
```
 crates/snapdir-cli/tests/e2e.rs | 190 ++++++++++++++++++++++++++++++++++++++++
 1 file changed, 190 insertions(+)
```
(tests only; no production code, no out-of-lane edits)

## Local verification result
```
cargo test -p snapdir-cli --locked transfer_concurrency:
running 3 tests
test transfer_concurrency_limit_rate_accepted ... ok
test transfer_concurrency_jobs4_roundtrip ... ok
test transfer_concurrency_jobs1_roundtrip ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 13 filtered out

cargo test -p snapdir-cli --locked (full):
all suites ok — 0 failed across e2e/cli/dryrun/manifest/store_roundtrip/etc.

cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings:
    Finished `dev` profile [unoptimized + debuginfo] target(s)  (clean)

cargo fmt -p snapdir-cli -- --check:
FMT CLEAN
```

## Reuse check / Blockers
- No production-code edits — feature already landed (`transfer-config`,
  `cli-transfer-flags`, `concurrent-upload/download`, `filestore-parallel`); this
  gate only verifies the wiring at the binary boundary.
- `--jobs` correctness proven hermetically via `file://` (round-trip ids match;
  `--jobs 4` == `--jobs 1` snapshot id).
- `--limit-rate` throttle is network-only (deterministically unit-proven in
  transfer-config's `RateLimiter` test); e2e timing would require MinIO/S3, so it
  is intentionally not in the hermetic suite (no CI dependency introduced).
- Verbose-jobs: NOT emitted by the CLI today → verbose test skipped, noted as a
  possible follow-up (no production code added).
- clippy + fmt clean; no out-of-lane changes (only `crates/snapdir-cli/tests/`).
- No real bug found during verification.

Ready for PM verification: YES
