# tests handoff for remove-bash-test-harnesses @ 2026-06-03T13:37:19Z

## Summary

Deleted the 4 Bash differential test harnesses and removed all now-dead
oracle-shelling / skip-branch test code, converting the valuable coverage to
pure-Rust golden assertions so `cargo test --workspace --locked` is green WITHOUT
losing coverage. The deleted oracle (root `snapdir*` scripts) is gone, so every
`oracle_bin()` / `run_oracle()` / `fn oracle(name) -> Option<PathBuf>` helper and
every `if oracle absent { eprintln!("skip: …"); return; }` branch was permanently
dead and has been removed.

The byte-format contract these tests used to enforce against the live oracle is
already owned by `crates/snapdir-core/tests/compat_golden.rs` (recorded oracle
constants). Where a test contributed unique coverage beyond pure byte-diffing
(walk semantics; CLI-subcommand flag wiring; cache/catalog behavior), it was
**converted to an embedded golden constant or a structural pure-Rust assertion**,
not deleted. Fixtures are now built with **explicit fixed permissions** (dirs
`0o700`/`0o755`, files `0o600`/`0o644`) so the `TYPE PERMS CHECKSUM SIZE PATH`
output is fully deterministic and pinnable; every golden checksum/merkle value
was cross-checked against the recorded oracle vectors in `compat_golden.rs`.

### How the 9+ walk tests were converted (`crates/snapdir-core/src/walk.rs`, test module only)

Removed `oracle_bin()`, `checksum_bin_available()`, `run_oracle()`,
`assert_matches_oracle()`. `Scratch` now chmods the root to a fixed `0o755`;
`write_file` chmods files to `0o600`; new `chmod_dirs()` pins every dir to a fixed
mode. Each test:

- `walk_empty_directory_golden` — **full golden constant** (`D 755 af1349b9… 0 ./`).
- `walk_single_empty_file_golden` — **full golden constant** (root `D` + empty `F`).
- `walk_nested_tree_relative_golden` — **full golden constant** (`NESTED_RELATIVE_GOLDEN`,
  the deep guide tree; checksums match `compat_golden.rs::MULTILEVEL_MANIFEST`).
- `walk_nested_tree_absolute_golden` — **derived golden**: rewrites the relative
  golden's `./` prefix to the absolute root, proving only the path column changes.
- `walk_directory_size_is_sum_of_members_golden` — **full golden constant** PLUS the
  structural `sub=8`, `root=13` size-sum assertions (kept).
- `walk_symlink_followed_by_default` — **structural** (symlink-perm column is
  platform-dependent: macOS `755` vs Linux `777`): asserts `./a_link/` mirrors
  `./a/`'s merkle, the target subtree materializes, and `./r1f_link` carries the
  followed file's content checksum.
- `walk_no_follow_drops_symlinks` — **full golden constant** over the real entries
  (no symlink rows, so no platform-dependent perm column) + `!contains("_link")`.
- `walk_exclude_regex_golden` — **full golden constant** over the survivors + `drop/`
  absence assertion.
- `walk_exclude_common_golden` — **full golden constant** (only `./src/` survives) +
  `.git`/`node_modules` absence assertions.
- `walk_snapshot_id_is_blake3_of_manifest_text` (was `…_matches_oracle_id_derivation`)
  — **structural**: recomputes `blake3(manifest_text + "\n")` in-test and asserts the
  public `snapshot_id` equals it (64 lowercase hex).
- `walk_root_must_be_absolute` — unchanged (never shelled out).

## Files changed

```
 crates/snapdir-cli/tests/cache_commands.rs         |  41 +-
 crates/snapdir-cli/tests/catalog_commands.rs       |  98 +--
 crates/snapdir-cli/tests/defaults_command.rs       |  94 +--
 crates/snapdir-cli/tests/e2e.rs                    |  63 +-
 crates/snapdir-cli/tests/manifest.rs               | 256 ++++----
 crates/snapdir-core/src/walk.rs                    | 443 +++++++------
 crates/snapdir-stores/tests/shim_external_store.rs |   2 +-
 crates/snapdir-stores/tests/snapdir-mock-store     |   2 +-
 tests/integration/file_store_roundtrip.sh          | 336 ---------- (deleted)
 tests/integration/remote_stores.sh                 | 557 ---------------- (deleted)
 tests/integration/remote_stores_live.sh            | 728 --------------------- (deleted)
 tests/interop/run.sh                               | 575 ---------------- (deleted)
 (test files only; 418 insertions, 2783 deletions)
```

