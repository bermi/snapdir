# Handoff — snapdir-name-docs (phase 26)

**Teammate:** docs · **Date:** 2026-06-11 · **Gate:** `snapdir-name-docs`

## What was done

1. **`README.md` (root)** — install section: `cargo install snapdir-cli` →
   `cargo install snapdir` as the primary crates.io install (same comment
   style); the prebuilt-archives + `cargo add snapdir-core` "Other ways" block
   kept verbatim. Added a blockquote migration note: `snapdir-cli` is now the
   implementation library, its versions ≤ 1.5.0 still install the binary, the
   binary lives in the `snapdir` crate from 1.5.1. Also fixed the stale Status
   bullet `v1.4.0` → `v1.5.0` (1.5.0 is the released version; the 15-subcommand
   count is unchanged — the wire plumbing subcommands are hidden).
2. **`crates/snapdir/README.md`** — replaced the placeholder with the full
   flagship crates.io page: one-paragraph pitch (content-addressed snapshots,
   deterministic ID, zero runtime deps — condensed from the root README),
   install (`cargo install snapdir` + release archives + the snapdir-cli
   migration note), the 60-second quick start lifted verbatim from the root
   README (id/push/pull round-trip through a `file://` store), a 6-row store
   table incl. `ssh://` + `sftp://` (with the `snapdir-ssh-store` install
   pointer), and links to snapdir.org, the canonical repo, and the
   `snapdir-cli` implementation crate.
3. **`crates/snapdir-cli/README.md`** — polished the cli teammate's draft
   (kept its structure): store list now mentions ssh/sftp remotes (parity with
   the other pages); "From the next release" → "From 1.5.1" (the README ships
   *in* 1.5.1, so "next release" would read wrong on crates.io); `<= 1.5` →
   "≤ 1.5.0 keep installing the binary directly". The not-semver-stable
   `snapdir_cli::run()` wording was already correct and was kept.
4. **`docs/rust-port/CHANGELOG.md` [Unreleased]** — Added: `snapdir` crate
   ships the CLI binary (`cargo install snapdir`); Changed: `snapdir-cli` is
   now the implementation library, binary moved to the `snapdir` crate,
   ≤ 1.5.0 unaffected, `run()` not a semver-stable general-purpose API.
5. **`CONTRIBUTING.md` workspace table** — `crates/snapdir-cli` row reworded
   to "CLI implementation library"; new `crates/snapdir` row (thin shim);
   added the previously-missing `crates/snapdir-ssh-store` row (pre-existing
   omission from phase 24, accuracy fix, in lane).
6. **`docs/rust-port/migration.md`** — checked; its only `snapdir-cli`
   reference is the `crates/snapdir-cli/src/cli.rs` clap-surface pointer,
   which is still accurate (the clap surface stays in the implementation
   library). No change needed. The `cargo install snapdir-cli` mention at
   CHANGELOG line ~210 is inside the historical [1.1.0-era] entry — left
   untouched per Keep-a-Changelog convention.

## Accuracy check (claims verified against code)

- `crates/snapdir/Cargo.toml`: `[[bin]] name = "snapdir"`, `readme =
  "README.md"`, depends on `snapdir-cli = { path, version = "1.5.0" }` — the
  shim design (b) docs describe. ✓
- `crates/snapdir/src/main.rs` is the only source file (3-line shim). ✓
- Pre-release wording constraint honored: the root README documents
  `cargo install snapdir` as primary (it ships in the same 1.5.1 release that
  publishes the crate), and the migration note + CHANGELOG [Unreleased] entry
  record the ≤ 1.5.0 / 1.5.1 boundary explicitly — same convention prior
  pre-release features used ([Unreleased] carries the claim until the release
  cut). No page claims the `snapdir` crate is *already* on crates.io.
- Store table rows match the root README's (file/s3/b2/gs/ssh/sftp);
  ssh/sftp external-binary + `cargo install snapdir-ssh-store` wording matches
  the root README Stores section. ✓

## Verification results

- Gate: `grep -q 'cargo install snapdir' README.md && test -f
  crates/snapdir/README.md` → exit 0. ✓
- `typos README.md docs/ crates/snapdir/README.md crates/snapdir-cli/README.md
  CONTRIBUTING.md` → clean. ✓
- `cargo package -p snapdir --list --allow-dirty | grep README` → `README.md`
  (the new flagship README ships in the package). ✓
- Diff touches ONLY: README.md, CONTRIBUTING.md, docs/rust-port/CHANGELOG.md,
  crates/snapdir/README.md, crates/snapdir-cli/README.md (lane +
  documented crate-README allowance). Nothing committed.

## Operator follow-ups (out of scope)

- **../snapdir-website (snapdir.org)**: its install/getting-started pages will
  still say `cargo install snapdir-cli` — update them when 1.5.1 ships (the
  site repo is outside this repo/lane).
- After 1.5.1 publishes, the `snapdir` crates.io page renders the new
  flagship README; first publish of the `snapdir` name needs the manual
  keychain-token path (TP can't create new crates), already flagged in the
  release gate.

Ready for PM verification: YES
