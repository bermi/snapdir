# 0011 — Package with cargo-dist and musl-static targets

Status: Accepted, 2026-06

## Context

The port must ship per-target archives for a single statically-linked binary, plus shell
completions and a man page. The static musl build is also the canary for the aws-lc
constraint (ADR-0004): if any banned crypto provider sneaks into the graph, the static
musl link is where it fails.

## Decision

Use **cargo-dist** for packaging, with cross-compiled per-target archives including
`x86_64`/`aarch64` `unknown-linux-musl` (static). The release builds with
`--profile dist`, so a `[profile.dist]` table is defined in the **root** `Cargo.toml`
(inheriting `release`, with `lto = fat`, `codegen-units = 1`, `opt-level = 3`,
`panic = abort`, `strip = true`). Each archive bundles the binary plus completions,
man page, README, LICENSE, CHANGELOG, the migration guide, and a sha256.

## Alternatives considered

- **Hand-rolled release shell scripts** (the bash-era approach). Rejected: cargo-dist
  gives a maintained cross-target matrix, installers, and checksums for free.
- **Keep `[profile.dist]` only in the cargo-dist workspace file.** Rejected: cargo
  requires a profile referenced by `--profile dist` to be defined at the workspace
  root, so the build failed until the profile was added to the root `Cargo.toml`. This
  was caught by the release dry-run.

## Consequences

- Releases are produced by a reproducible cross-target matrix.
- The musl-static target doubles as the standing aws-lc/openssl canary.
- The dist profile lives at the workspace root, where `--profile dist` requires it.
