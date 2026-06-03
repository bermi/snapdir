# tests handoff for b2-lane-scope-rust-format @ 2026-06-03T00-43-33Z

## Summary
Scoped the live B2 lane of `tests/integration/remote_stores_live.sh` to the proven,
port-meaningful subset, per the operator-approved GATE-BUMP decision. The B2 section
previously ran the full `run_delegate_for b2` differential (Lanes A/B/C) and `die`d on
failure — the C-lane exercises the FROZEN bash oracle's cold-cache fetch-FROM-B2 path,
which has latent legacy b2-CLI bugs (not a Rust defect, not an interop-format diff).

I replaced ONLY the `if run_delegate_for b2; then … else die …` block (keeping
`store_preflight b2` and the existing unreachable-sandbox loud-skip unchanged) with a
scoped B2 check that:
1. **Rust round-trip vs real B2 (MUST pass):** builds the shared `build_corpus` tree,
   `rust id`, `rust push --store <unique scoped sub-path>` (asserts push id == id),
   `rust fetch`, `rust pull` into a dest, `compare_trees` src==dest, re-`id` of dest ==
   id, and `rust verify`. Uses an isolated cache dir + a unique per-run/pid store
   sub-path under `${SNAPDIR_B2_TEST_STORE}`, all under `${WORKDIR}`.
2. **Rust<->Bash snapshot-id agreement (MUST pass):** the frozen oracle (`${ORACLE}` =
   `${REPO_ROOT}/snapdir`) derives the same id via `id --cache-dir=<tmp> <src>` (the
   exact flag form the delegate harness's B/C lanes use); `die` on mismatch (a real
   format/id interop diff, never normalized).
3. **Bash-oracle cold-fetch-FROM-B2:** emitted as a loud `info` NOTE documenting it as a
   known limitation of the frozen legacy oracle, deliberately NOT exercised; never a
   `die`. References that full bidirectional byte-identical interop is proven on S3+GCS.
4. On success prints the literal `scoped: Rust + format compat` green line and adds `b2`
   to `RAN_BACKENDS`.

Reused existing helpers verbatim: the `RUST` runner array, `build_corpus`, `compare_trees`,
and the oracle `id` invocation form. Added one read-only definition (`ORACLE=${REPO_ROOT}/snapdir`)
near the top since this wrapper hadn't defined it. The subshell uses no `local` (it runs at
top-level script scope, not inside a function — `local` would be a runtime error there); the
subshell already scopes the variables so nothing leaks to the parent. The S3 + GCS lanes,
the zero-dependency lane, the AWS-cred remap, endpoint-isolation (FIX A), and v3-b2
provisioning (FIX B/C) are all untouched.

## Files changed
```
 tests/integration/remote_stores_live.sh | 73 +++++++++++++++++++++++++++++----
 1 file changed, 64 insertions(+), 9 deletions(-)
```
(Note: `git diff --stat` also lists a pre-existing `.claude/ralph-loop.local.md` deletion
that was already in the working tree at spawn time — I did not touch it.)

## Local verification result
- `bash -n tests/integration/remote_stores_live.sh` → exit 0.
- `grep -Eq 'scoped: Rust \+ format compat' …` → exit 0.
- `shellcheck tests/integration/remote_stores_live.sh` → clean.
- `bash tests/integration/remote_stores_live.sh --self-check` → exit 0 (all plumbing ok).
- **LIVE run** (`source ~/.config/snapdir/test-creds.sh && bash …remote_stores_live.sh`),
  EXIT=0, with uvx-pinned b2 v3 shim + real B2 sandbox reachable:
  ```
  ok - s3 (MinIO): all Bash<->Rust differential lanes passed byte-identically
  ok - zero-external-dependency: Rust round-trip succeeded with aws/b2/gcloud absent from PATH
  ok - gcs (real): all Bash<->Rust differential lanes passed byte-identically
  ok - b2: reachability preflight OK; running the SCOPED B2 lane (port-meaningful subset)
  ok - b2 (scoped): Rust round-trip vs real B2 (push->fetch->pull->verify) reproduced the tree + same id
  ok - b2 (scoped): Bash oracle derived the SAME snapshot id as Rust (manifest/key/id format compatibility proven)
  [live] b2 (scoped): NOTE — the Bash-oracle cold-cache fetch-FROM-B2 path is a documented KNOWN LIMITATION … not exercised here.
  ok - b2 (real sandbox, scoped: Rust + format compat): Rust round-trip vs real B2 + Rust<->Bash snapshot-id agreement passed; bash-oracle cold-fetch-from-B2 is a documented legacy limitation
  [live] backends ran:     s3 gcs b2
  ok - remote-interop LIVE: backends ran [s3 gcs b2] + zero-external-dependency lane passed
  ```

## Reuse check / Blockers
- S3 + GCS lanes UNTOUCHED — both ran full differential (Lanes A/B/C) live and passed
  byte-identically. No S3/GCS weakening, no normalized diffs.
- Helpers reused: `RUST` array, `build_corpus`, `compare_trees`, oracle `id` flag form.
- No edits to oracle scripts, `utils/qa-fixtures/`, or `crates/**`. Diff is `tests/` only.
- No blockers.

Ready for PM verification: YES
