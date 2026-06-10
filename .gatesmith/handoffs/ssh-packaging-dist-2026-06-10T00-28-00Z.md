# Handoff — ssh-packaging-dist (phase 24)

## Summary

Made `snapdir-ssh-store` releasable through the EXISTING pipeline without releasing anything (no tag, no publish, no version bump — workspace stays 1.4.0):

1. **`.github/workflows/release.yml`**
   - **crates.io publish loop**: added `snapdir-ssh-store` to the idempotent skip-if-published loop, order now `core -> catalog -> stores -> ssh-store -> cli`. The loop's shape (sparse-index probe + `grep -q "vers"` skip + `cargo publish --locked --no-verify`) is byte-identical; only the `for crate in ...` list and comments changed. The sparse-index shard math (`c1c2/c3c4`) is uniform for any name >= 4 chars, so `snapdir-ssh-store` needs no special-casing (`sn/ap/snapdir-ssh-store`).
   - **Ordering justification**: runtime deps = `snapdir-core` only, BUT the crate has a **dev-dependency on `snapdir-stores`** (`workspace = true` carries `version = "<workspace>"`, which cargo records in the published index metadata). Publishing after `snapdir-stores` guarantees that recorded version exists on the registry; before `snapdir-cli` keeps the leaf consumer last. Hence stores -> ssh-store -> cli.
   - **PROMINENT TP comment** added at the `crates-io` job header: the job authenticates EXCLUSIVELY via Trusted Publishing OIDC (`rust-lang/crates-io-auth-action@v1` -> `CARGO_REGISTRY_TOKEN`); there is **no stored-token fallback** in the workflow. First publish of the NEW crate name is the operator-minded step (see Release-time operator TODOs below).
   - **Build matrix**: both build legs (cross + native) now build `-p snapdir-cli -p snapdir-ssh-store --bins` (verified locally that this selector produces all 3 bins), and the stage step copies `snapdir-ssh-store` + `snapdir-sftp-store` into the same per-target archive next to `snapdir`, via a new `SSH_STORE_CRATE`/`SSH_STORE_BINS` env pair. All 6 targets (incl. both musl-static legs) get both bins. Header comment updated to match.

2. **`packaging/dist-workspace.toml`** (comments only — no functional keys changed)
   - cargo-dist package selection is AUTO-DETECT of workspace `[[bin]]` packages; no config enumerates packages/bins explicitly and no crate carries `[package.metadata.dist]`, so the new crate is picked up automatically by `dist plan`. The `dist` tool is not installed locally (`which dist cargo-dist` -> nothing), so this is reasoned from the config + cargo-dist 0.28 semantics and documented in the file. The workspace-level `dist = false` (which keeps dist's own publish machinery off) is retained — release.yml's hand-rolled matrix is the implementation of record, and it now builds/stages all three bins per target (documented in the comment).

3. **`crates/snapdir-ssh-store/Cargo.toml` — NO CHANGES NEEDED.** It already matches sibling crates exactly: per-crate `description` + literal `readme = "README.md"` (the README exists from the ssh-docs gate, 0a40a05), workspace-inherited `license`/`repository`/`homepage` (homepage = https://snapdir.org via `[workspace.package]`). Siblings (core/catalog/stores/cli) set NO per-crate `keywords`/`categories` (workspace values are not inherited without explicit opt-in, and none opts in), so parity = adding none. **No `exclude` added — decision:** `cargo package --list` shows a tiny, clean package (9 src files, 5 test files, two fixture shell scripts of ~5K each, ~150K total source); nothing heavyweight or problematic is dragged in, and shipping the fixtures keeps the published package's test suite intact.

4. **musl**: the `x86_64-unknown-linux-musl` target is NOT installed locally (macOS host, no musl linker), so no local musl build. Verified CI covers it: `ci.yaml` `musl` job builds `cargo build --workspace --all-features --locked --target x86_64-unknown-linux-musl` in BOTH debug and release, and the new crate is a workspace member -> covered. The crate is std + snapdir-core only (no TLS/SDK edges), so the static link is trivially clean. Additionally built both bins with the actual `dist` release profile locally (fat LTO + panic=abort + strip): clean, 385K each.

5. NO version bumps, NO publishing, NO tags. `Cargo.toml`/`Cargo.lock` untouched (diff is release.yml + dist-workspace.toml only).

## Files changed (`git diff --stat`)

```
 .github/workflows/release.yml | 62 ++++++++++++++++++++++++++++++++-----------
 packaging/dist-workspace.toml | 21 +++++++++++----
 2 files changed, 62 insertions(+), 21 deletions(-)
```

## Local verification

```
$ actionlint .github/workflows/release.yml
ACTIONLINT-OK   (no findings)

$ cargo package -p snapdir-ssh-store --list --allow-dirty
.cargo_vcs_info.json
Cargo.lock
Cargo.toml
Cargo.toml.orig
README.md
src/args.rs
src/bin/snapdir-sftp-store.rs
src/bin/snapdir-ssh-store.rs
src/config.rs
src/lib.rs
src/script.rs
src/sftp_engine.rs
src/ssh_engine.rs
src/url.rs
src/version.rs
tests/accel.rs
tests/common/mod.rs
tests/common/sshd.rs
tests/emitted_contract.rs
tests/fake_sftp_roundtrip.rs
tests/fake_ssh_roundtrip.rs
tests/fixtures/fake-sftp
tests/fixtures/fake-ssh
tests/loopback_sshd.rs

$ cargo package -p snapdir-ssh-store --list --allow-dirty | grep -q 'README.md' \
    && cargo build --workspace --locked --target-dir target \
    && test -x target/debug/snapdir-ssh-store && test -x target/debug/snapdir-sftp-store
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.78s
GATE-OK exit=0

$ cargo build --locked -p snapdir-cli -p snapdir-ssh-store --bins   # release.yml selector
    Finished `dev` profile -> target/debug/{snapdir,snapdir-ssh-store,snapdir-sftp-store} all present

$ cargo build --locked --profile dist -p snapdir-ssh-store --bins   # actual release profile
    Finished `dist` profile [optimized] target(s) in 7.18s
    target/dist/snapdir-ssh-store   385520
    target/dist/snapdir-sftp-store  385520
```

## Release-time operator TODOs

1. **Trusted Publishing for `snapdir-ssh-store` (first release only).** The `crates-io` job authenticates ONLY via TP OIDC (`rust-lang/crates-io-auth-action@v1`) — no token fallback exists in the workflow. For a brand-new crate name the publish should succeed via the OIDC-asserted publisher; if crates.io rejects the first publish of the unregistered name, publish v-first manually with a scoped API token (`cargo publish -p snapdir-ssh-store --locked --no-verify`) and re-run the workflow (the idempotent skip-guard makes the re-run safe). **Either way, immediately after the first publish register TP for `snapdir-ssh-store`**: crate Settings -> Trusted Publishing -> GitHub -> repo `snapdir/snapdir`, workflow `release.yml`. (Same flow used for catalog/stores/cli at 1.2.0.) This is captured as a prominent comment block in release.yml's `crates-io` job.
2. Optional pre-flight before the next tag: `gh workflow run release.yml --ref dev -f dry_run=true` exercises the new 3-bin build/stage/archive path on all 6 targets without publishing (the publish jobs are hard-gated on a `v*` tag push).

## Blockers

None.

Ready for PM verification: YES
