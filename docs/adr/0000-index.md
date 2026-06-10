# Architecture Decision Records

This directory records the significant architecture and engineering decisions made
during the port of `snapdir` from Bash (`v0.5.0`, ~99% shell) to a single
statically-linked, zero-runtime-dependency Rust binary.

Records use the [MADR](https://adr.github.io/madr/) format. Each record states the
**Context**, the **Decision**, the **Alternatives considered**, and the
**Consequences**, plus a short status line. Records are immutable once accepted; a
later decision that changes course is written as a new record that supersedes the
earlier one (see ADR-0024, which supersedes ADR-0001).

## Index

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-differential-oracle-methodology.md) | Differential-oracle methodology | Accepted (superseded by 0024) |
| [0002](0002-manifest-format-freeze.md) | Freeze the manifest format and on-disk layout | Accepted |
| [0003](0003-snapshot-id-is-blake3-of-manifest-text.md) | Snapshot ID is BLAKE3 of the `#`-stripped manifest text | Accepted |
| [0004](0004-ring-tls-provider.md) | Use the `ring` rustls provider, ban aws-lc-rs | Accepted |
| [0005](0005-native-in-process-cloud-stores.md) | Native in-process cloud stores, no shelling out | Accepted |
| [0006](0006-b2-over-s3-compatible-endpoint.md) | Implement B2 over the S3-compatible endpoint | Accepted |
| [0007](0007-redb-catalog.md) | Replace the SQLite catalog with redb | Accepted |
| [0008](0008-catalog-json-output-lock.md) | Freeze the catalog JSON output format | Accepted |
| [0009](0009-gcs-notfound-classification.md) | Classify GCS missing objects via 404 and service NotFound | Accepted |
| [0010](0010-unix-only-drop-windows.md) | Unix-only: drop the Windows target | Accepted |
| [0011](0011-cargo-dist-musl-static-packaging.md) | Package with cargo-dist and musl-static targets | Accepted |
| [0012](0012-scratch-docker-image.md) | Ship a `FROM scratch` Docker image | Accepted |
| [0013](0013-coverage-floor-75.md) | Enforce a 75% line-coverage floor | Accepted |
| [0014](0014-remove-verify-purge.md) | Remove `verify --purge` | Accepted |
| [0015](0015-all-14-subcommands-wired.md) | Wire all 14 CLI subcommands, no stubs | Accepted |
| [0016](0016-rust-only-public-docs.md) | Rust-only public documentation | Accepted |
| [0017](0017-gatesmith-pm-orchestration.md) | Gatesmith PM orchestration model | Accepted |
| [0018](0018-no-false-passes.md) | No false passes: every checkpoint has a machine check | Accepted |
| [0019](0019-frozen-interface-sha-locks.md) | Frozen-interface SHA locks | Accepted |
| [0020](0020-interop-diff-keystone-gate.md) | Interop-diff keystone gate | Accepted |
| [0021](0021-performance-secondary-to-correctness.md) | Performance is secondary to byte-identical output | Accepted |
| [0022](0022-testing-strategy.md) | Testing strategy: proptest, trycmd, cargo-fuzz | Accepted |
| [0023](0023-b2-scope-rust-and-format-compat.md) | Scope the B2 interop gate to Rust round-trip and format compat | Accepted |
| [0024](0024-retire-the-bash-oracle.md) | Retire the Bash oracle (full cut) | Accepted (supersedes 0001) |
| [0025](0025-keep-native-certs.md) | Keep native-certs in the scratch image | Accepted |
| [0026](0026-latest-deps-with-release-age-cooldown.md) | Adopt latest deps with a 3-day minimum-release-age | Accepted |
| [0027](0027-ssh-sftp-stores-system-openssh-wire-plumbing.md) | SSH/SFTP stores via system OpenSSH + wire-versioned plumbing | Accepted |
