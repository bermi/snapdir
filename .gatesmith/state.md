# Project state

> Derived from `.gatesmith/gates.yaml` — re-projected by the PM at the end of every
> tick. Do not edit by hand; edit `gates.yaml` instead.

Active phase: **8** (Phases 5–7 green except the operator-deferred B2 gate).
Next gate: `ci-coverage-floor` (phase 8, owner ci, deps coverage-gate ✓) — head of the priority queue (id-asc before `fuzz-parser`); then `fuzz-parser` (P8), `rustdoc-doctests` (P9). `ci-coverage-floor` = spawn ci to change `.github/workflows/ci.yaml` coverage job `--fail-under-lines 0 → 75` (so GitHub CI enforces the same floor the coverage-gate now checks; verify via `grep`).
`cargo-llvm-cov` 0.8.7 + `llvm-tools-preview` are now installed locally (PM, for coverage-gate). `cargo llvm-cov report --fail-under-lines N` reuses existing profile data (fast, no test re-run).
Last passed: `coverage-gate` @ 2026-06-01T14:10:14Z — **GATE-BUMP from hollow to real** (operator-approved "set a real floor" + 75%): was `echo`+`human_confirm` with CI `--fail-under-lines 0`; now `cargo llvm-cov --workspace --all-features --locked --fail-under-lines 75 --summary-only` (exit_code). Measured **79.43% lines** (≥75% → pass; floor 90 → exit1 proves it's real). Remote-store live paths uncovered hermetically (env-gated, as in CI). Contract frozen (locks 4/4 OK each tick).
GATE-OWNER-FIX pattern (this phase): gates verified by `cargo test -p <crate>` but nominally owned by `tests` are corrected to the crate's lane — `cli-trycmd → cli`, `proptest-roundtrip → core`. `fuzz-parser` (`tests/fuzz/...`, `cargo fuzz`) genuinely IS the `tests` lane (top-level `tests/`).
CROSS-LANE follow-up (cli): (a) wire `verify-cache`/`flush-cache` to `snapdir_core::cache` + call `check_snapshot_integrity` in `checkout`/`verify` (resolve `${XDG_CACHE_HOME:-$HOME/.cache}/snapdir`); (b) wire the `catalog` subcommands to `snapdir_catalog::Catalog`.

**Remote-interop is now PM-auto-verified, not a human rubber-stamp.** `remote-interop` runs `bash tests/integration/remote_stores_live.sh` every tick: MinIO S3 Bash↔Rust cross-tool (byte-identical) + a zero-external-dependency lane (Rust round-trip with `aws`/`b2`/`gcloud` removed from PATH). `remote-interop-gcs` proves the same against the **real** `gs://snapdir-integration-testing` bucket (ADC). Both green. `gcs-store-notfound-fix` repaired a real GcsStore bug — `key_exists`/`get_bytes` only treated HTTP 404 as absent, but `google-cloud-storage` v1.12 reports a missing object as service-level `Code::NotFound` (`http_status_code()==None`), so skip-if-present aborted **every** real GCS push; fixed via an `is_not_found()` helper.

**B2 deferred to last (`remote-interop-b2`, pending, depends on `release-dryrun`)** per the operator: verify B2 once we have a usable release candidate. OPERATOR PREREQ before it can pass: fix `SNAPDIR_B2_TEST_ENDPOINT` to the key's real region (key reports `us-west-001`; creds file says `us-west-004`) + a key with object HEAD/GET/PUT. `b2` CLI is installed (oracle side). No local emulator serves both B2-native + S3 APIs, so it needs the real sandbox.

**Lesson (gate-design principle):** a `human_confirm` must be paired with a real machine check wherever feasible — `remote-interop` was the only *hollow* gate (echo + lone human_confirm) and nearly produced a false pass. Remaining human gates legitimately need external systems: `ci-matrix-green` (GitHub Actions), `coverage-gate` (Codecov), `release-dryrun` (tag/CI); `perf-gate`/`migration-guide` already pair human_confirm with a real `exit_code` check.

Live-verification env (for the PM/loop): GCS needs only ambient ADC (`gcloud auth application-default login` as `bermi@bermilabs.com`, project `snapdir-development`) + the bucket path (hardcoded in the gate). S3 is hermetic (docker MinIO, no creds). B2 needs `source ~/.config/snapdir/test-creds.sh` + the endpoint fix above.
TLS pattern established for remote stores: `default-features=false` + ring rustls — keeps aws-lc-rs out. Re-run `grep -i aws-lc Cargo.lock` (must be empty) after each remote-store gate.
Open (non-blocking) findings to revisit later:
- `Store` trait lacks object copy/delete → fetch double-copies; `verify --purge` can't remove corrupt objects yet (verify-cache/cache-id or a store-API extension).
- FROZEN oracle macOS bug: `snapdir` L2163-2170 `_snapdir_absolute_path` can't checkout nested dirs on macOS (no `realpath -m`); Rust is correct. Bash-side macOS limitation only — not ours to fix (oracle is frozen).
- FROZEN oracle space-path bug: `snapdir` L1276 `IFS=' ' read -r -a line_parts` then `line_parts[4]` truncates space-bearing paths on Bash **push** (reproduces vs `file://`; Rust pushes spaces fine). Interop harness lane-C uses a no-space corpus for the Bash-push direction. Oracle is frozen — not ours to fix.

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
- Phase 3 (Interop keystone, HARD): 4/4 passed ✅ 🔑 KEYSTONE PROVEN
- Phase 4 (Store trait + FileStore): 5/5 passed ✅
- Phase 5 (Remote stores): 8/9 passed (S3+GCS interop PM-verified; only `remote-interop-b2` — operator-deferred to post-release-candidate — remains)
- Phase 6 (Caching + redb catalog): 4/4 passed ✅ (`cache-id` `catalog-redb` `catalog-compat` 🔒 `catalog-rebuild`)
- Phase 7 (Performance): 4/4 passed ✅ (`bench-scaffold` `bench-compile` `perf-harness` `perf-gate` — Rust 33.6×/2.69× faster, output byte-identical, operator-signed-off)
- Phase 8 (Testing/fuzzing): 3/5 passed (`cli-trycmd` ✅ `proptest-roundtrip` ✅ `coverage-gate` ✅ 75% floor; `ci-coverage-floor`/`fuzz-parser` remain)
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
- 2026-05-31 — `cli-manifest-wire`: `snapdir manifest`/`id` wired to `snapdir-core` (thin layer; flags→WalkOptions/Hasher; keyed via `SNAPDIR_MANIFEST_CONTEXT`); 7 integration tests byte-identical vs the live oracle. `snapdir id` is checksum-mode-independent (always b3sum), matching the oracle.
- 2026-05-31 — `interop-diff` 🔑 **KEYSTONE** (human checkpoint): full `tests/interop/run.sh` → 15/15 corpus cases byte-identical Bash↔Rust (manifests + snapshot IDs, all checksum/keyed/no-follow modes); operator signed off. **Byte-for-byte interoperability proven; Phase 3 complete.**
- 2026-05-31 — `store-trait`: `snapdir-core::store` — `Store` trait (`get_manifest`/`fetch_files`/`push`, sync/object-safe) + sharded path helpers confirmed vs oracle (`snapdir` L1387/1399); 8 tests. Frozen files untouched.
- 2026-05-31 — `file-store`: `FileStore` (`file://`) impl of the `Store` trait — sharded `.objects`/`.manifests`, push (objects-before-manifest, skip-if-present), fetch (temp + verify BLAKE3 + retry≤5 + atomic rename); 10 tests. Mirrors `./snapdir-file-store`.
- 2026-05-31 — `external-store-shim`: `router.rs` (scheme→adapter, `gs`→`gcs` hardcoded, mirrors `snapdir` L1328-1370) + `shim.rs` `ExternalStore` (emit-command contract for third-party `snapdir-*-store` binaries; built-ins stay in-process); 10+5 tests w/ a mock store. → added `cli-store-wire` prereq.
- 2026-05-31 — `cli-store-wire`: `snapdir push/fetch/checkout/pull/verify` wired to FileStore+core (router-resolved `file://`); checkout restores perms → dest re-manifests to identical id; 2 `store_roundtrip` integration tests.
- 2026-05-31 — `file-store-roundtrip`: `tests/integration/file_store_roundtrip.sh` (15 assertions) — Rust e2e push/fetch/checkout/pull/verify + cross-tool Rust↔Bash both read each other's `file://` stores byte-identically. **Phase 4 complete.** (Surfaced a frozen-oracle macOS nested-checkout bug — not ours.)
- 2026-05-31 — `s3-store`: `S3Store` via `aws-sdk-s3` with **ring-only** rustls (aws-lc-rs kept out of Cargo.lock); `s3://bucket/prefix` + core sharded keys; push/fetch discipline; AWS cred chain via `aws-config`; sync↔async bridge via owned tokio rt; 12 tests (live gated behind env).
- 2026-05-31 — `b2-store`: `B2Store` = thin wrapper over `S3Store` at Backblaze's S3-compatible custom endpoint (`b2://` parse == `s3://` per oracle); no new deps; 12 tests.
- 2026-05-31 — `gcs-store`: `GcsStore` via `google-cloud-storage =1.12.0`, ring-only (eliminated google-cloud-auth's aws-lc-rs defaults + installed ring CryptoProvider); `gs://` parse matches oracle; ADC auth; 13 tests. aws-lc/openssl/native-tls all absent. → added `remote-stores-harness` prereq.
- 2026-05-31 — `remote-stores-harness`: `tests/integration/remote_stores.sh` (per-backend Rust roundtrip + Bash↔Rust cross-tool; emulator-free `--self-check` w/ skip-not-fail). Surfaced CLI `resolve_store` only wires `file://` → added `cli-remote-store-wire` prereq.
- 2026-05-31 — `cli-remote-store-wire`: CLI `resolve_store` now routes `file/s3/b2/gs` to their stores + `ExternalStore` shim for other schemes (via `snapdir_stores::resolve_adapter`); 4 creds-free routing tests. Remote push/fetch now reachable.
- 2026-06-01 — `remote-interop` **GATE-BUMP** (operator-approved): the hollow `echo`+`human_confirm` rubber-stamp was replaced with a real PM-run differential harness `tests/integration/remote_stores_live.sh` — MinIO S3 Bash↔Rust cross-tool (byte-identical) + a **zero-external-dependency** lane (Rust round-trip with `aws`/`b2`/`gcloud` removed from PATH). Hermetic, runs every tick. Caught (and prevented) a near-false-pass; now PM-auto-verified. (git fd17c22)
- 2026-06-01 — `gcs-store-notfound-fix` (stores): repaired a real GcsStore bug surfaced by the live harness — `key_exists`/`get_bytes` only treated HTTP 404 as absent; `google-cloud-storage` v1.12 reports a missing object as service-level `Code::NotFound` (`http_status_code()==None`), so skip-if-present aborted **every** real GCS push before upload. Fixed via `is_not_found()` (HTTP 404 OR service NotFound), mirroring S3; 3 regression tests; live GCS round-trip now passes. (git f08cb84)
- 2026-06-01 — `remote-interop-gcs` (tests): GCS Bash↔Rust cross-tool against the **real** bucket `gs://snapdir-integration-testing` (ADC) — Rust round-trip + Rust-push→Bash(`gcloud`)-fetch + Bash-push→Rust-fetch all byte-identical, same snapshot id `e52681b9…` every direction. **Phase 5 remote interop proven for S3+GCS.** B2 deferred to `remote-interop-b2` (post-release-candidate). (git fd17c22)
- 2026-06-01 — `cache-id` (core): new library-pure `snapdir-core::cache` — `check_snapshot_integrity` (mirrors `_snapdir_check_integrity`: manifest-present + every file object verified vs its content-address), `verify_cache(purge)` (mirrors `verify-cache`: scan `.objects/*/*/*/*`, recompute blake3, compare to path-encoded expected checksum, purge corrupt), `flush_cache`; reuses the frozen sharded helpers + in-process blake3, no `$HOME`/env/IO; 10 tests, tamper case cross-checked vs live `./snapdir verify-cache --purge`. Frozen files untouched. (git 3763bd3)
- 2026-06-01 — `catalog-redb` (catalog): redb-backed catalog (`redb =4.1.0`, pure-Rust, no sqlite/aws-lc) replacing the stub — fixed range scans (no SQL planner): `loc_head` → `locations` + save's previous_id (O(1)), `by_location` reverse-range → `revisions` DESC, `by_id` reverse-range → `ancestors` DESC; `created_at YYYY-MM-DD HH:MM:SS.SSS` (lexical=chronological) + monotonic seq; injectable `Clock`; `save` no-ops when head==id; field sets match the oracle SQL `json_object`. 8 tests. (git b63125f)
- 2026-06-01 — `catalog-compat` (catalog): JSON-line serialization (serde/serde_json) matches the oracle's sqlite `json_object` **byte-for-byte** — compact, exact key order, `revisions` without `location`, `previous_id` bare `null`; proven by a live-`sqlite3` golden test that drives the frozen `./snapdir-sqlite3-catalog` and neutralizes `NOW()` by parsing the oracle's own `created_at`. 6 `json_compat` tests. **Catalog JSON shapes CLI-compat FROZEN.** (git f7025b1)
- 2026-06-01 — `catalog-rebuild` (catalog): `Catalog::rebuild(location, RebuildEntry{id,created_at})` — store-agnostic (no `snapdir-stores` dep), clock-free: sorts the store's recovered entries by `created_at`, clears the location across all 4 redb tables, replays through `save`'s insert path to reconstruct `previous_id` + head==id dedup; touches only that location; idempotent. Recoverability boundary documented (store yields id-set + `created_at`; `previous_id` re-derived by chronological order). 6 rebuild tests incl. byte-identical round-trip. **Phase 6 complete.** (git d72913c)
- 2026-06-01 — `bench-scaffold` (ci): registered the top-level `benches` workspace member (`snapdir-benches`, criterion 0.7 no-aws-lc) + minimal real `hot_paths` `[[bench]]`, fixing `bench-compile`'s vacuous-pass risk (`cargo build --benches` was building zero targets). (git 26b89d5)
- 2026-06-01 — `bench-compile` (bench): 3 criterion hot-path groups — `hash/blake3/{64B,4K,64K,1M}` (Throughput::Bytes), `walk/{many_small,few_large}` via `walk()`+`Blake3Hasher`, `manifest/{emit,parse}/{1k,10k}`; `tempfile` corpora, deterministic, self-cleaning; smoke-run e.g. blake3/1M ~1.43 GiB/s. Benches only measure (output bytes unchanged). (git 06ff564)
- 2026-06-01 — `perf-harness` (bench): `benches/compare.sh` — Rust-vs-Bash `manifest` comparison that asserts **byte-identical output first** (hard-fail on drift) then times both (hyperfine if present, else a portable median-of-N `EPOCHREALTIME` fallback — hyperfine is absent here) over many-small + few-large corpora; jq JSON report for `perf-gate`; `--self-check`. Full run: **~33× (many-small), ~2.7× (few-large), output byte-identical**; perf win is pure in-process walk+BLAKE3 (no core change needed). (git ac688e5)
- 2026-06-01 — `perf-gate` (bench, **human checkpoint**): operator signed off after the PM ran `bash benches/compare.sh` — byte-identical output both corpora, **33.6× many-small / 2.69× few-large**. **Phase 7 complete.** (git ac688e5)
- 2026-06-01 — `cli-trycmd` (cli; owner-fixed from `tests`): 29 `trycmd` surface snapshots (help/version, all 14 subcommand `--help`, parse + missing-arg errors, the 7 stubs' real "not implemented yet") + 6 `assert_cmd`/`assert_fs` e2e (`id`/`manifest` byte-equal the oracle, `push→fetch→checkout→verify` file:// round-trip). Honest wired-vs-stub coverage map; no oracle divergence. (git 9218c33)
- 2026-06-01 — `proptest-roundtrip` (core; owner-fixed from `tests`): `tests/proptest_roundtrip.rs` — 3 proptests ×1024 cases proving manifest `parse_line(Display)==entry`, fields verbatim (paths keep spaces), `Manifest from_entries→Display→parse` modulo `sort -k5`; strategy constraints derived from real `parse_line`/`Display`. Frozen files untouched; no parse/emit asymmetry. (git 3690282)
- 2026-06-01 — `coverage-gate` (**GATE-BUMP**, operator-approved): hollow `echo`+`human_confirm` (CI `--fail-under-lines 0`) → real `cargo llvm-cov --fail-under-lines 75` machine check. PM installed cargo-llvm-cov 0.8.7, measured **79.43% workspace line coverage** (core/catalog/cli high; s3/gcs/b2 low — live-gated). Floor 75 set (operator value); proven real (90→exit1, 75→exit0). CI parity → `ci-coverage-floor`. (git f7db332)

## Remote-store test credentials (operator)

Before the `remote-interop` gate (and any live remote-store run), source the creds:

    source ~/.config/snapdir/test-creds.sh

Then run: `bash tests/integration/remote_stores.sh` (S3/B2/GCS round-trips +
Bash<->Rust cross-tool). Creds live in `~/.config/snapdir/test-creds.sh` (chmod 600,
outside the repo). They are TEST-bucket keys; rotate after use. To scrub the keys from
Claude transcripts/logs after a session ends: `bash ~/.config/snapdir/scrub-test-creds-from-logs.sh`.
