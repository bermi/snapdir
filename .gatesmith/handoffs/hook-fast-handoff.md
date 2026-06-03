# Hook fast-legs handoff

## Summary

Made the installed git pre-push hook practical for routine pushes by switching it
to the FAST CI-equivalent legs (fmt / clippy `--all-features -D warnings` / test /
deny / audit / doctests / shear / semver, ~2–4 min) instead of the FULL suite,
whose emulated-docker musl + coverage legs take 30–40 min on this macOS host and
drive `--no-verify` abuse. musl + coverage remain verified in CI (native Linux
runners) and are available locally via `make ci-local` (full suite).

Changes:
- `utils/git-hooks/pre-push`: `exec` now passes `--fast`; header comment rewritten
  to describe the fast legs and note musl + coverage are covered by CI / `make
  ci-local`. `git push --no-verify` bypass note kept.
- `utils/ci/pre-push.sh`: `--fast` help block (and the runtime `--fast` warn line)
  updated — no longer claims the hook always runs the full suite; now states the
  pre-push hook uses `--fast` and the full suite runs via `make ci-local` /
  `bash utils/ci/pre-push.sh` (no flag). No check logic changed (comments/help/
  warn text only).
- `CONTRIBUTING.md`: "Local CI gate (pre-push hook)" section now documents the
  fast legs the hook runs and that musl + coverage are covered by CI + `make
  ci-local`.

## Files changed (git diff --stat)

```
 CONTRIBUTING.md          | 12 ++++++++----
 utils/ci/pre-push.sh     |  9 ++++++---
 utils/git-hooks/pre-push | 14 ++++++++++----
 3 files changed, 24 insertions(+), 11 deletions(-)
```

## Local verification result

- `bash -n utils/git-hooks/pre-push` → SYNTAX_HOOK_OK
- `bash -n utils/ci/pre-push.sh` → SYNTAX_SH_OK
- `shellcheck utils/git-hooks/pre-push utils/ci/pre-push.sh` → SHELLCHECK_CLEAN (no findings)
- `bash utils/ci/pre-push.sh --fast` → **exit 0**, "All checks passed. Safe to push."

Per-leg results (`--fast` run):

| Leg | Result |
| --- | --- |
| 1/6 rustfmt --check | ok |
| 1/6 clippy (-D warnings, --all-features) | ok |
| 1/6 typos | ok |
| 1/6 actionlint | ok |
| 1/6 cargo-shear | ok |
| 1/6 cargo-semver-checks | warn (non-blocking, mirrors ci.yaml `\|\| true`: snapdir-core not on crates.io) |
| 2/6 cargo-deny | ok |
| 2/6 cargo-audit | ok |
| 3/6 build (workspace, --all-features, locked) | ok |
| 3/6 test (workspace, --all-features, locked) | ok |
| 3/6 MSRV 1.91.1 build | ok |
| 3/6 MSRV 1.91.1 test | ok |
| 4/6 Static musl | SKIPPED (--fast) |
| 5/6 doctests | ok |
| 6/6 Coverage | SKIPPED (--fast) |

## Reuse check / Blockers

- Reuse: no new flags or code paths — reused the existing `--fast` flag and its
  group 4/6 skip logic. Comment/help/warn text only; check logic untouched.
- The semver-checks `warn` is pre-existing and non-blocking (CI uses `|| true`);
  it does not affect the exit code.
- No commits/pushes made (per task). Scope limited to the three named files.
- Blockers: none.

Ready for PM verification: YES
