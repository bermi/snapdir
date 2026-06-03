# 0017 — Gatesmith PM orchestration model

Status: Accepted, 2026-06

## Context

The port is a large, multi-component effort (core, catalog, stores, CLI, tests, CI,
benches, docs, packaging) with a hard interop contract. It needs an execution model that
keeps changes traceable, prevents scope creep across components, and does not let
under-verified work be marked done.

## Decision

Drive the port with the **Gatesmith** project-management model: a deterministic gate
ledger executed one tick at a time.

- Work is decomposed into **gates** with explicit dependencies, an owning **lane**, and
  a machine-checkable **pass-criteria** DSL (exit codes, regex matches).
- Each tick spawns **at most one teammate** for one gate; the teammate stays inside its
  **lane** (a fenced set of files). An out-of-lane diff fails the fence.
- The PM verifies the pass-criteria and only then commits; teammates never commit.
- Frozen interfaces are protected by **SHA locks** re-verified each tick (ADR-0019).
- The model forbids **false passes**: every human checkpoint must be backed by a machine
  check (ADR-0018).

The plan ledger is itself the authoritative plan; the journal is an append-only audit
log written before any gate mutation.

## Alternatives considered

- **Ad-hoc multi-file edits.** Rejected: no lane fences, no traceability, easy scope
  creep across the interop boundary.
- **Trust human sign-off alone.** Rejected: human-confirm-only gates produced near
  false passes (ADR-0018).

## Consequences

- Every change is attributable to a gate, an owner, a commit, and evidence.
- Lane fences kept the frozen oracle and `crates/**` boundaries intact (e.g. a premature
  PM hand-edit was reverted so the change could go through its gate — ADR-0014).
- The discipline added overhead (gate authoring, lane routing) in exchange for
  traceability and no silent drift.
