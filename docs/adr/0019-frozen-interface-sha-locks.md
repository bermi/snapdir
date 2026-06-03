# 0019 — Frozen-interface SHA locks

Status: Accepted, 2026-06

## Context

The interop contract — manifest format, directory merkle, snapshot ID, checksum modes,
excludes — was frozen after the core lane landed (ADR-0002, ADR-0003). Downstream lanes
build against it. Without a mechanical guard, an accidental edit to a "frozen" core file
could silently change behaviour and break interoperability.

## Decision

Protect frozen interfaces with **SHA locks**: files recording the content hashes of the
frozen files, re-verified on every tick.

- `manifest-format.sha.lock` covers the core format files (manifest, merkle, excludes).
- A golden-fixtures lock covered the bash-era golden fixtures.

If a locked file's hash changes without an approved process, the lock check fails the
tick. Any intended change to a frozen interface requires explicit human approval and a
lock update.

## Alternatives considered

- **Rely on lane fences alone.** Rejected: fences catch out-of-lane *edits*, but a SHA
  lock also catches an in-lane edit to a file that is supposed to be immutable, and
  documents exactly which bytes are frozen.
- **No mechanical guard, code review only.** Rejected: too easy to miss a one-line drift
  in a hashing function.

## Consequences

- Frozen core behaviour cannot drift unnoticed; the lock is checked each tick.
- A deliberate frozen-interface change (e.g. the operator-approved one-line oracle fix
  in ADR-0023) is a visible, approved event.
- The golden-fixtures lock was retired when the bash fixtures were deleted; the
  `manifest-format.sha.lock` was kept and re-anchored to the Rust golden tests
  (ADR-0024).
