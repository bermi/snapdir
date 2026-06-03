# docs handoff for readme-docs-final @ 2026-06-03

## Summary

Finalized the human-facing docs now that the port is complete (Phase 11), the
legacy Bash implementation is removed, deps are modernized, the image is
scratch + static musl + bundled CA roots, and ADRs exist in `docs/adr/`.

- **CONTRIBUTING.md** — replaced the `## The frozen Bash oracle` section with
  `## The byte-format contract`: the port is complete, the legacy Bash
  implementation was removed in Phase 11, and the byte-format contract is now
  guarded by `crates/snapdir-core/tests/compat_golden.rs` (Rust golden-constant
  tests) + the `manifest-format.sha.lock` tripwire. Dropped all oracle
  invocations and linked to the manifest spec and ADRs. The "Local CI gate
  (pre-push hook)" section was kept (it already describes the fast legs ~2–4 min,
  musl+coverage in CI, and `make ci-local`).
- **docs/rust-port/migration.md** — reworded the header note that "pinned to the
  frozen Bash oracle (`./snapdir`, `./snapdir-manifest`, the `snapdir-*-store`
  scripts...)" into a past-tense historical description without the banned
  strings.
- **docs/rust-port/manifest-spec.md** — past-tensed the header block (now
  references the Rust golden tests + SHA-lock as the contract guard) and changed
  ":60 frozen Bash oracle has a known bug" → "the original snapdir had a known
  bug".
- **docs/rust-port/PLAN.md** — past-tensed the source-of-truth note (it lives
  under `docs/rust-port/` so the grep checks it); now a historical planning
  document, with the byte-format contract pointing at the golden tests + SHA-lock.
- **docs/rust-port/CHANGELOG.md** — line ~38 "verified byte-for-byte against the
  live `./snapdir-manifest` oracle" → "against the original snapdir's manifest
  output". Added a new top entry **[0.6.0] — Port complete** summarizing Phase 11
  (Bash implementation removed; byte-contract → Rust golden tests + SHA-lock;
  deps modernized: rustls 0.23 / hyper 1.x / ring, latest AWS SDK, unpinned
  google-cloud, 3-day cooldown; MSRV 1.91.1; scratch + musl + CA-certs image;
  ADRs added; local pre-push CI gate). Updated the version-compare links. No
  Cargo.toml versions touched — narrative only.
- **README.md** — bumped to v0.6.0; rewrote the Install section with an accurate
  Docker story (`FROM scratch`, static musl binary + bundled CA roots
  `ca-certificates.crt`, zero runtime executables/libc/shell) and install paths
  (cargo-dist release archives, `cargo install`, `docker run`/`docker build .`);
  added a link to the ADRs (`docs/adr/`). Contains the word `scratch`.

## Files changed

```
 CONTRIBUTING.md                 | 33 ++++++++++++++++++------------
 README.md                       | 21 ++++++++++++++-----
 docs/rust-port/CHANGELOG.md     | 45 +++++++++++++++++++++++++++++++++++++++--
 docs/rust-port/PLAN.md          |  8 +++++---
 docs/rust-port/manifest-spec.md | 25 +++++++++++------------
 docs/rust-port/migration.md     | 12 +++++------
 6 files changed, 102 insertions(+), 42 deletions(-)
```

## Local verification result

Verification command:

```
! grep -riqE 'frozen bash oracle|differential oracle|\./snapdir-(manifest|s3-store|b2-store|gcs-store|file-store)' README.md CONTRIBUTING.md docs/rust-port/ && grep -iq scratch README.md
```

Result: **exit 0** (pass — no banned strings, README mentions `scratch`).

Residual grep:

```
grep -rnE '\./snapdir-(manifest|s3-store|b2-store|gcs-store|file-store)|frozen bash oracle|differential oracle' README.md CONTRIBUTING.md docs/rust-port/
```

Output: **empty** (exit 1 — no matches).

## Reuse check / Blockers

- No banned strings remain in `README.md`, `CONTRIBUTING.md`, or
  `docs/rust-port/`.
- README has the `scratch` Docker story (static musl + bundled CA roots, zero
  runtime executables) and an ADR link (`docs/adr/`).
- CHANGELOG bumped to `0.6.0` with the Phase 11 narrative.
- No code, Cargo.toml, `.gatesmith/`, or `docs/adr/` content edited — docs lane
  scope only.
- No blockers.

Ready for PM verification: YES
