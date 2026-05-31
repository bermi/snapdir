# Project state

> Derived from `.gatesmith/gates.yaml` — re-projected by the PM at the end of every
> tick. Do not edit by hand; edit `gates.yaml` instead.

Active phase: **3**
Current gate: `cli-manifest-wire` (phase 3, owner cli) next tick.
Last passed: `core-walk` @ 2026-05-31T10:58:43Z.
Keystone path: `interop-diff` (HARD, human checkpoint) BLOCKED until `cli-manifest-wire` wires `snapdir manifest`/`id` to the now-real core walk. Sequence: ✅core-walk → cli-manifest-wire → interop-diff.
For cli-manifest-wire (from core-walk handoff): the CLI must resolve root to an absolute path before `walk`, build the `ExcludeMatcher` from `expand_excludes(...)`, set `NoFollow` when `forces_no_follow` or `--no-follow`. `snapdir-core::walk(root, &WalkOptions{follow,path_mode,exclude}, &hasher)` + `snapshot_id` are the entry points. Oracle `--no-follow` is path-first only.

> **🔒 FROZEN INTERFACES — re-verify EVERY tick (READ STATE step):**
> `shasum -a 256 -c .gatesmith/golden-fixtures.sha.lock .gatesmith/manifest-format.sha.lock`
> Both must report OK. A mismatch = CRITICAL ALERT → `AskUserQuestion`, do nothing else.
> Locks cover: `utils/qa-fixtures/expected-guide-commands.txt` (golden corpus) and the
> format-defining core source (`manifest.rs`/`merkle.rs`/`excludes.rs`). Any change to
> line format / ordering / checksum algorithm / sharding / exclude sets now needs a
> human-approved frozen-interface mutation (`## Proposal` → AskUserQuestion).

> **⚠ CONTRACT DISCREPANCY (escalated to human @ 2026-05-31T01:55:16Z).** The oracle (`snapdir` L259/762/436/776) derives the snapshot ID as `manifest | grep -v '^#' | b3sum --no-names` — BLAKE3 of the **full manifest text** (incl. trailing newline), NOT the root directory checksum. PLAN.md's frozen-contract line "Root dir checksum = snapshot ID", the `dir-merkle` gate description, and `merkle.rs` doc comments + the `snapshot_id_equals_root_directory_checksum` test all encode the doc bug. The `directory_checksum` function is correct (it computes the `D ./` line's CHECKSUM field); only the "= snapshot id" labeling is wrong. golden-b3's tests use the correct derivation (ids c678a299…/8af03a1b…) + a guard test. **Must be corrected before freeze-contract** so the frozen contract and keystone interop gate key on the real snapshot ID.

## Phase summary

- Phase 0 (Bootstrap): 1/1 passed ✅
- Phase 1 (Scaffolding + CI): 6/6 passed ✅
- Phase 2 (Core manifest/hashing + FREEZE): 7/7 passed ✅ 🔒 FROZEN
- Phase 3 (Interop keystone, HARD): 2/4 passed (cli-manifest-wire + interop-diff remain)
- Phase 2 (Core manifest/hashing + FREEZE): 0/5 passed
- Phase 3 (Interop keystone): 0/2 passed
- Phase 4 (Store abstraction + FileStore): 0/4 passed
- Phase 5 (Remote stores): 0/4 passed
- Phase 6 (Caching + redb catalog): 0/4 passed
- Phase 7 (Performance): 0/2 passed
- Phase 8 (Testing/fuzzing): 0/4 passed
- Phase 9 (Documentation): 0/2 passed
- Phase 10 (Packaging/release): 0/2 passed

## Recent milestones

