# generic handoff for deps-upgrade-general @ 2026-06-03T00:00:00Z

## Summary

Bumped the non-TLS/SDK workspace dependencies to their latest version published
≥3 days ago (supply-chain cooldown) and applied the required lockfile-hygiene pin
on `rustls-native-certs`.

Key findings:

- **Nearly every direct non-TLS dep was already at its aged-latest** (clap 4.6.1,
  clap_complete 4.6.5, clap_mangen 0.3.0, serde 1.0.228, serde_json 1.0.150,
  redb =4.1.0, thiserror 2.0.18, anyhow 1.0.102, blake3 1.8.5, walkdir 2.5.0,
  ignore 0.4.25, tokio 1.52.3, tempfile 3.27.0, predicates =3.1.4,
  assert_cmd =2.2.2, trycmd =1.2.0, proptest 1.11.0, hex 0.4.3, regex 1.12.3,
  bytes 1.11.1, assert_fs =1.1.4, http dev-dep 1.4.1). No change needed.
- **Three real bumps were available and applied** (Cargo.toml + lock):
  - `md-5` 0.10 → 0.11 (lock 0.10.6 → 0.11.0)
  - `sha2` 0.10 → 0.11 (lock: core now on 0.11.0; aws-sigv4→p256 still pins a
    transitive 0.10.9, an allowed duplicate — `deny.toml` `multiple-versions = "warn"`)
  - `criterion` 0.7 → 0.8 (lock 0.7.0 → 0.8.2)
  These pulled in the RustCrypto `digest 0.11` line. **No source migration was
  required**: `merkle.rs` uses only the one-shot `Md5::digest()` / `Sha256::digest()`
  API, which is unchanged across digest 0.10 → 0.11. Build + tests (incl. the
  `compat_golden.rs` byte-format contract) pass with zero `crates/` source edits.
- **Required cooldown pin applied:** `cargo update -p rustls-native-certs@0.8.4
  --precise 0.8.3` — lock 0.8.4 (1.5 days old) → 0.8.3 (155 days old, latest aged
  ^0.8). Pure lockfile hygiene; no Cargo.toml touched, rustls-native-certs stays.
- **Deliberately NOT bumped:** `bitflags` 2.11.1→2.12.1 and `log` 0.4.30→0.4.31
  are available in-range but were published 2026-06-02 (~1 day old) — they FAIL the
  cooldown, so the lock is correctly held at the aged 2.11.1 / 0.4.30.

## Files changed

```
 Cargo.lock                     | 75 ++++++++++++++++++++++++++++++------------
 benches/Cargo.toml             |  2 +-
 crates/snapdir-core/Cargo.toml |  4 +--
 3 files changed, 57 insertions(+), 24 deletions(-)
```

(No `crates/*/src` edits. Root `Cargo.toml` unchanged — its only direct deps,
thiserror "2" / anyhow "1", were already aged-latest.)

## Local verification result

`cargo build --workspace --locked` → OK (Finished dev profile).
`cargo test --workspace --locked` → OK (all unit/integration/doc tests pass,
including `compat_golden.rs`).
`cargo clippy --workspace --all-targets -- -D warnings` → OK (Finished, 0 warnings).
`cargo deny check` → **FAILED (advisories + licenses)** — see Blockers; bans ok,
sources ok. These failures are PRE-EXISTING and identical on the unmodified
git-HEAD lock (verified by stash + re-run); all are rooted in the TLS/SDK tree.
`bash utils/ci/check-crate-age.sh` final line:

```
check-crate-age.sh: OK — all 444 registry crate(s) are at least 3 day(s) old.
```
(exit 0 — 0 offenders; the prior rustls-native-certs 0.8.4 offender is resolved.)

## Reuse check / Blockers

- No oracle script edits (root `snapdir`, `snapdir-*`, `snapdir-test`) and no
  `utils/qa-fixtures/` edits. Confirmed.
- Did NOT touch rustls / hyper / aws-* / google-cloud-* semantic versions. All
  aws-* lock versions verified unchanged (aws-config 1.8.17, aws-sdk-s3 1.134.0,
  aws-sigv4 1.4.4, aws-smithy-runtime-api 1.12.1); rustls 0.21.12/0.23.40,
  hyper-rustls 0.24.2/0.27.9, google-cloud-storage 1.12.0, google-cloud-gax
  1.10.0 unchanged. ring rustls provider intact.
- `grep -i aws-lc Cargo.lock` → empty (0). `grep -i openssl-sys Cargo.lock` →
  empty (0). Invariants hold.
- Exact non-TLS crates bumped (old → new):
  - md-5 0.10.6 → 0.11.0
  - sha2 0.10.9 → 0.11.0 (core lane; transitive 0.10.9 retained for aws-sigv4/p256)
  - criterion 0.7.0 → 0.8.2
  - rustls-native-certs 0.8.4 → 0.8.3 (transitive lock pin, cooldown hygiene)

- **BLOCKER (out of this gate's scope — owned by `deps-upgrade-tls-sdk`):**
  `cargo deny check` exits non-zero, so the PM's combined verification command
  will not reach exit 0 until the TLS gate runs. ALL deny failures are
  pre-existing (present on git HEAD before my changes) and rooted entirely in the
  legacy TLS stack, NOT introduced by my non-TLS bumps:
  - RUSTSEC-2026-0104 / GHSA-xgp8-3hg3-c2mh (+ name-constraint advisories):
    `rustls-webpki 0.101.7` ← `rustls 0.21.12` ← `hyper-rustls 0.24.2` / `tokio-rustls 0.24.1`.
  - unmaintained: `rustls-pemfile 1.0.4`.
  - license rejection: same old-rustls 0.21 subtree.
  These require bumping rustls/hyper-rustls (or webpki), which is explicitly in
  the deps-upgrade-tls-sdk lane — I am forbidden from touching those semantic
  versions. `cargo deny check bans` and `cargo deny check sources` both pass,
  confirming my non-TLS bumps added zero new bans/advisory-source problems.

Ready for PM verification: YES
