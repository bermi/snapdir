# Catalog default-on + `revisions --catalog none` — design lock (1.9.0)

Gate: `catalog-default-design` (phase 31, human ✋). Locks the semantics for the catalog
fix cluster (`catalog-default-spec-tests` → `catalog-default-impl-cli` → `catalog-default-review`).

## Problem (root cause)

`snapdir revisions --catalog none --location <path>` returns nothing even after the user
created multiple snapshots **without** specifying a catalog. Two asymmetric bugs in
`crates/snapdir-cli/src/cli.rs`:

1. **Writes without `--catalog` log nowhere.** `catalog_db_path()` (~L2109) returns `None`
   when `--catalog` is unset (the `as_deref()?` short-circuits), so `log_event()` silently
   no-ops — snapshots are never recorded to any catalog.
2. **`--catalog none` opens a literal empty DB.** The string `"none"` (3 chars, non-empty,
   no path separator) is treated as a **bare adapter name** → opens/creates
   `<cache_dir>/none-catalog.redb`, which is always empty because nothing was ever logged
   there.

Net: the write path and the read path point at different (or no) catalogs, so `revisions`
can never surface snapshots taken without an explicit `--catalog`.

## Locked semantics (operator-approved during planning, 2026-06-17)

1. **Default-on catalog.** When `--catalog` is **unset**, resolve to a single **default
   catalog** at `<cache_dir>/default-catalog.redb`:
   - **`push` and `stage` auto-record** to it (the state-changing snapshot commands).
   - **Query commands** (`revisions`, `locations`, `ancestors`) **read** that same default
     when `--catalog` is unset — so a no-flag `push`/`stage` round-trips to a no-flag
     `revisions`.
   - **`manifest` logs only with an explicit `--catalog`** (unchanged); **`id` never logs**
     (it has no catalog arg). This keeps `manifest`/`id` usable in pipelines/scripts without
     a surprise DB write.

2. **`none` / empty = explicit DISABLE sentinel.** `--catalog none` (and `--catalog ""`)
   means "no catalog": `catalog_db_path()` returns `None` (no logging, no DB created).
   `revisions --catalog none` (and `locations`/`ancestors`) print a clear, distinct
   **"catalog disabled (--catalog none)"** message and exit 0 — NOT a silent empty result
   and NOT a fabricated success. This is distinguishable from the existing
   "no revisions for `<location>`" empty-but-enabled case.

3. **Explicit named / path catalogs unchanged.** `--catalog foo` → bare name →
   `<cache_dir>/foo-catalog.redb`; `--catalog ./x.redb` (contains a separator) → that path
   verbatim. A named catalog is isolated from the default (querying `foo` does not see
   `default-catalog.redb` rows and vice-versa).

4. **`defaults` surfaces it.** `snapdir defaults` shows the resolved `catalog` knob with its
   value + `source={flag|env|default}` (consistent with the Phase-30 effective-config
   rewrite).

## Precedence

`--catalog <v>` flag  >  `SNAPDIR_CATALOG` env  >  **default** (`<cache_dir>/default-catalog.redb`).
The sentinel check (`v == "none" || v.is_empty()` → disabled) applies to whichever of
flag/env supplies the value.

## Keystone / invariants (must hold)

- **`manifest`/`id` stdout BYTE-IDENTICAL** with and without catalog logging — the catalog is
  a pure side effect; snapshot ids are unaffected.
- Frozen manifest/merkle/excludes format untouched (this is a CLI-resolution change only).
- No change to the catalog on-disk redb schema (`crates/snapdir-catalog`); only WHICH db path
  the CLI resolves to and the disabled-sentinel handling.

## Back-compat note (why this is a minor, not breaking)

- **Additive for the common case:** `revisions` starts returning data for snapshots taken
  without a flag — strictly more useful; nothing relied on the prior emptiness.
- **`--catalog none` semantics change** from "use `none-catalog.redb`" to "disabled." Nothing
  sane relied on a literal catalog named `none`; the new behavior is the obviously-correct
  reading of the sentinel. Documented in CHANGELOG.
- A new file `<cache_dir>/default-catalog.redb` may appear after a no-flag `push`/`stage`. It
  lives under the cache dir (GC-able, same place as other catalogs) and is opt-out via
  `--catalog none`.

## Test surface the spec gate must pin (`catalog-default-spec-tests`)

- (a) no-flag `push` + `stage` → no-flag `revisions --location X` lists both.
- (b) `--catalog none` records nothing; `revisions --catalog none` prints the disabled message, exit 0.
- (c) `--catalog foo` isolation from the default (both directions).
- (d) `defaults` shows the catalog knob + source.
- (e) KEYSTONE: `manifest`/`id` stdout byte-identical with/without logging.
- (f) path-like `--catalog ./x.redb` still works.

All tests use an isolated `HOME`/`XDG_CACHE_HOME` so the default catalog is sandboxed.