- 2026-05-31 — Phase 0 complete: plan vendored, gatesmith ledger + lane templates authored; bootstrap verified green.
- 2026-05-31 — `scaffold-workspace`: Cargo workspace (core/catalog/stores/cli) + pinned toolchain 1.96.0 builds clean (`cargo build --workspace --locked`); ring TLS stance, no aws-lc-rs.
- 2026-05-31 — `ci-config`: ci.yaml (lint/deny/test-matrix incl. musl static + coverage + semver), deny.toml (aws-lc-rs banned), _typos.toml authored; actionlint clean.
- 2026-05-31 — `clippy-pedantic-clean`: workspace pedantic lints (warn, prio -1) wired + all crates opt in; `cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0.
- 2026-05-31 — `cli-skeleton`: clap v4 derive `snapdir` binary exposes all 14 subcommands + global options (stubbed), pinned to the `./snapdir` oracle; help-surface regex passes.
- 2026-05-31 — `fmt-clean`: `cargo fmt --all --check` clean across the workspace (rustfmt.toml stable-compatible).
- 2026-05-31 — `ci-matrix-green` (human checkpoint): operator confirmed the full GitHub Actions matrix (Linux/macOS/Windows × MSRV/stable/beta + musl static) green on `rust-port`. **Phase 1 complete.**
- 2026-05-31 — `manifest-format`: `snapdir-core` manifest line model (`Manifest`/`ManifestEntry`) — Display `TYPE PERM CHECKSUM SIZE PATH`, sort -k5, `#`-comment/empty-line stripping, `./` vs `--absolute`; 15 unit tests, pinned to `./snapdir-manifest`.
- 2026-05-31 — `dir-merkle`: `directory_checksum` = sort -u + concat(no separator) + rehash of child checksums via in-process `blake3` crate (no b3sum shell-out); `Hasher` trait seam for `--checksum-bin`; 7 checksum tests. (Note: its "root checksum = snapshot id" claim is the doc bug now under escalation.)
- 2026-05-31 — `golden-b3`: 8 `golden_b3sum` tests reproduce the frozen ids byte-for-byte in-process (empty af1349b9, foo 49dc870d, root D-line dba5865c/4a0732cf, snapshot ids c678a299/8af03a1b). **Surfaced the snapshot-ID contract discrepancy (escalated).**
- 2026-05-31 — `snapshot-id-core-fix` (remediation): added `snapdir_core::snapshot_id(manifest, hasher)` = BLAKE3 of `Display` text + trailing `\n` (oracle-exact), reproducing c678a299…/8af03a1b…; relabeled `merkle.rs` docs + replaced the misleading test. Contract discrepancy resolved in core.
- 2026-05-31 — `golden-multi`: `Md5Hasher`/`Sha256Hasher`/`Blake3KeyedHasher` (`md-5`/`sha2`/`blake3 derive_key`, no shell-out) + `excludes.rs` (`%system%`/`%common%` sets verbatim from oracle, regex matcher, `FollowMode`); 19 tests. (verification_cmd GATE-BUMP-fixed from a hollow 0-test filter.)
- 2026-05-31 — `snapshot-id-doc-fix`: corrected PLAN.md frozen-contract (snapshot ID = b3sum of `#`-stripped manifest text, not the root dir checksum). Snapshot-ID discrepancy fully resolved (core + docs) ahead of the freeze.
- 2026-05-31 — `freeze-contract` (human checkpoint): operator approved the FREEZE. Manifest format + dir-merkle + snapshot-id + checksum modes + excludes + golden fixtures LOCKED via `.gatesmith/*.sha.lock`. **🔒 Phase 2 complete; the contract is now immutable without human approval.**
- 2026-05-31 — `interop-harness`: `tests/interop/run.sh` differential harness built (deterministic corpus, byte-identical Bash↔Rust diff across all checksum/keyed/no-follow modes); `--self-check` green. Flagged that interop-diff needs the core walk + CLI wiring first → added `core-walk` + `cli-manifest-wire` prereq gates.
- 2026-05-31 — `core-walk`: in-process FS walk (`src/walk.rs`) → frozen-format Manifest; 10 tests diff byte-for-byte vs the live `./snapdir-manifest` (b3/md5/sha256, symlink follow/no-follow, excludes). Matched the oracle's lstat-perms/target-checksum symlink rule. Frozen files untouched.
