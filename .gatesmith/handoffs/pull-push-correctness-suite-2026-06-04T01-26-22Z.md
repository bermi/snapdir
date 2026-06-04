# cli handoff for pull-push-correctness-suite @ 2026-06-04T01:26:22Z

## Summary
Added four binary-level end-to-end regression tests to
`crates/snapdir-cli/tests/e2e.rs`, all built on the existing harness
(`snapdir(cache)`, `build_tree`, `stdout_ok`; hermetic `file://` store + temp
cache removed on drop). One new local helper, `count_files(dir)`, was added to
the file to count materialized files (store/cache/dest) for the dryrun
assertions.

1. **`push_pull_pull_is_idempotent`** — push a tree to a `file://` store; pull
   into a dest; pull AGAIN into the SAME dest. Asserts both pulls exit 0, the
   dest re-manifests to the source id after each pull, and contents are stable
   across the repeated pull (idempotent, no error, dest unchanged).
   Complements `fetch_cached_skips_store_objects` by focusing on positive
   idempotency / dest stability rather than "no store reads".

2. **`dryrun_push_leaves_store_empty_e2e`** — `push --dryrun` against an empty
   `file://` store: asserts the store has NO `.objects` and NO `.manifests`
   (and 0 files total) afterward, exit 0, and the pure-computation id is still
   printed. Then (after a real push to a second store) a `pull --dryrun` into a
   fresh dest + fresh cache: asserts both dest and cache remain empty.
   Intentionally overlaps `tests/dryrun.rs` so THIS gate's verification (e2e +
   store_roundtrip only) actually exercises dry-run.

3. **`pull_repairs_corrupted_dest_file`** — push → pull (populates cache +
   dest); overwrite `a.txt` with wrong bytes (asserts the dest no longer
   re-manifests to the source id); amputate the store's `.objects` to prove the
   repair is served offline from the cache; pull AGAIN into the same dest.
   Asserts the corrupted file is restored to "hello", `sub/b.txt` intact, and
   the dest re-manifests to the source id.

4. **`manifest_multi_exclude_drops_paths_e2e`** — build the known tree plus
   `node_modules/x` and `coverage/y`; run `manifest --exclude
   node_modules,coverage` (comma form) and `--exclude node_modules --exclude
   coverage` (repeated form). Asserts both omit the excluded paths, keep
   `./a.txt` + `./sub/b.txt`, and produce identical output; a plain `manifest`
   includes both subtrees.

   Note: the original spec suggested `tmp` as an exclude token. The exclude
   regex matches against the **absolute** scan path, and the test temp dir lives
   under `/tmp/...` (TMPDIR=/tmp/claude-501 in this environment), so `--exclude
   tmp` matched the ancestor temp prefix and excluded the entire tree. Swapped
   `tmp` → `coverage` (a distinctive name absent from the temp prefix); the
   behavior being tested (multi/comma exclude drops manifest paths) is
   unchanged. This is a test-fixture concern, NOT a product bug — `--exclude`
   correctly matches anywhere in the path as designed (`grep -E -v` semantics).

## Files changed
```
 crates/snapdir-cli/tests/e2e.rs | 272 ++++++++++++++++++++++++++++++++++++++++
 1 file changed, 272 insertions(+)
```

## Local verification result
```
     Running tests/e2e.rs (target/debug/deps/e2e-9ba39d8b622893df)

running 12 tests
test fetch_without_store_fails_with_clear_message ... ok
test verify_purge_is_rejected ... ok
test checkout_unknown_id_fails ... ok
test verify_without_purge_does_not_hit_purge_error ... ok
test id_is_64_lowercase_hex ... ok
test manifest_multi_exclude_drops_paths_e2e ... ok
test dryrun_push_leaves_store_empty_e2e ... ok
test pull_is_fetch_plus_checkout ... ok
test push_pull_pull_is_idempotent ... ok
test push_fetch_checkout_roundtrip_reproduces_id ... ok
test pull_repairs_corrupted_dest_file ... ok
test fetch_cached_skips_store_objects ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.44s

     Running tests/store_roundtrip.rs (target/debug/deps/store_roundtrip-92962f7cb3e5f98d)

running 4 tests
test push_by_unknown_id_errors_without_walking_cwd ... ok
test push_by_staged_id_pushes_the_staged_snapshot_not_cwd ... ok
test store_roundtrip_fetch_then_checkout_separately ... ok
test store_roundtrip_push_then_checkout_reproduces_tree ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
```

## Reuse check / Blockers
- Reused the existing e2e harness (`snapdir`, `build_tree`, `stdout_ok`); added
  one small `count_files` helper for the dryrun no-write assertions. Used the
  established "amputate `.objects`" trick (from `fetch_cached_skips_store_objects`)
  to prove offline repair in scenario 3.
- TEST-ONLY: no production-code changes; logic stays in the libs.
- No real product bug found. The four correctness properties all hold at the
  binary level. The only adjustment was a test-fixture exclude-token rename
  (`tmp` → `coverage`) to avoid the temp-dir-prefix collision described above.
- `cargo test -p snapdir-cli --locked` (full suite) green; `cargo clippy
  -p snapdir-cli --all-targets --all-features --locked -- -D warnings` clean;
  `cargo fmt -p snapdir-cli -- --check` clean.

Ready for PM verification: YES
