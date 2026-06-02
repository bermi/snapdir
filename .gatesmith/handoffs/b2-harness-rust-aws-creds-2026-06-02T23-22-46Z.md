# tests handoff for b2-harness-rust-aws-creds @ 2026-06-02T23-22-46Z

## Summary
The Rust B2 store uses `aws-sdk-s3` against Backblaze's S3-compatible endpoint and
authenticates via the STANDARD AWS credential chain (`AWS_ACCESS_KEY_ID` /
`AWS_SECRET_ACCESS_KEY`). `~/.config/snapdir/test-creds.sh` sets those to the real-AWS
key (`AKIA…`) for the S3/AWS lane and never remaps them for the B2 lane, so the Rust B2
preflight HEAD 403s. Added "FIX C" in the B2 `else` branch (where `SNAPDIR_B2_TEST_STORE`
and a usable b2 v3 client are confirmed), placed BEFORE `store_preflight b2` alongside the
existing `unset SNAPDIR_S3_STORE_ENDPOINT_URL` (FIX A) and the v3 shim (FIX B). It exports
the B2 application key as `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` and `AWS_REGION`
(from `SNAPDIR_B2_REGION`, default `us-west-001`), with `:?` guards so a missing B2 key
fails loudly rather than silently authenticating as AWS. The export is in scope for both
the `store_preflight b2` call and the subsequent `run_delegate_for b2` subshell (which
inherits the parent env). It runs AFTER the S3/MinIO + zero-dependency lanes, so those are
unaffected.

## Files changed
```
 tests/integration/remote_stores_live.sh | 12 ++++++++++++
 1 file changed, 12 insertions(+)
```
(`.claude/ralph-loop.local.md` appears in `git status` but is a pre-existing change from
the initial working tree, not part of this gate.)

## Local verification result
```
$ bash tests/integration/remote_stores_live.sh --self-check && \
  grep -Eq 'AWS_ACCESS_KEY_ID=.*SNAPDIR_B2_STORE_APPLICATION_KEY_ID' tests/integration/remote_stores_live.sh && \
  grep -Eq 'AWS_SECRET_ACCESS_KEY=.*SNAPDIR_B2_STORE_APPLICATION_KEY' tests/integration/remote_stores_live.sh
[live] self-check: validating live-wrapper plumbing (no containers)
ok - self-check: docker present + daemon reachable
ok - self-check: delegate harness present (.../tests/integration/remote_stores.sh)
ok - self-check: delegate harness --self-check passed
ok - self-check: free_port returns a valid TCP port (63051)
ok - self-check: PATH-sanitizer hides aws/b2/gcloud while keeping b3sum reachable
ok - self-check: Rust binary resolvable (.../target/debug/snapdir)
ok - self-check passed: live-wrapper plumbing OK (no emulators required)
exit 0

bash -n tests/integration/remote_stores_live.sh  -> syntax OK
shellcheck -S warning tests/integration/remote_stores_live.sh -> clean
```

## Reuse check / Blockers
- Placement confirmed AFTER the S3/MinIO + zero-dependency lanes (those are earlier in the
  file and ran already); the new exports do not disturb them.
- Inside the confirmed `else` branch where `SNAPDIR_B2_TEST_STORE` is set and a usable b2 v3
  client is provisioned; the existing `b2:` skip guards remain intact.
- No edits to the Bash oracle, `utils/qa-fixtures/`, or `crates/**`. `git diff --stat` shows
  only `tests/integration/remote_stores_live.sh`.
- No assertions weakened; new exports are `:?`-guarded so a missing B2 key fails loudly.

Ready for PM verification: YES
