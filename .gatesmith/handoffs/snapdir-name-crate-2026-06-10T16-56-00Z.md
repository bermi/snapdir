# cli handoff for snapdir-name-crate @ 2026-06-10T16:56:00Z

## Summary

**Design (b) shipped — the bin moved home.** `cargo install snapdir` now installs
THE snapdir binary from the new flagship crate `crates/snapdir`; `snapdir-cli`
became the implementation library behind it.

Design (a) was killed by the collision experiment (below): cargo emits an
"output filename collision" **warning** (exit 0, last-writer-wins) when two
workspace packages both define a `snapdir` bin — any warning is unacceptable
for this repo's zero-warning CI, so only ONE crate may own the bin target.

What changed:

- **`crates/snapdir-cli` → lib crate.** `src/main.rs` deleted; new `src/lib.rs`
  exposes `pub fn run() -> ExitCode` whose body is the old `main` moved
  VERBATIM (same `Cli::parse()`, same `{err:#}` stderr mapping, same
  SUCCESS/FAILURE exit codes — behavior-preserving by construction). `cli`/
  `progress` stay **private** modules, so `run()` is the entire public surface;
  the lib doc + README explicitly mark it "binary entrypoint, not a stable
  API" to keep semver expectations narrow. `[[bin]]` removed from its
  Cargo.toml (with a comment explaining why) and the description updated.
- **New crate `crates/snapdir`** (`[package] name = "snapdir"`, the verified-free
  flagship crates.io name): workspace-inherited version/edition/rust-version/
  authors/license/repository/homepage (homepage inherits = snapdir.org),
  description "Content-addressed directory snapshots — the snapdir CLI.",
  `readme = "README.md"` + placeholder README (docs gate rewrites it),
  `[lints] workspace = true`, and `[[bin]] name = "snapdir"` over a 3-line
  `main` calling `snapdir_cli::run()`. Dependency is a **direct path+version
  dep** (`snapdir-cli = { path = "../snapdir-cli", version = "1.5.0" }`) —
  NOT added to `[workspace.dependencies]` because only this crate consumes it
  (keeps the root Cargo.toml delta to the one members line; cargo-shear has
  killed unused workspace entries before).
- **Parity tests** `crates/snapdir/tests/parity.rs` (run by
  `cargo test -p snapdir`): under (b) there is one binary, so parity = the new
  bin vs the EXPECTED outputs of the old one — `version`/`--version` pinned to
  the frozen `snapdir <semver>` line (empty stderr, exit 0); `--help` asserted
  **byte-identical** (modulo trailing newline) to the snapdir-cli trycmd
  snapshot `tests/cmd/help.trycmd` under a cleared env, so the shim can never
  drift from the snapshot suite guarding the implementation; `defaults` keeps
  the oracle's `sort -u` shape + `SNAPDIR_BIN_PATH` names the new bin; and an
  id → push → fetch → checkout round-trip over a temp `file://` store with
  isolated `SNAPDIR_CACHE_DIR` (fetch into a FRESH cache) re-manifests to the
  same snapshot id (condensed from `store_roundtrip.rs`).
- **e2e install smoke** (`cargo_install_smoke` in parity.rs): runs
  `cargo install --path crates/snapdir --root <tempdir> --locked` then asserts
  `<root>/bin/snapdir version` prints the frozen line. Marked
  `#[ignore = "cargo install rebuilds the workspace in release mode (minutes); run with -- --ignored"]`
  — justification: cargo install rebuilds the full dep graph in release mode in
  its own staging target (measured: see verification — minutes, far over the
  60s budget) and must not thrash CPU/locks under the parallel workspace suite.
  Proven green manually (output below).
