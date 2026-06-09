# cli handoff for external-cli-wiring @ 2026-06-09T21:31:00Z

## Summary

Fixed the CLI ↔ external-store wiring bug: the emit-command contract expects `--staging-dir`/`--cache-dir` to be SHARDED store roots (`.objects/<sharded>` + `.manifests/<sharded>`), but cli.rs was passing TREES, breaking `push/fetch/pull --store <external>://…` end-to-end. Three branches grew adapter-aware external arms (gated on a new `store_is_external()` helper that reuses `resolve_adapter`, the same router `resolve_store` delegates to — no scheme re-encoding):

1. **`run_push` `--id` branch** (path-less push of a staged snapshot): External arm skips the scratch materialization entirely and calls `store.push(&manifest, &self.cache_dir())` — the snapshot is already filed in the cache as a sharded root (manifest-present-implies-objects-present, proven by the preceding `get_manifest`). Native arm keeps the scratch `cache.fetch_files` → `store.push(scratch)` path byte-identically.
2. **`run_push` tree branch**: External arm stages into the local cache first (`cache.push(&manifest, &root)` — the same idempotent, manifest-written-last write `stage` performs, via `cache_store_with_meter`), then `store.push(&manifest, &self.cache_dir())`. The cache root is a valid superset staging dir: the emitted script reads only `.manifests/<sharded id>` and that manifest's objects. Native arm unchanged (`store.push(&manifest, &root)`).
3. **`fetch_inner`**: External arm calls `store.fetch_files(&manifest, &self.cache_dir())` directly (objects land sharded in the cache, no scratch double-copy), then commits the manifest LAST via the cache `FileStore`'s `put_manifest` (`StreamStore` method, already imported) — preserving the manifest-last invariant locally (a failed fetch leaves orphan objects but never a cache manifest). It does NOT route through `cache.push`. Native arm keeps the scratch-tree → `cache.push` path verbatim.

`--dryrun` behavior is unchanged in shape (all three dryrun early-returns sit before the new arms, so dryrun stays write-free); `log_event` calls untouched; the cache root path reuses the existing `cache_dir()` resolver (the same `PathBuf` `cache_store()` builds its `FileStore` from).

New gate test `crates/snapdir-cli/tests/external_store_roundtrip.rs` drives the REAL binary (`env!("CARGO_BIN_EXE_snapdir")`, pattern from store_roundtrip.rs) against `mock://<tempdir>` with the canonicalized `crates/snapdir-stores/tests/` fixture dir prepended to PATH and isolated `SNAPDIR_CACHE_DIR` temp dirs per logical client:
- (a) push of a multi-file tree (subdir + a file with spaces, explicit perms) → exit 0, id on stdout, mock store dir contains the sharded manifest + all 3 objects byte-equal;
- (c) second push → exit 0, same id (idempotent no-op);
- (b) fetch with a FRESH cache + checkout → byte-identical tree that re-manifests to the same id;
- (e) the fresh cache holds the sharded manifest + sharded objects after the external fetch (proves the put_manifest-last arm ran);
- (d) fetch of an unknown 64-hex id → non-zero exit, stderr contains "not found", no cache manifest committed.

## Files changed

```
 crates/snapdir-cli/src/cli.rs | 118 +++++++++++++++++++++++++++++++++++-------
 1 file changed, 98 insertions(+), 20 deletions(-)
?? crates/snapdir-cli/tests/external_store_roundtrip.rs (new, 273 lines)
```

## Local verification result

1. `cargo test -p snapdir-cli --test external_store_roundtrip --locked`:

```
running 3 tests
test external_fetch_unknown_id_fails_with_not_found ... ok
test external_push_lands_sharded_manifest_and_objects ... ok
test external_fetch_fresh_cache_then_checkout_reproduces_tree ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.72s
```

2. `cargo test -p snapdir-cli --locked` — ALL GREEN, 0 failed across all 20 test binaries (142 tests total; per-binary `test result:` lines all `ok. … 0 failed`), including the trycmd CLI-surface snapshots (37 passed) and the pre-existing store_roundtrip/dryrun/e2e/sync suites — no regressions.

3. `cargo fmt --check -p snapdir-cli` → clean (FMT OK).
   `cargo clippy -p snapdir-cli --all-targets --locked -- -D warnings` (workspace pedantic lints) →

```
    Checking snapdir-stores v1.4.0 (/Users/bermi/code/snapdir/crates/snapdir-stores)
    Checking snapdir-cli v1.4.0 (/Users/bermi/code/snapdir/crates/snapdir-cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.22s
```

## Reuse check / Blockers

- Adapter detection REUSED from the router: `store_is_external()` calls `snapdir_stores::resolve_adapter` (the exact function `resolve_store` uses) and matches `Adapter::External { .. }` — no duplicated scheme parsing; its error contexts mirror `resolve_store`'s, so calling it after a successful `resolve_store` introduces no new failure mode.
- Native file/s3/b2/gcs paths byte-identically untouched: every native arm is the verbatim pre-existing code, only re-indented inside an `else`; FileStore semantics in snapdir-stores untouched (no files outside `crates/snapdir-cli/` modified — `git status` confirms cli.rs + the new test only).
- `put_manifest` came from the already-imported `StreamStore` trait; cache root path from the existing `cache_dir()` helper — nothing new exposed cross-crate.
- No blockers; no cross-lane needs. NOT committed (PM commits).

Ready for PM verification: YES
