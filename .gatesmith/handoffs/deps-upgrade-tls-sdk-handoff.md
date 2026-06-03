# stores handoff for deps-upgrade-tls-sdk @ 2026-06-03T00:00:00Z

## Summary

Collapsed the two TLS islands in the lock down to the single **rustls 0.23 /
hyper 1.x / ring** island, and unpinned the GCS SDK. The old island (rustls
0.21, hyper-rustls 0.24, tokio-rustls 0.24, rustls-native-certs 0.6,
rustls-webpki 0.101, hyper 0.14) was pulled exclusively by the AWS S3 connector;
it is now gone.

What I changed:

- **`crates/snapdir-stores/Cargo.toml` (S3 TLS stack):**
  - Dropped the `connector-hyper-0-14-x` feature on `aws-smithy-runtime` (and the
    direct `aws-smithy-runtime` dep entirely — it was only there for that
    connector feature).
  - Removed the explicit `rustls = "0.21"` and `hyper-rustls = "0.24"` pins.
  - Added `aws-smithy-http-client = { version = "1", default-features = false,
    features = ["rustls-ring"] }` — the SDK's modern hyper-1.x HTTP client with a
    rustls(ring) connector. `rustls-ring` pulls `default-client` (hyper 1.x) +
    rustls 0.23 ring; no aws-lc-rs, no hyper 0.14.
- **`crates/snapdir-stores/src/s3_store.rs`:** rewrote `ring_https_client()` to
  `aws_smithy_http_client::Builder::new().tls_provider(Provider::Rustls(CryptoMode::Ring)).build_https()`.
  Native roots are kept via the builder's default `TrustStore` (native roots
  enabled by default), satisfying the keep-`with_native_roots()` operator
  decision. Dropped the `aws_smithy_runtime::client::http::hyper_014::HyperClientBuilder`
  import and the hand-built `hyper_rustls::HttpsConnectorBuilder` connector.
- **`crates/snapdir-stores/Cargo.toml` (GCS SDK):** unpinned
  `google-cloud-storage` from `=1.12.0` -> `1` and `google-cloud-gax` from
  `=1.10.0` -> `1`, both still `default-features = false`. Latest aged versions
  resolve to 1.12.0 / 1.10.0 (both published 2026-05-05, 29d old). The
  `is_not_found` / `Error::status()` -> `rpc::Code` error model is unchanged in
  these versions — `gcs_store.rs` needed no edits; its three error-classification
  regression tests pass.
- **`deny.toml`:** added `"CDLA-Permissive-2.0"` to the license allow-list (the
  license of `webpki-root-certs`, a Mozilla CA-trust-roots *data* crate pulled
  transitively via rustls-platform-verifier <- reqwest <- google-cloud-auth).
- **`Cargo.lock`:** old island removed; `rustls-native-certs` pinned to **0.8.3**
  (`cargo update -p rustls-native-certs --precise 0.8.3`) because 0.8.4
  (published 2026-06-01) is younger than the 3-day cooldown; 0.8.3 satisfies `^0.8`.

The object/manifest sharded-key logic, push ordering (objects-before-manifest,
skip-if-present), and fetch verify/retry discipline are untouched — only the TLS
client construction changed.

## Files changed

```
 Cargo.lock                            | 286 +++++++---------------------------
 crates/snapdir-stores/Cargo.toml      |  48 +++---
 crates/snapdir-stores/src/s3_store.rs |  28 ++--
 deny.toml                             |   5 +
 4 files changed, 103 insertions(+), 264 deletions(-)
```

## Local verification result

Verification command:
`cargo build -p snapdir-stores --locked && ! grep -iq 'aws-lc' Cargo.lock && ! grep -iq 'openssl-sys' Cargo.lock && cargo test -p snapdir-stores --locked && bash utils/ci/check-crate-age.sh`

`cargo build -p snapdir-stores --locked` -> Finished, exit 0.
`grep -i aws-lc Cargo.lock` -> empty; `grep -i openssl-sys Cargo.lock` -> empty.

