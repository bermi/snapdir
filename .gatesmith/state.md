# Project state

> Derived from `.gatesmith/gates.yaml` — re-projected by the PM at the end of every
> tick. Do not edit by hand; edit `gates.yaml` instead.

Active phase: **5**
Current gate: `remote-interop` (phase 5, owner tests, **HUMAN CHECKPOINT**) next tick.
Last passed: `cli-remote-store-wire` @ 2026-05-31T13:09:48Z — CLI now routes s3/b2/gs/external. 🔑 interop keystone proven; contract frozen (locks 4/4 OK each tick).
remote-interop is fully unblocked. Next tick: escalate to the operator to run `bash tests/integration/remote_stores.sh` against emulators (MinIO / B2 sandbox / fake-gcs-server) with the env contract below, and confirm Bash↔Rust cross-tool round-trips pass with identical keys/ids.
`remote-interop` operator env contract: S3 `SNAPDIR_S3_TEST_STORE`+`SNAPDIR_S3_TEST_ENDPOINT`+AWS creds; B2 `SNAPDIR_B2_TEST_STORE`+`SNAPDIR_B2_TEST_ENDPOINT`+`SNAPDIR_B2_STORE_APPLICATION_KEY`/`_ID`; GCS `SNAPDIR_GCS_TEST_STORE`+`STORAGE_EMULATOR_HOST`. Memory holds a real GCS test project/bucket.
TLS pattern established for remote stores: `default-features=false` + aws-smithy-runtime `connector-hyper-0-14-x` + hyper-rustls/rustls 0.21/ring — keeps aws-lc-rs out. Re-run `grep -i aws-lc Cargo.lock` (must be empty) after each remote-store gate.
Eligible besides Phase 5: cache-id+catalog-redb (P6), bench-compile (P7), proptest-roundtrip+cli-trycmd (P8), rustdoc-doctests (P9) — phase-asc keeps Phase 5 first.
Open (non-blocking) findings to revisit later:
- `Store` trait lacks object copy/delete → fetch double-copies; `verify --purge` can't remove corrupt objects yet (verify-cache/cache-id or a store-API extension).
- FROZEN oracle macOS bug: `snapdir` L2163-2170 `_snapdir_absolute_path` can't checkout nested dirs on macOS (no `realpath -m`); Rust is correct. Bash-side macOS limitation only — not ours to fix (oracle is frozen).

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
- Phase 5 (Remote stores): 5/6 passed (only remote-interop — a human checkpoint — remains)
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
