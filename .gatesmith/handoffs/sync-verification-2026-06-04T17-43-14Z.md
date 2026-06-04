# cli handoff for sync-verification @ 2026-06-04T17:43:14Z

## Summary

Added a dedicated `sync_e2e*` suite (`crates/snapdir-cli/tests/sync_e2e.rs`,
TEST-ONLY — no production-code changes) that goes deeper than the existing
`sync_cmd*` suite, with emphasis on the NO-LOCAL-STAGING property of
`snapdir sync --id <id> --from file://A --to file://B`. All tests are hermetic
(temp `file://` stores + temp caches removed on drop) and use the same
`assert_cmd` + `assert_fs` harness as `tests/e2e.rs`. Trees are multi-file with
nested dirs and deterministic permissions.

Tests (all fns named `sync_e2e*`):
- `sync_e2e_mirror_roundtrips` — push multi-file tree → A, `sync` A→B (exit 0,
  id on stdout), B physically holds `.manifests` + the same `.objects` file
  count as A, and a `pull` from B re-materializes the tree byte-identically
  (`top1.txt`, nested `deep.txt`, deep `leaf.dat`) re-manifesting to the source id.
- `sync_e2e_incremental_second_sync_copies_nothing` — second sync of the same id
  reports `0 copied` on stderr and leaves B's path-set physically unchanged.
- `sync_e2e_dryrun_leaves_dest_untouched` — `--dryrun` against empty B: exit 0,
  stderr "would copy", and B has no `.manifests`, no `.objects`, zero files.
- `sync_e2e_no_local_staging` (THE KEY ONE) — a FRESH, EMPTY cache dir is pinned
  exclusively for the sync (both `--cache-dir` flag and `SNAPDIR_CACHE_DIR`).
  How the assertion works: snapshot the recursive path-set of each guarded dir
  (source tree, store A, store B) BEFORE the sync; run the sync; then assert
  (1) the sync cache is still empty — `count_files == 0` and no `.objects` /
  `.manifests` created (proving sync never staged through the local cache), and
  (2) the source tree and store A path-sets are byte-for-byte unchanged, and the
  ONLY new paths anywhere are inside store B, each starting with `.objects` or
  `.manifests` (the store's own layout, never a scratch/staging dir).
- `sync_e2e_partial_overlap_only_copies_missing` — pre-seed B by syncing a small
  snapshot that shares object bodies with the big one, then sync the big
  snapshot; parse the `synced <id>: N copied, M skipped` summary and assert
  `skipped > 0` and `copied < total_objects_in_A`; finally pull the big id from B
  and confirm it re-manifests to `big_id` (B fully serves it).

## Files changed

```
 crates/snapdir-cli/tests/sync_e2e.rs | 491 +++++++++++++++++++++++++++++++++++
 1 file changed, 491 insertions(+)
```
(crates/snapdir-cli/ only; tests only. No production code touched. The untracked
`.claude/ralph-loop.local.md` is unrelated and not mine.)

## Local verification result

`cargo test -p snapdir-cli --locked sync_e2e -- --nocapture` (last lines):
```
     Running tests/sync_e2e.rs (target/debug/deps/sync_e2e-6a128f2d7318b1c1)

running 5 tests
test sync_e2e_dryrun_leaves_dest_untouched ... ok
test sync_e2e_no_local_staging ... ok
test sync_e2e_incremental_second_sync_copies_nothing ... ok
test sync_e2e_mirror_roundtrips ... ok
test sync_e2e_partial_overlap_only_copies_missing ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.55s
```

Full suite `cargo test -p snapdir-cli --locked`: 14 test binaries, all
`test result: ok`, zero failures (includes the existing `sync_cmd*` suite — no
regressions).

`cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings`:
clean (Finished, no warnings).

`cargo fmt -p snapdir-cli -- --check`: clean (FMT_CLEAN).

## Reuse check / Blockers

- NO production-code edits — the `sync` command already existed and behaves
  correctly; this gate is purely additional test coverage.
- The no-local-staging property HOLDS: with a dedicated fresh cache, a full
  mirror leaves that cache completely empty and creates files only under store
  B's `.objects`/`.manifests`. Proven by before/after path-set diffing.
- No bug found.
- clippy + fmt clean.

Ready for PM verification: YES