(`.claude/ralph-loop.local.md` + `.claude/scheduled_tasks.lock` in `git status` are
pre-existing harness state churn — NOT touched by this lane.)

### Per-CLI-file actions
- `e2e.rs`: removed `repo_root()`/`oracle()`; converted
  `id_is_64_lowercase_hex_and_matches_oracle` → `id_is_64_lowercase_hex` (kept the
  pure-Rust hex assertion, dropped the oracle branch); **deleted**
  `manifest_matches_oracle_bytes` (pure differential, covered by `compat_golden.rs`).
  All `file://` push/fetch/checkout/pull round-trips KEPT.
- `cache_commands.rs`: removed `repo_root()`/`oracle()`; **deleted**
  `cache_commands_stage_id_matches_oracle` (pure differential). KEPT purge/flush
  behavior + `.objects`/`.manifests` sharded-key existence assertions.
- `manifest.rs`: removed `oracle()`/`run_oracle()`; **converted all 7** oracle-diff
  tests to pure-Rust golden constants driving the real `snapdir` binary
  (`--absolute`, `--checksum-bin md5sum/sha256sum`, `--exclude`, keyed
  `SNAPDIR_MANIFEST_CONTEXT`, and `id`) — preserves the CLI flag-wiring coverage.
- `catalog_commands.rs`: removed `repo_root()`/`oracle()`; **deleted**
  `catalog_commands_revisions_line_matches_oracle_sqlite_catalog` (the JSON shape is
  pinned directly by the surviving `id`/`previous_id`/`location` field assertions).
- `defaults_command.rs`: removed `repo_root()`/`oracle()`; **deleted**
  `defaults_command_matches_oracle_under_controlled_env`, replaced with
  `defaults_command_reformats_store_env_var` (pins the same `--cache-dir=…` +
  `--store=…` env-reformat lines as pure-Rust assertions).
- `shim_external_store.rs` + `snapdir-mock-store`: scrubbed the lone
  `./snapdir-file-store` token from doc comments (generic third-party-protocol
  wording); the ExternalStore shim test itself is unchanged and green.

## Local verification result

`! ls tests/interop/run.sh tests/integration/remote_stores.sh tests/integration/remote_stores_live.sh tests/integration/file_store_roundtrip.sh 2>/dev/null`
→ exit 0 (all four harnesses gone).

`cargo build --workspace --locked` → Finished, 0 errors/warnings.
`cargo clippy --workspace --tests --locked` → 0 warnings.

`cargo test --workspace --locked` summary (aggregated across all test binaries +
doc-tests):

```
TOTAL passed=232 failed=0 ignored=0
```

walk module specifically: `test result: ok. 11 passed; 0 failed`.
stores shim: `test result: ok. 5 passed; 0 failed`.

## Reuse check / Blockers

- **sha-locked files untouched**: `git diff --name-only HEAD` shows NO
  `crates/snapdir-core/src/{manifest,merkle,excludes}.rs`. (The `manifest.rs` in
  the diff is `crates/snapdir-cli/tests/manifest.rs`, a test file in scope.)
- **walk.rs production code byte-identical**: lines 1–428 (everything above
  `#[cfg(test)] mod tests`) diff clean vs `HEAD`; only the test module changed.
- **No production (non-`#[cfg(test)]`) code touched** anywhere — only test code
  and the deleted `.sh` harnesses + the test-only mock-store script comment.
- **walk tests CONVERTED, not just deleted**: 8 full/derived golden constants +
  3 structural pure-Rust assertions (symlink-follow, snapshot-id, dir-size-sum),
  preserving every original coverage case (empty dir, single empty file, nested
  relative+absolute, dir-size=sum, symlink-followed, no-follow-drops, exclude
  regex, exclude %common%). Coverage floor (75%) preserved — nothing dropped.
- **oracle() helpers + skip branches removed** from all 5 CLI test files.
- **`./snapdir` / `./snapdir-manifest` / `snapdir-*-store` tokens scrubbed** from
  the touched test files (verified by grep; only descriptive `oracle` prose
  remains where it's not a bash-reference literal).
- **Left for `no-bash-references-remain` (not mine):**
  - `crates/snapdir-core/src/store.rs:248` — a `// matching … ./snapdir` PRODUCTION
    comment (out of my test-only scope; flagged per instructions).
  - `crates/snapdir-cli/tests/catalog_logging.rs` — contains descriptive
    comments referencing the oracle's line numbers (`snapdir` L212/L223/L826).
    It does NOT shell out to any oracle and has no `./snapdir`/`snapdir-*-store`
    literal token, so it was out of this gate's exact scope; left as-is.

Ready for PM verification: YES
