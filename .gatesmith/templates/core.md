# core teammate template (snapdir-rs)

You are the **core** teammate. You own ONLY:

```
crates/snapdir-core/
```

Read `.gatesmith/templates/_shared.md` first.

## Style discipline (this is the heart of the port)

- **Library purity (jj principle):** `snapdir-core` does NO terminal I/O and reads NO
  `$HOME`/config/env for behavior. Inputs come in as parameters; errors out as typed
  `thiserror` enums (`#[from]`, `#[source]`). The CLI lane adds I/O and `anyhow`.
- **Hashing = `blake3` crate**, default path `update_mmap_rayon`, with a streamed
  single-threaded fallback flag (mmap+rayon regresses on spinning disks). Keyed mode
  uses BLAKE3 `derive_key` for `SNAPDIR_MANIFEST_CONTEXT`. Support a `--checksum-bin`
  abstraction (md5/sha256) so the full interop matrix validates — but the SHIPPED
  default is in-process blake3; never shell out to `b3sum`.
- **Manifest format is exact and frozen after Phase 2.** Line:
  `PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH`, single space, sorted by path (`sort -k5`
  semantics), `#`-comments excluded from the checksum, `D` paths end `/`, `./` relative
  vs `--absolute`. Octal perms + sizes must match `stat -f`(macOS)/`stat -c`(Linux).
  Symlinks followed by default (inherit target type/size/checksum); `--no-follow` drops them.
- **Directory checksum (do not get this wrong):** take direct children's checksums ->
  sort -> dedup -> concatenate with NO separators -> hash. Root dir checksum = snapshot ID.
- Excludes: extended-regex `--exclude`; `%system%` (forces no-follow) and `%common%`
  expand to the built-in sets from the Bash source.
- Define the **`Store` trait** (`get_manifest`, `fetch_files`, `push`) and the sharded
  path helpers here (`.objects/<h0:3>/<h3:6>/<h6:9>/<h9:>`, `.manifests/<id…>`).
- Zero-copy parsing on `&[u8]`/`&str`; reuse buffers; no per-file allocs in hot loops.
- Validate against `utils/qa-fixtures/expected-guide-commands.txt` (read-only) and the
  embedded behaviors of `./snapdir-manifest` (read-only). Use `insta` snapshots.

## Frozen interfaces

Manifest format + dir-merkle + content-addressable layout + golden fixtures freeze
when `freeze-contract` passes. After that, any change to line format, ordering,
checksum algorithm, or sharding needs a `## Proposal` -> human approval. A change here
breaks every parent hash and the interop gate.

## Current gate

- **Gate id:** `{{gate_id}}`  (phase {{phase}})
- **Description:** {{gate_description}}
- **Verification (PM will re-run):** `{{verification_cmd}}`
- **Pass criteria:** `{{pass_criteria}}`

## Your task this spawn

1. Read `.gatesmith/state.md`, recent `.gatesmith/journal.md`, and `docs/rust-port/PLAN.md` (Frozen contract section).
2. Cross-check exact behavior against `./snapdir-manifest` and the golden fixtures (READ ONLY).
3. Implement the minimum change in `crates/snapdir-core/` and add/extend tests.
4. Run the verification command locally; confirm it passes.
5. Do not commit. Do not edit the oracle or other lanes.

## Handoff

Write to **`{{handoff_path}}`**:

```markdown
# core handoff for {{gate_id}} @ {{utc_iso}}

## Summary
<what you changed and why; note any format detail confirmed against the oracle>

## Files changed
<git diff --stat — crates/snapdir-core/ only>

## Local verification result
<{{verification_cmd}} output, last 30 lines>

## Reuse check / Blockers
<confirm no shelling to b3sum; no I/O/env reads; list cross-lane needs>

Ready for PM verification: YES
```
