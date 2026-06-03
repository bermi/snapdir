# 0023 — Scope the B2 interop gate to Rust round-trip and format compat

Status: Accepted, 2026-06

## Context

The remote-store interop model proves byte-identical, bidirectional interop by running
cross-tool push/fetch in both directions (Rust↔Bash) against a real backend. This was
fully achieved on S3 (MinIO) and GCS (a real bucket). The B2 lane, run end-to-end for
the first time, surfaced a chain of failures — all in the **frozen Bash oracle's**
cold-cache fetch-from-B2 path (legacy bash driving the `b2` CLI), a path never exercised
before:

- The installed `b2` CLI v4 removed the v3 subcommands the oracle's `snapdir-b2-store`
  calls (worked around with a pinned v3 shim in the harness).
- The oracle emitted `--store`-less object-fetch worker commands (line 395, then line
  169's trailing `ensure-no-errors`), so cold Bash fetch failed "Missing --store".
- Even with those patched in a throwaway copy, the legacy parallel `b2`-CLI
  fetch/checkout path still failed silently.

The shipped Rust binary, by contrast, passed every B2 operation live (push/fetch/pull/
verify), and Bash derives the same snapshot id from a Rust-pushed B2 store.

## Decision

Scope the `remote-interop-b2` gate (2026-06-03) to what is proven and meaningful for the
**port**:

1. Rust round-trip against real B2 (push → fetch → pull → verify) must pass.
2. Rust↔Bash snapshot-id agreement (Bash derives the same id from a Rust-pushed B2
   store) must pass — proving format/key/id compatibility.
3. The legacy Bash-oracle cold-cache fetch-from-B2 is a **documented known limitation**
   of the retired tool: reported loudly, but it does not hard-fail the B2 lane.

S3 and GCS keep the full bidirectional differential, unchanged. Only the B2 lane is
scoped. One operator-approved one-line oracle fix (line 395 `--store`) was applied under
a temporary deny-lift, then the oracle was re-locked.

## Alternatives considered

- **Chase the legacy-bash B2 fetch bugs to full bidirectional green.** Rejected: they
  are in code being retired (ADR-0024); drip-feeding more frozen-oracle one-liners
  through repeated deny-lifts is not worthwhile.
- **Silently rubber-stamp the B2 lane.** Rejected: forbidden by ADR-0018; the limitation
  is reported loudly instead.

## Consequences

- B2 is proven for the shipped tool (Rust round-trip + format/id compatibility).
- Full bidirectional byte-identical interop remains proven on S3 and GCS.
- The legacy bash cold-fetch-from-B2 limitation is documented, not hidden, and becomes
  moot once the oracle is retired (ADR-0024).
