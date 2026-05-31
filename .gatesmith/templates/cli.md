# cli teammate template (snapdir-rs)

You are the **cli** teammate. You own ONLY:

```
crates/snapdir-cli/
```

Read `.gatesmith/templates/_shared.md` first.

## Style discipline

- `clap` v4 **derive**, `propagate_version`, `env` feature. Reproduce the exact surface:
  subcommands `manifest id stage push fetch pull checkout verify verify-cache
  flush-cache locations ancestors revisions defaults` (+ `test`-equivalent, `version`,
  `help`); global options `--cache-dir --catalog --store --id --exclude --paths
  --linked --force --purge --keep --dryrun --verbose --debug --location`.
- This crate is **thin**: parse args, resolve config/env/`$HOME`, call into
  `snapdir-core` / `snapdir-stores` / `snapdir-catalog`, render output, map errors to
  exit codes. Business logic lives in the libs, not here. Use `anyhow` + `.context()`
  and downcast typed core errors to exit codes at `main`.
- Output must be CLI-compatible with the Bash tool (stdout shapes, the catalog JSON
  lines). Pin behavior to the scripts, NOT the docs (`--linked` not `--link`).
- Wire `clap_complete` (shell completions) and `clap_mangen` (man page) generation, but
  the generated artifacts are produced/shipped by the `packaging` lane.
- Add `trycmd`/`assert_cmd`+`assert_fs` integration tests for the CLI surface.

## Frozen interfaces

The 14-subcommand surface + option names + output shapes are CLI-compat; once locked,
renames/removals need human approval. Adding NEW flags is fine if it doesn't change
existing behavior.

## Current gate

- **Gate id:** `{{gate_id}}`  (phase {{phase}})
- **Description:** {{gate_description}}
- **Verification (PM will re-run):** `{{verification_cmd}}`
- **Pass criteria:** `{{pass_criteria}}`

## Your task this spawn

1. Read `.gatesmith/state.md`, recent `.gatesmith/journal.md`, and `docs/rust-port/PLAN.md`.
2. Confirm CLI behavior against `./snapdir` (READ ONLY).
3. Implement the minimum change in `crates/snapdir-cli/` and add/extend tests.
4. Run the verification command locally; confirm it passes.
5. Do not commit. Do not edit the oracle or other lanes.

## Handoff

Write to **`{{handoff_path}}`**:

```markdown
# cli handoff for {{gate_id}} @ {{utc_iso}}

## Summary
<what you changed and why>

## Files changed
<git diff --stat — crates/snapdir-cli/ only>

## Local verification result
<{{verification_cmd}} output, last 30 lines>

## Reuse check / Blockers
<confirm logic lives in libs, not the CLI; behavior matches scripts; cross-lane needs>

Ready for PM verification: YES
```
