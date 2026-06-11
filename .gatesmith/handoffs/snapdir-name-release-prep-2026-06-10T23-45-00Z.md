# Handoff — snapdir-name-release prep (phase 26, code prep for `snapdir-name-release-1.5.1`)

**Teammate:** packaging · **Date:** 2026-06-11 · **Gate:** `snapdir-name-release-1.5.1`
(shipping as **VERSION 1.6.0** per PM decision — design (b) moved the binary out
of snapdir-cli, too big for a patch)

## What was done

1. **Version bump 1.5.0 → 1.6.0** (`Cargo.toml`, `Cargo.lock`,
   `crates/snapdir/Cargo.toml`):
   - root `[workspace.package] version = "1.6.0"`;
   - the THREE internal path-dep versions in `[workspace.dependencies]`
     (snapdir-core/-catalog/-stores) → `1.6.0`;
   - `crates/snapdir/Cargo.toml`'s direct path+version dep
     `snapdir-cli = { path = "../snapdir-cli", version = "1.6.0" }` (it is NOT
     in workspace.dependencies by design — cargo-shear);
   - `Cargo.lock` regenerated (`cargo update --workspace`: all 7 workspace
     packages 1.5.0 → 1.6.0, zero external dep changes).

2. **CHANGELOG cut** (`docs/rust-port/CHANGELOG.md`):
   - `## [Unreleased]` → `## [1.6.0] — 2026-06-11` (em-dash style, today's
     date); fresh empty `## [Unreleased]` above it;
   - verified the snapdir-crate entries from the docs gate are present under
     1.6.0: Added (`snapdir` crate / `cargo install snapdir`) + Changed
     (`snapdir-cli` is now the implementation library, ≤ 1.5.0 unaffected);
   - footer: `[Unreleased]: compare/v1.6.0...HEAD`, new
     `[1.6.0]: compare/v1.5.0...v1.6.0`.

3. **`.github/workflows/release.yml`** — diff summary:
   - **env**: `CLI_CRATE: snapdir-cli` → `BIN_CRATE: snapdir` (with a comment:
     the `snapdir` shim package owns the `[[bin]]`; `snapdir-cli` is the
     implementation lib with no binary). `BIN_NAME: snapdir` unchanged.
   - **gen-assets**: `cargo run -p "${BIN_CRATE}"` for completions + man
     (the shim delegates to `snapdir_cli::run()`, so `completions`/`man`
     still work); header comment updated.
   - **build legs (cross + native)**: `-p "${BIN_CRATE}" -p
     "${SSH_STORE_CRATE}" --bins` — the `snapdir` bin now builds from the
     `snapdir` package; the two ssh-store bins unchanged. Staging/archive
     steps untouched (they key off `BIN_NAME`/`SSH_STORE_BINS`).
   - **crates-io publish loop**: now SIX crates, `snapdir` added LAST —
     `snapdir-core snapdir-catalog snapdir-stores snapdir-ssh-store
     snapdir-cli snapdir` (snapdir depends on snapdir-cli's lib, which
     depends on everything else). Skip-if-published guard shape IDENTICAL
     (sparse-index curl + grep vers; `snapdir` is 7 chars so the >=4-char
     shard rule still holds — comment updated four→six).
   - **NEW-CRATE operator comment rewritten** (job header + in-loop note):
     `snapdir` is NEW at 1.6.0; its FIRST publish will **403 under TP** (TP
     tokens cannot create new crates; this job auths exclusively via the TP
     OIDC exchange, no token fallback). Documented operator flow: let the loop
     publish the five existing crates and 403 on `snapdir` → one manual
     `cargo publish -p snapdir --locked --no-verify` with a scoped API token →
     re-run the job (idempotent skip-guard turns it green) → IMMEDIATELY
     register TP for `snapdir` on the crate page. Precedent note:
     snapdir-ssh-store bootstrap at 1.5.0; TP registered for all five
     previously-published crates.

4. **`packaging/dist-workspace.toml`**: no functional change needed — dist
   auto-detects `[[bin]]` targets and there were no explicit package/bin
   references; updated the two stale comments (primary bin now in
   `crates/snapdir`, build flags `-p snapdir -p snapdir-ssh-store --bins`,
   publish order ... -> cli -> snapdir).

## Verification

- `grep -qE '^version = "1.6.0"' Cargo.toml && grep -q '## \[1.6.0\]'
  docs/rust-port/CHANGELOG.md && cargo build --workspace --locked` — PASS.
- `cargo test --workspace --locked` — full suite GREEN, zero failures (incl.
  trycmd + loopback suites). No test pins a literal version string (grepped
  `1.5.0` across all `crates/**/*.rs` + `benches/**/*.rs`: zero hits — they
  use `env!(CARGO_PKG_VERSION)`).
- `actionlint .github/workflows/release.yml` — clean.
- `typos` on all changed files — clean.
- Sanity greps: no `1.5.0` left in any Cargo.toml; no release.yml line builds
  or runs the bin from `snapdir-cli` (remaining `snapdir-cli` mentions are the
  publish-loop entry + explanatory comments, all intentional).

## Changed files (CODE-ONLY, no .gatesmith)

- `Cargo.toml`, `Cargo.lock` (version bump)
- `crates/snapdir/Cargo.toml` (snapdir-cli dep 1.5.0 → 1.6.0)
- `docs/rust-port/CHANGELOG.md` (1.6.0 cut)
- `.github/workflows/release.yml` (BIN_CRATE, 6-crate loop, TP comment)
- `packaging/dist-workspace.toml` (comments only)

## Blockers / flags for PM

1. **Stale "1.5.1" in two OUT-OF-LANE docs files** (docs lane wrote them
   before the 1.6.0 decision — NOT edited by me):
   - `README.md:30` — "from 1.5.1 the binary lives in the `snapdir` crate"
   - `crates/snapdir-cli/README.md:22` — "From 1.5.1 this crate installs..."
   Both should say 1.6.0 before the release commit; one-line fixes for the
   docs teammate (or PM) — `crates/snapdir-cli/README.md` ships ON the
   crates.io page, so it matters.
2. **Release-time operator step stands** (also in the gate description):
   first `snapdir` publish 403s under TP → manual publish → re-run → register
   TP. The loop is ordered + idempotent so the re-run is safe.
3. Gate `verification_cmd` accepts `v1.5.1|v1.6.0` tags, so the 1.6.0 tag
   satisfies it as-is. The eventual PR title should read
   "release: 1.6.0 — cargo install snapdir".

Nothing committed, per instructions — the PM commits.
