# cli handoff for dryrun-honored @ 2026-06-04T00:46:28Z

## Summary

Made the global `--dryrun` flag a real no-op-writes mode across every
store/FS-mutating command in `crates/snapdir-cli/src/cli.rs`. Previously
`pub dryrun: bool` was declared in `GlobalArgs` but never read, so e.g.
`snapdir push --dryrun` actually uploaded objects.

Guarded every write site by branching on `self.globals.dryrun` at the point
just before the first persistent write, after all read-only computation:

- **run_push** — both paths (staged `--id` path and the normal walk path).
  The snapshot `id` is a pure read-only computation, so it is still computed
  and printed to **stdout** (preserving the scriptable id-on-stdout contract);
  the `store.push()` + `log_event("push", …)` (and, on the `--id` path, the
  discarded scratch materialize) are skipped.
- **run_fetch** — keeps the `store.get_manifest` read; skips the scratch
  materialize + `cache.push()` (the only persistent write).
- **run_checkout** — early-returns before BOTH the cache manifest read and the
  `cache.fetch_files()` + `restore_permissions()` destination writes. The
  notice is intentionally emitted *before* the cache read so `pull --dryrun`
  composes cleanly (see below).
- **run_pull** — composes `run_fetch` + `run_checkout`; both are guarded, so
  pull is automatically dry. Because dry `fetch` leaves the cache unpopulated,
  `checkout`'s dryrun guard had to precede its cache-manifest read, otherwise
  `pull --dryrun` failed with "manifest not found". Verified via
  `dryrun_pull_writes_nothing`.
- **run_stage** — keeps computing/printing `id` to stdout; skips the
  `cache.push()` + `log_event("stage", …)`.
- **run_flush_cache** — skips the destructive `cache::flush_cache()`.
- **run_verify_cache** — under `--dryrun`, `--purge` is forced to `false`
  (`let purge = self.globals.purge && !self.globals.dryrun;`) so corrupt
  objects are NOT deleted, but the corruption report is still printed and the
  non-zero exit on corruption is preserved. Read-only commands
  (`verify`, `manifest`, `id`, `locations`, `ancestors`, `revisions`,
  `defaults`) were left untouched.

UX convention: each guarded command emits a `dry-run: would <action> …
(no writes performed)` line to **stderr**; `push`/`stage` additionally print
the computed `<id>` to **stdout** so scripts keep working. Zero-writes is the
hard invariant the new tests assert.

## Files changed

```
 crates/snapdir-cli/src/cli.rs | 88 ++++++++++++++++++++++++++++++++++-------
 1 file changed, 75 insertions(+), 13 deletions(-)
```

Plus a new (untracked) test file:

```
?? crates/snapdir-cli/tests/dryrun.rs
```

`crates/snapdir-cli/` only — no core/stores/catalog edits.

## Local verification result

`grep -q 'self.globals.dryrun' crates/snapdir-cli/src/cli.rs && cargo test -p snapdir-cli --locked dryrun -- --nocapture` (tail):

```
test dryrun_flush_cache_keeps_objects ... ok
test dryrun_checkout_writes_nothing ... ok
test dryrun_pull_writes_nothing ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.47s
```

(GREP OK; 5 dryrun tests pass:
`dryrun_push_writes_nothing`, `dryrun_stage_writes_nothing`,
`dryrun_flush_cache_keeps_objects`, `dryrun_checkout_writes_nothing`,
`dryrun_pull_writes_nothing`.)

Also clean:
- `cargo test -p snapdir-cli --locked` — full suite passes, no regressions.
- `cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings` — clean.
- `cargo fmt -p snapdir-cli` — applied; `--check` clean.

## Reuse check / Blockers

- No core/stores/catalog edits — the CLI only adds `self.globals.dryrun`
  guards at existing write sites; no business logic moved into the CLI.
- Every mutating command guarded, including `verify-cache --purge` (forced
  read-only under dryrun while preserving the report + non-zero exit).
- Read-only commands untouched.
- clippy + fmt clean. No blockers.

Ready for PM verification: YES
