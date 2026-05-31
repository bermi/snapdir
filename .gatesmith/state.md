# Project state

> Derived from `.gatesmith/gates.yaml` — re-projected by the PM at the end of every
> tick. Do not edit by hand; edit `gates.yaml` instead.

Active phase: **2**
Current gate: `dir-merkle` (phase 2, owner core) next tick.
Last passed: `manifest-format` @ 2026-05-31T01:46:33Z.
Blocked: none. (Phase 2 ends at `freeze-contract`, which freezes the manifest spec + golden fixtures.)

## Phase summary

- Phase 0 (Bootstrap): 1/1 passed ✅
- Phase 1 (Scaffolding + CI): 6/6 passed ✅
- Phase 2 (Core manifest/hashing + FREEZE): 1/5 passed
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
