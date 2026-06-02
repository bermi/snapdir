# stores handoff for b2-oracle-fetch-store-arg @ 2026-06-02T23-13-08Z

## Summary
Operator-approved one-line fix to the frozen Bash oracle `snapdir-b2-store:395`. The parallel object-fetch worker command emitted by `_snapdir_b2_store_get_transfer_objects_command` did not pass `--store`, but the fetch worker requires `_SNAPDIR_B2_STORE_STORE` (set from `--store`) at line 472, so a cold-cache Bash pull from B2 failed with "Missing --store". Appended `--store \"${_SNAPDIR_B2_STORE_STORE}\"` (the canonical global; this function has no local `store`), mirroring the manifest fetch on line 204.

## Files changed
```
 snapdir-b2-store | 2 +-
 1 file changed, 1 insertion(+), 1 deletion(-)
```
(`git diff --stat -- snapdir-b2-store`. The `.claude/ralph-loop.local.md` and `.claude/settings.json` entries in the full `git diff --stat` are pre-existing working-tree state from before this tick and were NOT touched.)

## Local verification result
- `grep -Eq 'fetch --checksum[^\n]*--store[^\n]*_SNAPDIR_B2_STORE_STORE' snapdir-b2-store` -> exit 0
- `bash -n snapdir-b2-store` -> exit 0
- New line 395:
```
			echo "nice ${_SNAPDIR_B2_STORE_BIN_PATH} fetch --checksum \"${checksum}\" --source-path \"${rel_file_path}\" --target-path \"${target_dir}/${rel_file_path}\" --log-file \"$log_file\" --store \"${_SNAPDIR_B2_STORE_STORE}\" & "
```

## Reuse check / Blockers
Confirmed ONLY line 395 of `snapdir-b2-store` changed (1 insertion + 1 deletion). No `crates/`, `tests/`, or `.gatesmith/` files edited. Did not run the oracle test subcommand or any remote-mutating command. Did not commit.

Ready for PM verification: YES
