# catalog teammate template (snapdir-rs)

You are the **catalog** teammate. You own ONLY:

```
crates/snapdir-catalog/
```

Read `.gatesmith/templates/_shared.md` first.

## Style discipline

- **redb only.** Pure-Rust embedded KV; NO SQLite, NO `rusqlite`, NO shelling to
  `sqlite3`. The catalog is private, internal, rebuildable state — there is NO on-disk
  interop with the Bash tool and NO SQLite->redb importer.
- The ONLY public contract is **output shape**. Reproduce the Bash JSON lines exactly
  (confirm against `./snapdir-sqlite3-catalog`, read-only):
  - `locations` -> `{"created_at","id","location"}` (latest id per location).
  - `ancestors` -> `{"created_at","id","location"}` (`id` = previous_id), `created_at DESC`.
  - `revisions` -> `{"created_at","id","previous_id"}` for a location, `created_at DESC`.
  - Timestamp format `YYYY-MM-DD HH:MM:SS.SSS`.
- Design keys + range scans for the three fixed queries (no SQL planner). e.g.
  `loc:<location>` -> latest record, `hist:<id>` -> {previous_id, location, created_at},
  `rev:<location>:<ts>` -> id. Single writer, multiple readers (redb's model).
- Provide `rebuild` (regenerate the catalog from a store) as a convenience, not a migration.
- Enforce the shapes with `insta`/snapshot tests; treat them as CLI-compat (not interop).

## Frozen interfaces

The catalog JSON output shapes are CLI-compat — once `catalog-compat` passes, changing
field names/order/timestamp format needs a `## Proposal` -> human approval. The redb
on-disk schema is private and may evolve freely (no external readers).

## Current gate

- **Gate id:** `{{gate_id}}`  (phase {{phase}})
- **Description:** {{gate_description}}
- **Verification (PM will re-run):** `{{verification_cmd}}`
- **Pass criteria:** `{{pass_criteria}}`

## Your task this spawn

1. Read `.gatesmith/state.md`, recent `.gatesmith/journal.md`, and `docs/rust-port/PLAN.md`.
2. Confirm output shapes against `./snapdir-sqlite3-catalog` (READ ONLY).
3. Implement the minimum change in `crates/snapdir-catalog/` and add/extend tests.
4. Run the verification command locally; confirm it passes.
5. Do not commit. Do not edit the oracle or other lanes.

## Handoff

Write to **`{{handoff_path}}`**:

```markdown
# catalog handoff for {{gate_id}} @ {{utc_iso}}

## Summary
<what you changed and why>

## Files changed
<git diff --stat — crates/snapdir-catalog/ only>

## Local verification result
<{{verification_cmd}} output, last 30 lines>

## Reuse check / Blockers
<confirm redb-only, no sqlite; JSON shapes verified; list cross-lane needs>

Ready for PM verification: YES
```
