# 0014 — Remove `verify --purge`

Status: Accepted, 2026-06

## Context

In the Rust CLI, `snapdir verify` is store-based: it resolves the store, fetches the
manifest, and verifies. The global `--purge` flag was accepted by `verify` but was inert
— `run_verify` never read it. There was also a semantic divergence from the oracle: the
oracle's `verify` is a *cache* operation that runs integrity checks over the local cache
and, with `--purge`, removes corrupt cache objects. The Rust `verify` is store-based
instead. Implementing a store-side purge would require a `Store::delete` the trait does
not have; a cache-side purge already exists as `verify-cache --purge`.

## Decision

Make `snapdir verify` **reject** `--purge` with a clear error rather than silently
ignore it:

> `snapdir: \`verify\` does not support --purge; use \`verify-cache --purge\` to remove
> corrupt objects from the local cache`

The rejection happens before the store is resolved. Normal `verify` (without `--purge`)
is unchanged. No cache or store purge is added to `verify`.

## Alternatives considered

- **Leave the flag inert.** Rejected: a silently-ignored flag is a trap for users who
  expect it to do something.
- **Implement store-side purge** (add `Store::delete`). Rejected: out of scope, and the
  correct purge target for snapdir is the cache, which `verify-cache --purge` already
  covers.
- **Make `verify` cache-based like the oracle.** Rejected: `verify` was already wired
  and signed off as store-based; re-pointing it would undo accepted behaviour.

## Consequences

- `--purge` has one clear home: `verify-cache`.
- Users get an actionable error instead of a no-op.
- The decision was implemented through the gate (a premature direct edit was reverted
  for traceability — see ADR-0017).
