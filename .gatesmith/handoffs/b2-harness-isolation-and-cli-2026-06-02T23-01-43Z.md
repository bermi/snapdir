# tests handoff for b2-harness-isolation-and-cli @ 2026-06-02T23-01-43Z

## Summary
Made the live B2 lane in `tests/integration/remote_stores_live.sh` actually executable by fixing the two proven blockers (neither a Rust defect — Lane A Rust↔Rust B2 already passes).

**FIX A — endpoint leak (Rust B2 client was misrouted to MinIO).** The S3 lane exports `SNAPDIR_S3_STORE_ENDPOINT_URL=<MinIO>` (in `start_minio`) and never unset it. The Rust CLI's `store_for_adapter` reads it as an endpoint override that takes precedence over `SNAPDIR_B2_TEST_ENDPOINT` for `Adapter::B2`, so the B2 preflight PUT went to MinIO → `NoSuchBucket` → silent B2 skip. Unset it in TWO places, both AFTER the S3 + zero-dep lanes (which legitimately need it) have run:
  1. At the very start of the B2 section's `else` branch, before the preflight, with a comment explaining the leak.
  2. Appended to the `unset …` list in `run_delegate_for`'s `b2)` case.

**FIX B — b2 CLI v3 provision for the bash oracle.** The frozen oracle `snapdir-b2-store` uses b2 v3 subcommands; the system `b2` is v4.7.0, which dropped them. Added a `b2_is_v3` detector (parses the FIRST `N.N.N` token from `b2 version`; deliberately NOT the `upload-file` probe, since v4 still accepts that as a deprecated alias) and a `provision_b2_v3` helper that:
  - uses the system `b2` as-is when it is v3.x (prints empty → no shim);
  - when b2 is v4+ AND `uvx` is available, writes a tiny executable `b2` shim under `WORKDIR` that `exec uvx --quiet --python 3.11 --from 'b2<4' --with 'docutils==0.18.1' b2 "$@"` (the `docutils==0.18.1` pin is required — b2 v3 imports `docutils.utils.error_reporting`, removed in 0.19), and the B2 lane PREPENDs that shim dir to PATH for the lane's duration;
  - returns non-zero when neither a v3 b2 nor uvx exists → the lane LOUD-SKIPs with remediation (never a false pass).
The lane-entry gate was widened from `command -v b2` to `command -v b2 || command -v uvx` so a v4-only host can still provision v3 via uvx.

The shim is named `b2`, which the existing zero-dependency PATH-sanitizer already drops by name; the zero-dep lane also runs BEFORE the B2 section and only ever sees the sanitized PATH, so the shim cannot leak into it. Verified the sanitizer's `--self-check` assertions (`aws/b2/gcloud` hidden, `b3sum` reachable) still pass.

Validated out-of-band: `b2 version` → v4.7.0 (detector → major 4, takes shim path); `uvx … --from 'b2<4' … b2 version` → v3.19.1 and runs clean (docutils pin satisfied, no import error). Script is shellcheck-clean and `set -euo pipefail`-safe; the shim dir lives under `WORKDIR` and is removed by the existing cleanup trap.

## Files changed
```
 tests/integration/remote_stores_live.sh | 127 ++++++++++++++++++++++++++++----
 1 file changed, 114 insertions(+), 13 deletions(-)
```
(`git diff --name-only` shows only `tests/integration/remote_stores_live.sh`; the `.claude/ralph-loop.local.md` deletion was pre-existing in the start-of-session snapshot, not mine. No `crates/**`, oracle, or `utils/qa-fixtures/` edits.)

## Local verification result
```
shellcheck OK
=== VERIFY ===
[live] self-check: validating live-wrapper plumbing (no containers)
ok - self-check: docker present + daemon reachable
ok - self-check: delegate harness present (/Users/bermi/code/snapdir/tests/integration/remote_stores.sh)
ok - self-check: delegate harness --self-check passed
ok - self-check: free_port returns a valid TCP port (62477)
ok - self-check: PATH-sanitizer hides aws/b2/gcloud while keeping b3sum reachable
ok - self-check: Rust binary resolvable (/Users/bermi/code/snapdir/target/debug/snapdir)
ok - self-check passed: live-wrapper plumbing OK (no emulators required)
VERIFY EXIT: 0
```
PM verification command:
`bash tests/integration/remote_stores_live.sh --self-check && grep -Eq 'unset[^\n]*SNAPDIR_S3_STORE_ENDPOINT_URL' … && grep -Eqi "b2<4|uvx|b2 .*version|upload-file|account authorize" …` → **exit 0**.

## Reuse check / Blockers
- No edits to the frozen oracle (`snapdir-b2-store` etc.), `utils/qa-fixtures/`, or `crates/**`. Only `tests/integration/remote_stores_live.sh`.
- No interop assertion was weakened: the differential lanes still delegate verbatim to `remote_stores.sh`, and a real diff is still a HARD `die`. The only new skip paths are operator-env conditions (no usable b2 v3 client), each LOUD with remediation — never a false pass.
- Zero-dependency lane safety: the v3 shim is named `b2`, dropped by name by the existing PATH-sanitizer; it lives under `WORKDIR` (removed by the cleanup trap); and the B2 section runs strictly AFTER the zero-dep lane. The zero-dep lane builds and uses only the sanitized symlink-farm PATH, so the prepended shim never reaches it. `--self-check` confirms the sanitizer still hides aws/b2/gcloud and keeps b3sum reachable.

Ready for PM verification: YES
