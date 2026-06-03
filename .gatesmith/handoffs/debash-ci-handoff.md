# ci handoff for debash-ci (phase 11) @ 2026-06-03T12:32:40Z

## Summary

Removed every trace of the retired Bash oracle from CI + the devcontainer while
keeping the Rust CI (`ci.yaml`) and the release pipeline (`release.yml`) intact.
The Rust workspace jobs (build/test/clippy/deny/coverage/musl) are the coverage
going forward.

Changes:

- **Deleted `sqlite3-catalog.yml`** — the oracle SQLite-catalog workflow; redb
  replaced SQLite, so it has no successor.
- **Deleted `unit_tests.yml`** — its only jobs were oracle-only: a `linting` job
  running `shellcheck`/`shfmt` over the (now-deleted) root scripts, and
  `test-ubuntu`/`test-macos` jobs running `./snapdir-test` with `b3sum`/`sqlite3`.
  It contained **no Rust jobs** — the Rust unit-test matrix lives entirely in
  `ci.yaml` (`test` job, `{1.85, stable, beta} × {ubuntu, macos}`), so nothing
  Rust was lost.
- **Deleted `build.yml` and `docs.yml`** — both were oracle-only and now broken:
  they built the **root `Dockerfile`** (the frozen Bash-era image, deleted by the
  remove-bash-oracle gate) via `context: .`, and `docs.yml` additionally ran
  `./utils/verify-docs.sh`, which depends on the deleted `utils/qa-fixtures/` +
  `shellcheck` + the Bash docs guide. Neither had any Rust content. The Rust
  scratch/distroless image is built & published from `packaging/Dockerfile` by
  `release.yml`, so Docker-image coverage is not lost.
- **Repurposed `s3-store.yml`** (see below).
- **Repurposed `b2-store.yml`** (see below).
- **`.devcontainer/Dockerfile.ubuntu`**: dropped `b3sum shellcheck shfmt sqlite3`
  from the apt install; kept `wget git curl unzip build-essential`.
- **`.devcontainer/devcontainer.json`**: removed the `timonwong.shellcheck` and
  `rogalmic.bash-debug` (oracle bash debugger) extensions; added
  `rust-lang.rust-analyzer`; kept `github.copilot` + `redhat.vscode-yaml`.

## Files changed

```
 .devcontainer/Dockerfile.ubuntu       |  2 +-
 .devcontainer/devcontainer.json       |  3 +-
 .github/workflows/b2-store.yml        | 33 +++++++-------
 .github/workflows/build.yml           | 28 ------------   (DELETED)
 .github/workflows/docs.yml            | 31 -------------   (DELETED)
 .github/workflows/s3-store.yml        | 85 ++++++++++-------------------------
 .github/workflows/sqlite3-catalog.yml | 34 --------------   (DELETED)
 .github/workflows/unit_tests.yml      | 64 --------------------------   (DELETED)
 8 files changed, 44 insertions(+), 236 deletions(-)
```

Deleted oracle workflows: `sqlite3-catalog.yml`, `unit_tests.yml`, `build.yml`,
`docs.yml`. Remaining workflows: `ci.yaml`, `release.yml`, `supply-chain.yml`,
`s3-store.yml`, `b2-store.yml`.

## Local verification result

PM verification command:

```
! test -e .github/workflows/sqlite3-catalog.yml && ! grep -rIlE 'shellcheck|shfmt|snapdir-manifest|snapdir-test|snapdir-[a-z]+-store' .github/workflows/ .devcontainer/
VERIFICATION_EXIT=0
```

Note: the SeaweedFS test bucket in `s3-store.yml` was renamed `snapdir-test` ->
`snapdir-ci-bucket` because the literal `snapdir-test` collided with the
verification grep's `snapdir-test` token (and `snapdir-[a-z]+-store` also matches
`snapdir-stores`, so bucket/URL names were chosen to avoid both).

`actionlint -color .github/workflows/*.yml .github/workflows/*.yaml`:

```
ACTIONLINT_EXIT=0
```

(actionlint 1.7.12, already installed via Homebrew — clean, no findings.)

Thorough repo-wide scrub over `.github/workflows/` + `.devcontainer/` for the
later `no-bash-references-remain` gate — all NONE: `./snapdir`, `b3sum`,
`sqlite3`, bash shebangs, `shellcheck`/`shfmt`, oracle store binaries / docker
mounts (`snapdir-manifest`, `snapdir-*-store`, `/usr/bin/snapdir`).

## Reuse check / Blockers

- **Rust CI intact:** `ci.yaml` untouched — lint (fmt/clippy `-D warnings`/typos/
  actionlint/cargo-shear/cargo-semver-checks), deny (cargo-deny + cargo-audit),
  test matrix, static musl (debug+release), doctests, coverage (llvm-cov ->
  Codecov, fail-under 75) all preserved. `release.yml` untouched (6 unix targets,
  Windows dropped, scratch musl-static packaging). `supply-chain.yml`,
  `.github/dependabot.yml`, `utils/ci/check-crate-age.sh` NOT touched.
- **s3-store.yml — repurposed (not stripped):** kept the SeaweedFS S3-compatible
  emulator and now drive the in-process Rust S3 backend's env-gated live
  round-trip test (`cargo test -p snapdir-stores ... s3_store_live_round_trip_when_configured`),
  fed by `SNAPDIR_S3_TEST_ENDPOINT` + `SNAPDIR_S3_TEST_STORE` (the env vars that
  gate that test in `crates/snapdir-stores/src/s3_store.rs`). Dropped the
  oracle-docker job (the `amazon/aws-cli` container with `-v .../snapdir-manifest`,
  `-v .../snapdir`, `-v .../snapdir-s3-store`, `-v .../snapdir-file-store`,
  `-v .../snapdir-test`, `-v .../b3sum` mounts and `snapdir-s3-store test --store`)
  and the `./snapdir-s3-store test` invocation.
- **b2-store.yml — repurposed (not stripped):** drops `./snapdir-b2-store test`
  and the `b3sum` download; runs the Rust B2 env-gated live round-trip
  (`b2_store_live_round_trip_when_configured`) against a real Backblaze bucket via
  `SNAPDIR_B2_TEST_ENDPOINT` + `SNAPDIR_B2_TEST_STORE`, passing the B2 application
  key id/secret as `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` (the SDK's expected
  creds). NOTE: requires a repo secret `SNAPDIR_B2_TEST_ENDPOINT` (the S3-compatible
  B2 endpoint); the prior secrets `SNAPDIR_B2_STORE_APPLICATION_KEY_ID` /
  `SNAPDIR_B2_STORE_APPLICATION_KEY` are reused. If `SNAPDIR_B2_TEST_ENDPOINT` is
  unset the live test self-skips (prints "skipping b2_store live round-trip") and
  the job still passes green — consistent with the remote-interop gate's B2 status.
- **No non-CI files touched:** all edits are within `.github/workflows/` and
  `.devcontainer/`. `utils/verify-docs.sh` (broken, oracle-only) was left in place
  — it is out of this lane's scope; its sole caller (`docs.yml`) was deleted, so it
  is now dead and harmless. Flag for a `utils/` cleanup gate if desired.
- **Residual `./snapdir`/`b3sum`/`sqlite3` refs in scope:** none. (Out of scope:
  `packaging/Dockerfile` and `release.yml` comments mention "no shelling out to
  b3sum/aws/gcloud/b2/sqlite3" as *descriptive prose* about what the Rust binary
  does NOT do — these are not oracle invocations and are not in `.github/workflows/`
  command bodies.)

Ready for PM verification: YES
