# cli handoff for pull-fetch-skip-cached @ 2026-06-04T01:19:14Z

## Summary
Added a manifest-present fast path to `run_fetch` (crates/snapdir-cli/src/cli.rs).
Before any store resolution/read, `run_fetch` now consults the local cache
(`self.cache_store()`, a `FileStore`) and, if `cache.get_manifest(id).is_ok()`,
returns early (printing `CACHED: <id>` under `--verbose`). By snapdir's
manifest-written-last invariant a present manifest implies every referenced
object is present — the same invariant `FileStore::push`'s skip-if-manifest-present
relies on — and `get_manifest` re-verifies the cached manifest hashes back to `id`,
so this is a sound local integrity gate. The effect: a 2nd `fetch`/`pull` of the
SAME id performs ZERO store object reads (no network re-download), fixing the
operator-reported same-id repeat-pull regression.

Composition:
- `pull` = `run_fetch` + `run_checkout`. The fetch leg now short-circuits on a
  cache hit; checkout already read from the cache, so a repeat `pull` is fully
  cache-served.
- `--dryrun`: the cache-hit early return is write-free, so a cached-id dry fetch
  is a clean no-op. For an uncached id, the existing dryrun guard (after
  `resolve_store`) is unchanged, so a dry fetch on an uncached id stays a no-op
  exactly as before — not regressed.
- Error precedence preserved: the cache is only consulted when `--id` is actually
  present (`self.globals.id.as_deref()`); with no id we fall through so the
  canonical "missing --store option" error still fires first (keeps the frozen
  `error-fetch-missing-store.trycmd` snapshot green).

Deferred follow-up (NOT implemented): the partial case — cache holds SOME objects
but not the manifest (e.g. objects shared with another snapshot) — could fetch
only the cache-missing objects, but that needs an object-presence API on the
`Store` trait (stores lane), which doesn't exist yet. The manifest-present fast
path fully fixes the reported regression and satisfies this gate.

## Files changed
```
 crates/snapdir-cli/src/cli.rs   | 25 +++++++++++++++++-
 crates/snapdir-cli/tests/e2e.rs | 58 +++++++++++++++++++++++++++++++++++++++++
 2 files changed, 82 insertions(+), 1 deletion(-)
```
Test added: `fetch_cached_skips_store_objects` in tests/e2e.rs — push + pull #1
(populates cache+dest), DELETE the store's `.objects` subtree (keep `.manifests`),
then pull #2 of the same id into a fresh dest must SUCCEED and re-manifest to the
same id (proves zero store object reads + correctness, not a silent skip); plus a
bare `fetch` of the cached id as a no-op success.

## Local verification result
`cargo test -p snapdir-cli --locked` (last lines):
```
running 7 tests
test manifest_*_golden ... ok (7 passed)

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

     Running tests/store_roundtrip.rs
running 4 tests
test push_by_unknown_id_errors_without_walking_cwd ... ok
test store_roundtrip_fetch_then_checkout_separately ... ok
test push_by_staged_id_pushes_the_staged_snapshot_not_cwd ... ok
test store_roundtrip_push_then_checkout_reproduces_tree ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
All 12 cli test binaries report `test result: ok`, no failures. Targeted:
- `cargo test -p snapdir-cli --locked fetch_cached -- --nocapture` →
  `test fetch_cached_skips_store_objects ... ok`
- e2e.rs `pull_is_fetch_plus_checkout`, store_roundtrip.rs, dryrun.rs, and the
  cli_surface trycmd snapshots all still pass.
- `cargo clippy -p snapdir-cli --all-targets --all-features --locked -- -D warnings`
  → clean.
- `cargo fmt -p snapdir-cli --check` → clean.

## Reuse check / Blockers
- No core/stores/catalog edits — cli-only diff. Uses the existing public
  `cache_store().get_manifest(id)` (`Store` trait). Logic stays in the libs.
- `--dryrun` not regressed (cached-id dry fetch is write-free; uncached-id dry
  fetch unchanged).
- Frozen CLI error precedence preserved (missing-store error still fires first
  with no id).
- Partial-object-skip deferred as a documented follow-up needing a stores
  object-presence API.
- clippy + fmt clean.

Ready for PM verification: YES
