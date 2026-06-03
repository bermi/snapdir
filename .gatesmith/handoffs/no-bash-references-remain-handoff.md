# generic handoff for no-bash-references-remain @ 2026-06-03

## Summary

Built the final Group-A de-bash guard and scrubbed the last live-oracle-implying
references in editable source. Specifically:

- Added `utils/ci/check-no-bash.sh` — a shellcheck-clean bash guard that FAILS
  (exit 1) if the deleted Bash oracle is reintroduced or invoked, exits 0
  otherwise. Two check families:
  - **(A) Path existence:** none of the deleted oracle artifacts exist — the 8
    root `snapdir*` scripts, root bash `Dockerfile` (guarded: the live root
    `Dockerfile` is the Rust image; it only fails if it `COPY`s an oracle
    `snapdir*` script), `utils/qa-fixtures/`, `utils/pre-commit-hook.sh`, and the
    now-removed `utils/{generate-docs,verify-docs,install}.sh`.
  - **(B) Live invocation / oracle-tooling call** in executable/CI contexts
    (`.github/workflows/`, root `Makefile`, tracked `*.sh`): no live
    `./snapdir-*` (or bare `snapdir-{manifest,file-store,…}`) command, and no
    real `shellcheck`/`shfmt`/`b3sum`/`sqlite3` call. Whole-line `#` comments are
    stripped before scanning, and `# shellcheck disable=` linter directives are
    subtracted so they aren't mistaken for a `shellcheck` *call*.
  - The new CI plumbing is excluded by name (`check-no-bash.sh`, `pre-push.sh`,
    `check-crate-age.sh`, `utils/git-hooks/*`). The guard never content-scans
    `.git/`, `target/`, `.gatesmith/`, `docs/`, or `*.rs` (Rust comments and the
    sha-locked core files are excluded by construction — see the script's header).

- Removed 3 genuinely-dead bash-era scripts (see decisions below).
- Scrubbed live-oracle-implying comments to past-tense historical notes in 6
  NON-sha-locked source files (comment-only; no logic changed).

The guard's negative path was exercised: reintroducing `snapdir-manifest`, a live
`./snapdir-manifest` invocation in a tracked `.sh`, and a `sqlite3` call each
correctly produced exit 1; the clean tree produces exit 0.

## Files changed

```
 crates/snapdir-catalog/src/lib.rs           | 29 +++++++++++++------------
 crates/snapdir-cli/src/cli.rs               | 19 +++++++++--------
 crates/snapdir-cli/tests/catalog_logging.rs |  2 +-
 crates/snapdir-core/src/store.rs            |  5 +++--
 crates/snapdir-core/src/walk.rs             | 15 +++++++------
 crates/snapdir-core/tests/compat_golden.rs  | 13 +++++-------
 utils/generate-docs.sh                      | (deleted, git rm)
 utils/install.sh                            | (deleted, git rm)
 utils/verify-docs.sh                        | (deleted, git rm)
 utils/ci/check-no-bash.sh                   | (new, +160, untracked — PM to add)
```

## Local verification result

```
$ bash utils/ci/check-no-bash.sh
check-no-bash.sh: OK — no Bash oracle artifacts or live invocations remain.

$ cargo build --workspace --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ cargo test --workspace --locked
... (every test binary) test result: ok. N passed; 0 failed; ...
TOTAL PASSED: 232   (0 failed, 0 ignored)

$ shasum -a 256 -c .gatesmith/manifest-format.sha.lock
crates/snapdir-core/src/manifest.rs: OK
crates/snapdir-core/src/merkle.rs: OK
crates/snapdir-core/src/excludes.rs: OK

$ shellcheck utils/ci/check-no-bash.sh
shellcheck: clean

Final combined exit: 0
```

## Reuse check / Blockers

- **Guard exclusion rationale** (documented in the script header): the new CI
  tooling (`check-no-bash.sh`, `pre-push.sh`, `check-crate-age.sh`,
  `utils/git-hooks/*`) is excluded because it legitimately carries the binary
  name and `# shellcheck disable=` directives. `.git/`, `target/`, `.gatesmith/`,
  `docs/`, and Rust `*.rs` comments are excluded because they legitimately retain
  historical references (ADRs, port history, engineering notes) and the frozen
  sha-locked `crates/snapdir-core/src/{manifest,merkle,excludes}.rs` must not be
  edited — their comments are excluded rather than scrubbed.

- **Scripts removed vs kept:**
  - `utils/verify-docs.sh` — REMOVED (dead). Its only caller (`docs.yml`) was
    deleted in debash-ci, and it builds/runs the deleted `snapdir-test` docker
    image and depends on the deleted `utils/qa-fixtures/`.
  - `utils/generate-docs.sh` — REMOVED (dead). Pure bash-oracle tooling:
    `find_binaries` enumerates and invokes the deleted `./snapdir-*` scripts and
    reads `utils/qa-fixtures/`. Referenced by nothing but itself.
  - `utils/install.sh` — REMOVED (dead). It `wget`s the 8 deleted bash oracle
    scripts from GitHub and tells users to install `b3sum`/`sqlite3` runtime
    deps. The Rust port ships via cargo-dist's `installers = ["shell"]`
    (`packaging/dist-workspace.toml` + `release.yml`), so this bash-era installer
    is obsolete; referenced by no workflow/Makefile/doc.

- **Comments scrubbed (past-tense, design rationale preserved):**
  - `cli.rs`: module doc + `defaults`/`locations`/`id` docs — dropped the
    `./snapdir`-invocation form and "frozen oracle" phrasing → "reproduces the
    original `snapdir …`". (The `assert_eq!(…, "snapdir-gcs-store")` is a tested
    string value, not a comment — left as-is.)
  - `walk.rs`: module + `walk()` docs and the test-mod note → "the original
    `snapdir-manifest`"; the deleted-oracle note kept past-tense.
  - `catalog/lib.rs`: module doc, `json_compat` block, the `_golden` test doc and
    its inline locate-comment, and the `query_bytes` doc → past-tense; clarified
    the golden test self-skips now that the script is removed.
  - `catalog_logging.rs`: "The frozen Bash oracle logs…" → "The original Bash
    implementation logged…".
  - `store.rs`: sharding cross-check comment → dropped `./snapdir` form.
  - `compat_golden.rs`: module doc → past-tense; dropped the dead
    `utils/qa-fixtures/...` cross-reference (kept the `docs/rust-port` one).
  - `_typos.toml` `persit` comment already reads "now-deleted Bash oracle"
    (past-tense) — left unchanged.

- **Sha-locked files untouched:** confirmed via `shasum -c` (all three OK). No
  edits to `manifest.rs` / `merkle.rs` / `excludes.rs`, `docs/`, `.gatesmith/`,
  or production logic. All comment edits are doc/comment-only; build + 232 tests
  pass with 0 failures.

Ready for PM verification: YES