- **Test repointing in snapdir-cli** (anticipated by the gate): 7 test files
  used compile-time `env!("CARGO_BIN_EXE_snapdir")`, which only resolves in the
  crate DEFINING the bin — they now use `assert_cmd::cargo::cargo_bin("snapdir")`
  (runtime lookup: `CARGO_BIN_EXE_*` env first, then the shared-target-dir
  fallback — same sibling-lookup idea as `snapdir-ssh-store/tests/accel.rs`,
  but via the existing assert_cmd dev-dep instead of hand-rolling it):
  `dryrun.rs`, `list_options.rs`, `external_store_roundtrip.rs`, `manifest.rs`
  (whose `run_stdout` helper now takes `&Path`), `path_normalize.rs`,
  `plumbing.rs`, `store_roundtrip.rs`. Each helper documents that a standalone
  `cargo test -p snapdir-cli` needs `cargo build -p snapdir` first (under
  `cargo test --workspace` the bin always builds before tests run).
- **trycmd NOT repointed — verified unnecessary:** `trycmd::TestCases` resolves
  `$ snapdir` via snapbox's `cargo_bin_opt` (read the vendored 1.2.0/1.2.2
  sources): `CARGO_BIN_EXE_snapdir` env first, then a legacy target-dir lookup
  (`current_exe()` minus `deps`) that finds `target/debug/snapdir` regardless
  of which package built it. Same for the 12 files using assert_cmd's
  `Command::cargo_bin("snapdir")`. All snapdir-cli trycmd snapshots pass
  unchanged (workspace run below) — zero `.trycmd` files touched.
