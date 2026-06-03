# 0004 — Use the `ring` rustls provider, ban aws-lc-rs

Status: Accepted, 2026-06

## Context

The shipped binary statically links on musl (the zero-runtime-dependency target).
`aws-lc-rs`, the default rustls crypto provider for the AWS SDK and for several cloud
crates, does not static-link cleanly on musl. Both the AWS SDK and
`google-cloud-storage` pull `aws-lc-rs` in through default features, and some paths can
also drag in `openssl-sys`.

## Decision

Standardize the whole workspace on the **`ring`** rustls crypto provider. Ban
`aws-lc-rs`, `aws-lc-sys`, and `openssl-sys` in `deny.toml` so that any reintroduction
is a build failure. To keep them out of the dependency graphs:

- **AWS SDK (s3/b2):** disable default features on `aws-config`/`aws-sdk-s3` (dropping
  the aws-lc-rs-backed connector and default crypto), build a `hyper-rustls` HTTP
  client pinned to `ring`, and hand it to the SDK as a custom `HttpClient`; keep
  `behavior-version-latest`.
- **GCS:** set `default-features = false` on `google-cloud-storage` (dropping
  `default-idtoken-backend` → `aws-lc-rs` and `default-rustls-provider` →
  `rustls/aws_lc_rs`), force a `ring`-backed rustls into the graph, and install it as
  the process-default `CryptoProvider` at connect time; auth/transport uses `reqwest`
  with `rustls-no-provider`, which then uses that installed default.

The acceptance check is mechanical: after building, `grep -i aws-lc Cargo.lock` must be
empty (and likewise no `openssl-sys`).

## Alternatives considered

- **Default aws-lc-rs provider.** Rejected: breaks the musl static build, which is the
  whole point of the zero-dependency binary.
- **OpenSSL / native-tls.** Rejected: reintroduces a system dependency and is also
  banned in `deny.toml`.

## Consequences

- The static musl build links cleanly; the binary stays dependency-free.
- The SDK wiring is more verbose (custom connector, manual provider install) than the
  defaults.
- `deny.toml` enforces the ban permanently; the `grep -i aws-lc Cargo.lock` empty check
  is part of every dependency-touching gate. (The provider stack was later unified on
  rustls 0.23 / hyper-rustls 0.27 while keeping ring — see ADR-0026.)
