# generic handoff for deps-upgrade-general @ 2026-06-03T00:00:00Z (RE-RUN)

## Summary

Re-applied the small set of non-TLS dependency bumps validated by the parked run,
on top of the current post-`deps-upgrade-tls-sdk` (deny-clean) tree. Three direct
bumps only; no source migration; no TLS/SDK touched.

Bumps applied (Cargo.toml + lock):

- `md-5` 0.10 → **0.11** (lock 0.10.6 → 0.11.0)
- `sha2` 0.10 → **0.11** (lock core now 0.11.0; transitive 0.10.9 retained for
  aws-sigv4 → p256, an allowed duplicate under `deny.toml multiple-versions = "warn"`)
- `criterion` 0.7 → **0.8** (benches/Cargo.toml; lock 0.7.0 → 0.8.2)

These pull in RustCrypto `digest 0.11.3`. No `src/` migration required:
`crates/snapdir-core/src/merkle.rs` uses only the one-shot `Md5::digest()` /
`Sha256::digest()` API, unchanged across digest 0.10 → 0.11. Zero `crates/*/src`
edits. Lock regenerated with a targeted `cargo update -p md-5@0.10.6
-p sha2@0.10.9 -p criterion@0.7.0` (NOT a blanket `cargo update`) so the cooldown
holds: `bitflags` stays 2.11.1 and `log` stays 0.4.30 (the in-range 2.12.1 /
0.4.31 are ~1 day old and were deliberately NOT pulled). criterion 0.8's new
transitive deps (alloca, page_size, winapi*) are all ≥3 days old.

`rustls-native-certs` was already pinned to 0.8.3 by the TLS gate — left untouched.

## Files changed

```
 Cargo.lock                     | 79 ++++++++++++++++++++++++++++++------------
 benches/Cargo.toml             |  2 +-
 crates/snapdir-core/Cargo.toml |  4 +--
 3 files changed, 59 insertions(+), 26 deletions(-)
```

(No `crates/*/src` edits. Root `Cargo.toml` unchanged.)

## Local verification result

Full command:
`cargo build --workspace --locked && cargo test --workspace --locked && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check && bash utils/ci/check-crate-age.sh`

- `cargo build --workspace --locked` → OK (`Finished dev profile ... in 19.67s`).
- `cargo test --workspace --locked` → OK (all unit/integration/doc tests pass,
  including `crates/snapdir-core/tests/compat_golden.rs`; no failures).
- `cargo clippy --workspace --all-targets -- -D warnings` → OK (`Finished` clean).
- `cargo deny check` → **advisories ok, bans ok, licenses ok, sources ok** (exit 0).
- `bash utils/ci/check-crate-age.sh` final line:

```
check-crate-age.sh: OK — all 429 registry crate(s) are at least 3 day(s) old.
```
(exit 0 — 0 offenders.)

## Reuse check / Blockers

- No oracle script edits (root `snapdir`, `snapdir-*`, `snapdir-test`) and no
  `utils/qa-fixtures/` edits. No `src/` edits.
- No TLS/SDK version changes: rustls / hyper / hyper-rustls / aws-* /
  google-cloud-* / ring untouched. rustls-native-certs left at 0.8.3.
- `grep -ic aws-lc Cargo.lock` → 0. `grep -ic openssl-sys Cargo.lock` → 0.
- The 3 bumps applied (md-5 0.11, sha2 0.11, criterion 0.8); bitflags 2.11.1 and
  log 0.4.30 left aged (cooldown respected).
- No blockers. Full verification command reaches exit 0.

Ready for PM verification: YES