`cargo test -p snapdir-stores --locked` (last 20 lines):

```
test result: ok. 58 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running tests/shim_external_store.rs (target/debug/deps/shim_external_store-184eac7701d1b91c)

running 5 tests
test shim_get_manifest_missing_id_maps_to_manifest_not_found ... ok
test shim_fetch_files_surfaces_missing_object_error ... ok
test shim_push_is_noop_when_manifest_already_present ... ok
test shim_push_writes_objects_before_manifest_then_get_manifest_roundtrips ... ok
test shim_fetch_files_pulls_objects_into_cache_dir ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

   Doc-tests snapdir_stores

running 1 test
test crates/snapdir-stores/src/router.rs - router::resolve_adapter (line 138) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.59s
```

`cargo deny check` -> **`advisories ok, bans ok, licenses ok, sources ok`**, exit 0.
(RUSTSEC-2026-0098/0099, RUSTSEC-2025-0134 cleared with the old island; CDLA
license accepted. Only `multiple-versions = "warn"` duplicate warnings remain —
pre-existing, non-fatal.)

`bash utils/ci/check-crate-age.sh` final line:

```
check-crate-age.sh: OK — all 425 registry crate(s) are at least 3 day(s) old.
```
exit 0.

`cargo clippy -p snapdir-stores --locked` -> Finished, exit 0 (no warnings).

## Reuse check / Blockers

- **aws-lc-rs / aws-lc-sys / openssl-sys:** ABSENT from `Cargo.lock`
  (`grep -i aws-lc Cargo.lock` and `grep -i openssl-sys Cargo.lock` both empty).
- **Old island GONE:** `grep -E 'rustls 0.21|hyper-rustls 0.24|tokio-rustls 0.24|rustls-native-certs 0.6|rustls-webpki 0.101|hyper 0.14' Cargo.lock`
  returns nothing. `rustls-pemfile` is also gone. No hyper 0.14.x remains.
- **ring provider only + native roots kept:** S3 uses
  `Provider::Rustls(CryptoMode::Ring)` + default native-root `TrustStore`; GCS
  still installs `rustls_ring::crypto::ring::default_provider()` as the process
  default in `GcsStore::connect`.
- **Sharded-key / push-ordering / verify logic UNTOUCHED** (only TLS client
  construction changed; `S3Location`/`GcsLocation` key helpers, push ordering,
  fetch retry/verify unchanged).
- **Old -> new versions:**
  - rustls: 0.21.12 (removed) — now only 0.23.40.
  - hyper-rustls: 0.24.2 (removed) — now only 0.27.9.
  - tokio-rustls: 0.24.1 (removed) — now only 0.26.4.
  - hyper: 0.14.32 (removed) — now only 1.10.1.
  - rustls-webpki: 0.101.7 (removed) — now only 0.103.13.
  - rustls-native-certs: 0.6.3 (removed); 0.8.4 -> pinned 0.8.3.
  - rustls-pemfile: 1.0.4 (removed).
  - aws-sdk-s3: 1.134.0 (unchanged, latest).
  - aws-config: 1.8.17 (unchanged, latest).
  - aws-smithy-http-client: 1.1.12 (newly direct dep, was transitive).
  - google-cloud-storage: `=1.12.0` -> `1` (resolves 1.12.0).
  - google-cloud-gax: `=1.10.0` -> `1` (resolves 1.10.0).
- **deny.toml:** added `"CDLA-Permissive-2.0"` to the license allow-list (for
  `webpki-root-certs`).
- **rustls-native-certs pin:** 0.8.3 (155d old) via
  `cargo update -p rustls-native-certs --precise 0.8.3`.
- **cargo deny check:** now clean (exit 0).
- musl-static: not linkable locally; no openssl-sys / aws-lc introduced, so
  static linking is not regressed (proven later by deps-verify in CI).
- **Blockers:** none. All changes stayed within
  `crates/snapdir-stores/`, `Cargo.lock`, and `deny.toml` (license allow-list only).

Ready for PM verification: YES
