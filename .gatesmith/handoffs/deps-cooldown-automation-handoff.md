# ci handoff for deps-cooldown-automation @ 2026-06-03

## Summary

Added two-layer supply-chain protection so the workspace never adopts a crate
version that has been public for fewer than 3 days:

1. **`.github/dependabot.yml`** — Dependabot v2 with a **3-day cooldown** on
   `cargo` (`/`), `github-actions` (`/`), and `docker` (`/packaging`, where the
   Dockerfile lives). Each ecosystem uses `schedule.interval: weekly`, a sane
   `open-pull-requests-limit`, `cooldown.default-days: 3` (plus explicit
   `semver-major/minor/patch-days: 3` for clarity), and groups minor/patch
   updates into a single PR.
2. **`utils/ci/check-crate-age.sh`** — bash (`set -euo pipefail`, shellcheck
   clean, executable). Parses `Cargo.lock`, selects only packages whose source
   is the crates.io registry (skips path/git/workspace-local deps), queries
   `https://crates.io/api/v1/crates/<name>/<version>` for `.version.created_at`
   with a descriptive `User-Agent`, polite 0.3s spacing, and 429/5xx backoff,
   then asserts age ≥ `MIN_AGE_DAYS` (default 3). Flags: `--help`, `--lock`,
   `--min-age-days`. Requires `jq`+`curl` (clear error if missing). Transient
   API/network errors are a hard exit 2 — never a silent pass. Exit 0 = all old
   enough; exit 1 = lists every offender.
3. **`.github/workflows/supply-chain.yml`** — runs `check-crate-age.sh` on PRs
   touching the lockfile/script and on a weekly cron, plus a `cargo deny check
   advisories` job. `permissions: contents: read`, concurrency-guarded.

No `crates/**`, oracle, or `utils/qa-fixtures/` changes.

## Files changed

```
 .github/dependabot.yml             |  60 ++++++++++
 .github/workflows/supply-chain.yml |  50 +++++++++
 utils/ci/check-crate-age.sh        | 250 +++++++++++++++++++++++++++++++++
```
(The `.claude/ralph-loop.local.md` deletion in the tree is pre-existing, not
from this lane.)

## Local verification result

- **Dependabot validity:** `version: 2`, `updates:` present, three ecosystems
  `["cargo","github-actions","docker"]`, all `cooldown.default-days == 3`
  (verified by Ruby `YAML.load_file`).
- **`--help`:** `bash utils/ci/check-crate-age.sh --help` → exit 0.
- **shellcheck:** clean (`shellcheck utils/ci/check-crate-age.sh` → no output).
- **actionlint:** `supply-chain.yml` passes clean. (The many actionlint warnings
  in the repo are all in pre-existing legacy oracle workflows — b2-store,
  build, docs, s3-store, sqlite3-catalog, unit_tests — not in this lane's file.)
- **Gate verification command** (PM re-runs): exits **0**.
- **Real run against current `Cargo.lock`** (440 registry crates, live
  crates.io): 439 PASS, **1 FAIL** → exit 1. Tail:

  ```
  PASS  zerovec                        0.11.6       age=62d
  PASS  zerovec-derive                 0.11.3       age=62d
  PASS  zmij                           1.0.21       age=110d

  check-crate-age.sh: 1 of 440 registry crate(s) younger than 3 day(s).
  ```

  The single offender:

  ```
  FAIL  rustls-native-certs  0.8.4  age=1d  (published 2026-06-01T12:16:09Z, < 3d)
  ```

  This is a **correct** detection, not a script bug — `rustls-native-certs
  0.8.4` is a transitive dep (not declared in any manifest) published one day
  ago, exactly the freshly-published window the cooldown exists to block.

## Reuse check / Blockers

- ring TLS provider unaffected; no `aws-lc-rs` introduced; no oracle edits;
  diff is `.github/` + `utils/ci/` only.
- **Blocker for a green CI run (NOT for this gate):** the live check exits 1 on
  `rustls-native-certs 0.8.4` (transitive, 1 day old). That is out of this
  gate's file scope (it would require a `Cargo.lock` dependency-resolution
  change, which I did not make). Options for the PM / a follow-up dep lane:
  pin `rustls-native-certs` to the prior aged version until 0.8.4 crosses 3
  days, or re-run the supply-chain workflow after 2026-06-04 (when 0.8.4 ages
  past the threshold) and it will go green on its own. The Dependabot cooldown
  would have prevented adopting it in the first place going forward.

Ready for PM verification: YES
