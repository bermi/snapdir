# 0026 — Adopt latest deps with a 3-day minimum-release-age

Status: Accepted, 2026-06

## Context

Phase 11 modernizes the dependency tree to current versions, including unifying the TLS
and SDK stack. At the same time, immediately adopting a just-published crate version is a
supply-chain risk: malicious or broken releases are most often caught and yanked within
the first days. A balance is needed between staying current and not being the first to
run a brand-new release.

## Decision

Adopt latest dependencies, but never a crate version younger than **3 days** (a
cooldown):

- Configure Dependabot cooldown (`default-days: 3`) / Renovate `minimumReleaseAge`
  ("3 days").
- Add a `check-crate-age.sh` CI check enforcing that adopted versions have a crates.io
  `created_at` at least 3 days old.

Alongside the cooldown, perform the full TLS/SDK upgrade: unify on **rustls 0.23**,
**hyper-rustls 0.27**, a **hyper 1.x** connector, the latest AWS SDK, and **unpin
`google-cloud-storage`** — while preserving the `ring` provider, no aws-lc-rs
(ADR-0004), and native-certs (ADR-0025).

## Alternatives considered

- **Adopt new versions immediately.** Rejected: exposes the build to brand-new releases
  before the ecosystem has had time to catch problems.
- **A long pin / manual-only updates.** Rejected: lets the tree drift stale; the cooldown
  keeps it current with a short safety window.
- **Switch crypto providers during the upgrade.** Rejected: the ring/no-aws-lc constraint
  (ADR-0004) is preserved through the upgrade; the musl static build must stay clean.

## Consequences

- Dependencies stay current without adopting day-zero releases; a `>= 3 days` age is
  enforced in CI.
- The TLS/SDK stack is unified on rustls 0.23 / hyper-rustls 0.27 / hyper 1.x, with
  `google-cloud-storage` unpinned, still ring-only and aws-lc-free.
- Dependabot/Renovate config plus the crate-age check are part of the repository's
  supply-chain hardening.
