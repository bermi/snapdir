# cli handoff for cli-list-options-multi @ 2026-06-04T00:35:01Z

## Summary

Made the list-valued `--exclude` CLI option accept BOTH repeated occurrences
(`--exclude a --exclude b`) AND comma-delimited values (`--exclude a,b`),
OR-combined (a path is excluded if it matches ANY pattern). Additive and
backward-compatible.

Changes in `crates/snapdir-cli/src/cli.rs` only (the CLI lane CALLS core; no
core edits):

- `GlobalArgs.exclude` and the `Manifest` subcommand's `exclude` both changed
  from `Option<String>` to `Vec<String>` with
  `#[arg(... action = clap::ArgAction::Append, value_delimiter = ',')]`.
  `value_delimiter = ','` gives comma-splitting; `Append` gives the repeated
  form. Doc comments were kept to a single line (the multi-value prose lives in
  regular `//` comments) so the `--help` byte-output stays identical and the
  frozen `cli_surface` trycmd snapshots still pass.
- `GlobalArgs.paths` (a DEAD flag — confirmed consumed nowhere via grep) was
  given the same `Vec<String>` arity for surface consistency, with a code
  comment noting it remains UNWIRED (no `--paths` filtering — out of scope).
- **OR-combine + per-pattern macro expansion:** new free fn `combine_excludes`
  expands EACH pattern independently via core `expand_excludes(...)` (so the
  `%system%`/`%common%` macro tokens are never split by a naive `|`-join of raw
  patterns), wraps each expanded ERE in a non-capturing `(?:...)` group, and
  joins the groups with `|` into ONE combined ERE fed to a single
  `ExcludeMatcher`. `forces_no_follow` is the OR of every pattern's flag. Empty
  list → `pattern: None` (no filtering, exactly as before). A single pattern
  produces `(?:<expansion>)`, which matches the identical set as the old bare
  `<expansion>` (the group changes grouping only, never the matched set).
- `build_manifest`'s param changed from `exclude: Option<&str>` to
  `exclude: &[String]`; the expansion/combine now happens inside it. All call
  sites updated: `manifest` (precedence preserved — subcommand list if
  non-empty, else global list), `id`, `push`, `stage` pass
  `&self.globals.exclude` (or the subcommand list).
- Imported `ExpandedExclude` from `snapdir_core` (already re-exported) for the
  combine fn's return type.

## Files changed

```
 crates/snapdir-cli/src/cli.rs | 122 ++++++++++++++++++++++++++++++++++--------
 1 file changed, 100 insertions(+), 22 deletions(-)
```

Plus new test file (untracked): `crates/snapdir-cli/tests/list_options.rs`.

## Local verification result

```
     Running tests/list_options.rs (target/debug/deps/list_options-681c073b90aae37e)

running 7 tests
test list_options_single_exclude_unchanged ... ok
test list_options_repeated_exclude ... ok
test list_options_mixed_comma_and_repeated ... ok
test list_options_comma_delimited ... ok
test list_options_subcommand_overrides_global_exclude ... ok
test list_options_macro_combined_with_literal ... ok
test list_options_repeated_and_comma_match_same_set ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.45s
```

`grep -q 'ArgAction::Append' crates/snapdir-cli/src/cli.rs` → matches (literal
present at both global and subcommand `--exclude`). Full
`cargo test -p snapdir-cli --locked` → all suites green (cli_surface trycmd
snapshots unchanged). `cargo clippy -p snapdir-cli --all-targets --all-features
--locked -- -D warnings` → clean. `cargo fmt -p snapdir-cli -- --check` → clean.

Tests cover (all named with `list_options`): single `--exclude` (unchanged),
repeated multi-arg, comma list, mixed comma+repeated, comma-vs-repeated-match-
same-set, a `%common%` macro combined with a literal (drops a `node_modules/`
subtree — proves per-pattern macro expansion), and global-vs-subcommand
precedence. (A `%system%`-macro test was dropped: `%system%` matches against the
ABSOLUTE path and forced an empty manifest on this machine's temp root, making
it environment-fragile; `%common%` already proves per-pattern expansion.)

## Reuse check / Blockers

- No core edits — `excludes.rs`/`manifest.rs`/`merkle.rs` untouched
  (`git diff --stat` shows only `crates/snapdir-cli/src/cli.rs`).
- `expand_excludes` reused from `snapdir-core` (called per pattern); the matcher
  is the existing `ExcludeMatcher`.
- Single-pattern behavior is identical: `(?:<expansion>)` matches the same set
  as the prior bare `<expansion>` (verified by `list_options_single_exclude_*`
  and the unchanged `manifest_exclude_golden` + cli_surface snapshots).
- `--paths` is arity-only (Vec) and remains explicitly UNWIRED per scope.
- clippy + fmt clean. No cross-lane needs; PM commits.

Ready for PM verification: YES
