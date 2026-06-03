# 0008 — Freeze the catalog JSON output format

Status: Accepted, 2026-06

## Context

While the catalog's storage moved from SQLite to redb (ADR-0007), the catalog's
JSON-line *output* (`snapdir locations`/`ancestors`/`revisions`) is user- and
script-visible and must stay compatible with the Bash oracle's `sqlite3`-produced JSON.
The original task brief had the `revisions` shape wrong, which had to be corrected
against the oracle.

## Decision

Freeze the catalog JSON-line output to be byte-identical to the oracle's `sqlite3`
`json_object` output:

- `locations` → `{"created_at","id","location"}` (latest id per location).
- `ancestors` → `{"created_at","id","location"}` where `id` is the previous id,
  ordered `created_at DESC`.
- `revisions` → `{"created_at","id","previous_id"}` (no `location`), ordered
  `created_at DESC`.
- Timestamp format `YYYY-MM-DD HH:MM:SS.SSS` (`STRFTIME('%Y-%m-%d %H:%M:%f')`).

Serialization is compact (no spaces), with exact key order via dedicated serde structs;
a null `previous_id` renders as bare `null`. A golden test drives the frozen
`snapdir-sqlite3-catalog` read-only and asserts byte-identical lines (neutralizing the
`NOW()` timestamp).

## Alternatives considered

- **A cleaner Rust-native JSON shape.** Rejected: breaks compatibility with scripts
  consuming the existing output.
- **Pretty-printed JSON.** Rejected: not byte-identical to the oracle's compact output.

## Consequences

- Catalog output stays a drop-in replacement for the Bash tool's.
- The three shapes are fixed by serde struct definitions; changing a key name or order
  is a format break.
- The `revisions` shape intentionally omits `location` (matching the oracle), unlike
  `locations`/`ancestors`.
