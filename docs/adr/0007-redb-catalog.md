# 0007 — Replace the SQLite catalog with redb

Status: Accepted, 2026-06

## Context

The Bash version keeps a catalog of snapshot locations/ancestors/revisions in SQLite,
driven by the `sqlite3` CLI (`snapdir-sqlite3-catalog`). Shelling out to `sqlite3` is a
runtime dependency the zero-dependency binary cannot have (ADR-0005). The catalog is
local bookkeeping — it is not part of the interop contract; only its JSON-line *output*
is observed by users.

## Decision

Replace the SQLite catalog with **redb**, a pure-Rust embedded key/value store. The
on-disk format is private and fully rebuildable (`snapdir catalog rebuild`); there is no
on-disk interop with the old SQLite database and no SQLite→redb importer. Only the
JSON-line output is format-frozen (see ADR-0008).

Because redb has no SQL query planner, the catalog uses explicit range scans over
purpose-built tables: a `records` table keyed by `(created_at, seq)`, a `loc_head`
table for O(1) latest-per-location, and prefix reverse-range scans for the
`created_at DESC` ordering of `revisions`/`ancestors`. A monotonic `seq` and a
lexically-sortable `created_at` keep ordering deterministic; the clock is injectable.

## Alternatives considered

- **Bundle SQLite via `rusqlite`.** Rejected: heavier dependency and, in some build
  configurations, C/system linkage concerns at odds with the clean static musl goal;
  redb is pure Rust.
- **Keep shelling out to `sqlite3`.** Rejected: runtime dependency, against ADR-0005.
- **Import the old SQLite DB.** Rejected: the catalog is rebuildable from the store, so
  an importer is unnecessary complexity.

## Consequences

- No runtime SQLite dependency; the catalog is pure Rust.
- The catalog is disposable and rebuildable from the store's manifests.
- Queries are hand-written range scans rather than SQL, which must reproduce the
  oracle's ordering exactly (covered by tests).
