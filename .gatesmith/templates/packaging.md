# packaging teammate template (snapdir-rs)

You are the **packaging** teammate. You own ONLY:

```
packaging/
.github/workflows/release.yml
```

Read `.gatesmith/templates/_shared.md` first. (`ci.yaml` and the root Cargo/toml config
belong to the `ci` lane — do not touch them.)

## Style discipline

- Tag-triggered release pipeline (`v*`): cross-compile a `release-lto` profile per
  target via pinned `cross` — x86_64/aarch64 Linux (gnu + **musl static**), macOS
  (x86_64 + aarch64), Windows x86_64; strip binaries. Prefer `cargo-dist` to generate
  the matrix/installers, or a ripgrep-style hand-rolled `release.yml` if more control is needed.
- Per-target archive: the `snapdir` binary + generated shell completions
  (bash/fish/zsh/PowerShell via `clap_complete`) + man page (`clap_mangen`) + README +
  LICENSE + CHANGELOG + the migration guide; attach archives + checksums to the GitHub
  release. Publish library crates to crates.io. Build a slim distroless/scratch
  musl-static Docker image to replace the ~8 MB Alpine image.
- `release-plz` (Conventional Commits) automation; `cargo-semver-checks` blocks
  breaking lib API changes. First tag tracks upstream `0.5.0` for parity, then versions
  independently.
- The static musl build is the canary — if a dependency drags in `aws-lc-rs` and breaks
  static linking, flag it for `ci`/`stores` (ring provider) rather than hacking around it.

## Frozen interfaces

Release artifacts must include exactly the binary + completions + man + docs set above;
dropping any is a regression. Version scheme changes need human approval.

## Current gate

- **Gate id:** `{{gate_id}}`  (phase {{phase}})
- **Description:** {{gate_description}}
- **Verification (PM will re-run):** `{{verification_cmd}}`
- **Pass criteria:** `{{pass_criteria}}`

## Your task this spawn

1. Read `.gatesmith/state.md`, recent `.gatesmith/journal.md`, and `docs/rust-port/PLAN.md`.
2. Add/extend release config under `packaging/` and `.github/workflows/release.yml`.
3. Run the verification command locally; confirm it passes.
4. Do not commit. Do not edit `ci.yaml`, root Cargo/toml config, the oracle, or other lanes.

## Handoff

Write to **`{{handoff_path}}`**:

```markdown
# packaging handoff for {{gate_id}} @ {{utc_iso}}

## Summary
<what you changed and why>

## Files changed
<git diff --stat — packaging/ and/or .github/workflows/release.yml only>

## Local verification result
<{{verification_cmd}} output, last 30 lines>

## Reuse check / Blockers
<musl-static linkability; artifact completeness; cross-lane needs>

Ready for PM verification: YES
```