- **READMEs:** snapdir-cli README documents the move per the operator decision
  ("the snapdir binary now ships in the `snapdir` crate; snapdir-cli <= 1.5
  keeps installing the old bin; the lib entrypoint continues, not a stable
  API"); the new crates/snapdir README documents `cargo install snapdir`.
- **Root registration (documented mechanical exception):** one `members` line
  in the root Cargo.toml + the generated 7-line Cargo.lock delta (the new
  `snapdir` package entry). Nothing else at the root.

## Files changed (git diff --stat)

```
 Cargo.lock                                         |   7 +
 Cargo.toml                                         |   1 +
 crates/snapdir-cli/Cargo.toml                      |   9 +-
 crates/snapdir-cli/README.md                       |  19 +-
 crates/snapdir-cli/src/lib.rs (NEW)                |  49 ++++
 crates/snapdir-cli/src/main.rs (DELETED)           |  29 ---
 crates/snapdir-cli/tests/dryrun.rs                 |  10 +-
 .../snapdir-cli/tests/external_store_roundtrip.rs  |  10 +-
 crates/snapdir-cli/tests/list_options.rs           |  10 +-
 crates/snapdir-cli/tests/manifest.rs               |  31 ++-
 crates/snapdir-cli/tests/path_normalize.rs         |  10 +-
 crates/snapdir-cli/tests/plumbing.rs               |  12 +-
 crates/snapdir-cli/tests/store_roundtrip.rs        |  10 +-
 crates/snapdir/Cargo.toml (NEW)                    |  25 ++
 crates/snapdir/README.md (NEW)                     |  26 ++
 crates/snapdir/src/main.rs (NEW)                   |   9 +
 crates/snapdir/tests/parity.rs (NEW)               | 275 +++++++++++++++++++++
 17 files changed, 479 insertions(+), 62 deletions(-)
```

## Collision experiment result

Scratch workspace at `/tmp/bincoll` (OUTSIDE the repo): packages `a` and `b`,
each `[[bin]] name = "snapdir"`, `cargo build --workspace`:

- **toolchain 1.91.1** (the repo's pinned toolchain): `warning: output filename
  collision. The bin target `snapdir` in package `b …` has the same output
  filename as the bin target `snapdir` in package `a …`. Colliding filename is:
  /tmp/bincoll/target/debug/snapdir … This may become a hard error in the
  future; see rust-lang/cargo#6313.` Exit code **0**, last-writer-wins.
- **cargo 1.96.0** (current stable): same warning (plus a second one for the
  macOS `.dSYM`), still exit 0.

A warning on every workspace build violates the zero-warning CI bar → design
(a) (both crates keeping a `snapdir` bin) is dead → design (b).

## Local verification

1. **Exact gate command** —
   `cargo test -p snapdir --locked && cargo build --workspace --locked && test -x target/debug/snapdir`
   → exit 0. Parity suite: `4 passed; 0 failed; 1 ignored` (the ignored one is
   the documented `cargo_install_smoke`); workspace build finished with ZERO
   warnings (no filename collision — only one crate owns the bin);
   `target/debug/snapdir` is executable; `crates/snapdir/Cargo.toml` exists.
2. **`cargo test --workspace --locked`** → exit 0, **558 passed / 0 failed /
   1 ignored** across 49 test binaries — including snapdir-cli's
   `cli_surface.rs` trycmd suite (unchanged snapshots) and the loopback-sshd
   suite. Last 30 lines of the run:

   ```
   test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

      Doc-tests snapdir_cli

   running 0 tests

   test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

      Doc-tests snapdir_core

   running 2 tests
   test crates/snapdir-core/src/store.rs - store::object_path (line 69) ... ok
   test crates/snapdir-core/src/store.rs - store::manifest_path (line 91) ... ok

   test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.50s

      Doc-tests snapdir_ssh_store

   running 0 tests

   test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

      Doc-tests snapdir_stores

   running 1 test
   test crates/snapdir-stores/src/router.rs - router::resolve_adapter (line 138) ... ok

   test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.87s
   ```

3. **Lint/format/deps**: `cargo fmt --check` clean;
   `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0
   (`Finished dev profile`, zero warnings); `cargo shear` → "no issues found".
4. **Install smoke proven manually** (the `#[ignore]`d test's exact steps):
   `cargo install --path crates/snapdir --root /tmp/snapdir-install-smoke --locked`
   → `Installed package snapdir v1.5.0 … (executable snapdir)` (took minutes —
   full release-mode rebuild in cargo-install's own staging target, confirming
   the `#[ignore]` justification), then
   `/tmp/snapdir-install-smoke/bin/snapdir version` → `snapdir 1.5.0`. Exit 0.

## Reuse check

- The new crate reimplements NOTHING: `main` is a 3-line shim over
  `snapdir_cli::run()`; `run()` is the old `main` body moved verbatim.
- Bin lookup in repointed tests reuses the existing `assert_cmd` dev-dep
  (`assert_cmd::cargo::cargo_bin`) — the same env-then-target-dir fallback
  `accel.rs` hand-rolls, with no new code or deps.
- Parity round-trip condenses the proven `store_roundtrip.rs` pattern; the
  `--help` parity reuses the existing trycmd snapshot as its golden instead of
  duplicating 127 lines of help text.
- Zero new external dependencies; Cargo.lock delta is the new internal package
  entry only (7 lines).

## Blockers

None for this gate. **Out-of-lane follow-ups the PM should route (release
plumbing still points at snapdir-cli for the bin):**

1. `packaging/dist-workspace.toml` builds the release archives via
   `-p snapdir-cli … --bins` — snapdir-cli no longer has bins, so the next
   release's archives would miss the `snapdir` binary until packaging repoints
   to `-p snapdir`.
2. `.github/workflows/release.yml`: `CLI_CRATE: snapdir-cli` (gen-assets shells
   the completions/man subcommands — the binary now builds from `-p snapdir`)
   and the crates.io publish loop (`snapdir-core … snapdir-cli`) does not yet
   publish the new `snapdir` crate; publish order must end
   `… snapdir-cli snapdir`. Also: crates.io Trusted Publishing registration for
   the brand-new `snapdir` name (first publish via TP will 403, same as
   snapdir-ssh-store in 1.5.0).
3. Docs gate rewrites `crates/snapdir/README.md` (placeholder) and flips the
   documented install to `cargo install snapdir` repo-wide.

Ready for PM verification: YES
