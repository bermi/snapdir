# 0016 — Rust-only public documentation

Status: Accepted, 2026-06

## Context

The repository carried a bash-era public documentation set: a Retype website
(`docs/index.md`, `install.md`, `guide.md`, `authoring-stores.md`,
`understanding-manifests.md`, per-script readmes, `docs/api/**`, `docs/images/**`,
`retype.yml`) plus a README and CONTRIBUTING framed around installing and running the
shell scripts. Those docs also carried known bugs. After the port, this material
describes a tool that no longer ships.

## Decision

Make the public documentation Rust-only:

- Delete the bash-era public docs (the Retype site, the per-script readmes, `docs/api/**`,
  `docs/images/**`, `retype.yml`).
- Rewrite `README.md` (Rust-only, discovery-oriented, no bash-install/script framing,
  no AI/historical reasoning) and `CONTRIBUTING.md` (a Rust contributor guide: cargo
  build/test/fmt/clippy, workspace layout, MSRV, the frozen-oracle note).
- Keep `docs/rust-port/**` (manifest spec, CHANGELOG, migration guide).
- Fix the known doc bugs in the Rust docs (`--linked` not `--link`; `ensure-no-errors`
  not `verify-transactions`).

The frozen oracle scripts themselves are never edited — only their docs.

## Alternatives considered

- **Maintain the bash-era docs alongside the Rust docs.** Rejected: they describe a
  retired tool and would mislead users.
- **Delete all docs and start over.** Rejected: `docs/rust-port/**` (spec, migration
  guide, CHANGELOG) is the accurate, transitional documentation and is kept.

## Consequences

- Public docs describe only the shipped Rust tool.
- A transitional migration guide remains for users coming from the Bash version.
- A stale Makefile docs-site target was left dangling on the deleted assets and was
  later cleaned up (the Makefile became a thin cargo wrapper).
