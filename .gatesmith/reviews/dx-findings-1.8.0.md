# snapdir 1.8.0 — Adversarial DX/UX findings (judged)

**Judge:** Gatesmith PM. **Date:** 2026-06-15. **Target:** installed `snapdir 1.7.0`, black-box.
**Sources:** `.gatesmith/reviews/dx-arg-matrix.md` (objective command×flag audit) + 6 sealed
persona reports under `.gatesmith/evidence/dx-personas/persona-{1..6}-*.md`. Findings below are
**deduped across sources**; each cites its source(s), a severity, a one-line repro, and the
**fix cluster** it maps to (the 4 planned 1.8.0 clusters, or **NEW** = beyond the planned DX
scope → operator decision at `dx-findings-signoff`).

---

## 1. Calibration coverage — is the harness genuinely adversarial?

The 6 personas were **sealed**: each got only its goal + black-box rules, was **not** told any
known smell, ran in isolation, and could not read source or each other's notes. The operator's
3 pre-known smells were held only by the judge. Result — **calibration 3/3**, all independently
rediscovered by personas (separate channel from the objective arg-matrix):

| Known smell | Independently rediscovered by | Verbatim |
|---|---|---|
| Progress stuck at 0% / useless | **persona-3 (perf tuner)** F1 (major) | "denominator is the byte count not files, the bar never fills, % is frozen at 0%" |
| `defaults` is cruft / missing real knobs | **persona-6 (newcomer)** F2 (major), persona-3 F6 | "prints just 4 env vars… none of the defaults a newcomer wants" |
| Flags do nothing / `--debug`/`--verbose` | **persona-3** F3, **persona-1** F2 | "`--debug` prints *less* than `--verbose`"; "`SAVED` only with `--verbose`, inconsistent" |

**Verdict: the harness IS operating in adversarial mode (no harness gap).** Stronger evidence: the
unbiased personas surfaced **more** than the seed set — including two CI-hostile *silent-wrong*
behaviours and several correctness gaps the operator had not flagged (§6). No persona needed to be
re-run; coverage of the realistic goal-space was good. One caveat logged: the sandbox `tree/` has
**no symlinks**, so `--no-follow` / `--linked` follow-semantics were under-exercised (a future
sandbox could add symlinks).

---

## 2. Cluster 1 — Argument hygiene & validation (approach B)  ← the core of 1.8.0

Root cause (objective, arg-matrix §"Structural note"): **clap flattens all ~28 global flags onto
every subcommand**, so acceptance ≠ effect. Findings:

- **[MAJOR] Dozens of accepted-but-ignored flags.** Every transfer/network/staging flag
  (`--limit-rate --jobs --walk-jobs --adaptive --max-jobs --max-retries --retry-* --max-requests
  --keep --purge --linked --force --dryrun`) is accepted and SILENT-NOOP on commands that neither
  transfer, stage, nor walk-for-write: `id`, `manifest`, `defaults`, `verify-cache`, `flush-cache`,
  `locations`/`ancestors`/`revisions`, and the transfer/walk subset on `diff`. *Repro:* `snapdir id
  --limit-rate 1M --adaptive --max-retries 9 $T` == `snapdir id $T`. *(arg-matrix #5,#6; personas
  1,3,5,6 all hit the "undifferentiated ~30-option help" form of this.)* → approach-B per-command
  groups make clap reject these + scope each `--help`.
- **[MAJOR] `--debug` is a universal ghost** (0 observable effect on any command). *Repro:* `snapdir
  stage --debug $T` ≡ `snapdir stage $T`. *(arg-matrix #2; persona-3 F3.)* → **remove**.
- **[MAJOR] `--paths` filters nothing on `manifest`/`id`.** Identical output/hash for matching,
  partial, and deliberately non-matching patterns; help promises "Only include paths matching
  PATTERN" (contrast `--exclude`, which works). *Repro:* `snapdir id --paths zzz_nomatch $T` ≡
  `snapdir id $T`. *(arg-matrix #1.)* → either implement or remove+document; at minimum it must not
  silently claim to filter. **Borderline NEW (unimplemented feature), but lives in the flag surface.**
- **[MAJOR] `manifest --id <ID>` silently ignores `--id` and re-walks the cwd**, printing unrelated
  content as if it were the snapshot (78k lines of the repo from repo-root). A user asking for a
  snapshot's manifest gets a *different directory's* manifest, exit 0. *(persona-5 F9.)* → `--id`
  must be rejected on `manifest` (approach B) or honored.
- **[MINOR] `--verbose` coverage gap (4/16).** EFFECTIVE on stage/push/fetch/checkout; silent on
  manifest/id/verify/verify-cache though advertised identically. *(arg-matrix #3; persona-1 F2.)* →
  keep universal but document which commands honor it; make confirmations consistent (§5).
- **[MINOR] Inconsistent value validation.** `--jobs`/`--adaptive`/`--max-retries` reject bad values
  (exit 2), but **`--limit-rate bogus`** and **`--color bogus`** are silently accepted (exit 0) —
  `--color` even has a documented `auto/always/never` enum that isn't enforced. *(arg-matrix #8.)*
- **[MINOR] `checkout --linked` yields a plain regular file** (link count 1, distinct inode), not a
  symlink/hardlink; help says "Use symlinks instead of copies." *(arg-matrix #7.)* → implement or
  fix help. **Borderline NEW (unimplemented).**
- **[MINOR] `--catalog` accepts any string silently** (`json`, `sql`, typos → exit 0, empty output),
  so a typo'd adapter is indistinguishable from a real one; valid values undocumented. *(persona-6
  F3.)*
- **[NIT] `--max-jobs` silently ignored without `--adaptive`; `--dryrun` accepts bogus protocols
  (`mem://`,`ssh://`) without validation.** *(persona-3 F5,F7.)*

## 3. Cluster 2 — `defaults` rewrite

- **[MAJOR] `defaults` prints ~4–5 near-static lines** (binary path twice + two empty legacy
  `SNAPDIR_MANIFEST_*` vars + the env cache-dir) and **none** of the real effective knobs
  (cache-dir resolved value, store, catalog, jobs/auto-resolved value, walk-jobs, limit-rate,
  retries, fsync, clonefile…). It also **ignores every override flag it accepts**, including its own
  `--cache-dir`. *Repro:* `diff <(snapdir defaults) <(snapdir defaults --cache-dir /tmp/xyz)` → no
  diff. *(arg-matrix #4; persona-6 F2; persona-3 F6.)* → rewrite to print every effective knob with
  resolved value + source {flag|env|default}; still surface any set `SNAPDIR_*`.

## 4. Cluster 3 — Progress observability

- **[MAJOR] Progress line is nonsense.** The "files" denominator is actually the **total byte
  count** (e.g. `209715200 files` = 200 MiB; `31383488 files` = the 32 MB tree), the byte total
  reads up to absurd values (`2.4 PB`), the **bar never fills, and % is frozen at 0%**. The one
  "how far along am I?" affordance reports garbage. *(persona-3 F1.)* → the planned fix (visible
  discovery phase + `set_total(file count)` before hashing + a real %); ensure the denominator is
  **files**, not bytes.
- **[MINOR] `--walk-jobs` is never echoed** (only transfer concurrency is confirmed under
  `--verbose`); a tuner can't confirm it took effect. *(persona-3 F4.)*
- **[NIT] Progress silently absent on non-TTY with no hint.** *(persona-3 F8.)*

## 5. Cluster 4 — Error messages, silent-success & consistency

- **[MAJOR] Missing-store / split-store errors give no actionable hint.** `push`/`pull`/`verify`
  without `--store` → terse "missing --store option"; forgetting `--from-objects` on a split source
  → raw `object not found: <hash>` that never mentions the split-store concept or the fix. *(persona-2,
  persona-4 F2.)*
- **[MAJOR] `verify --help` says "Verify the integrity of a *staged* snapshot" but it checks the
  *store*** and requires `--store`+`--id`; during cache corruption it returned a reassuring exit 0
  because it was silently verifying the healthy store. Misleading help → **false reassurance**.
  *(persona-5 F5; persona-6 F4.)* → fix help text (+ consider a cache-verify alias clarity).
- **[MAJOR] Integrity errors name the object *hash*, never the *file path(s)*** — with dedup one
  object backs many files; the user must grep the manifest by hand. *(persona-5 F6.)*
- **[MINOR] Silent success is inconsistent.** `pull`/`checkout`/`verify`/`verify-cache` print nothing
  on success while `stage`/`push` echo the id; a first-timer can't tell a 2089-file restore happened.
  *(personas 1,4,5,6.)*
- **[MINOR] `file://` store-URI scheme is never shown in help** (only abstract
  `protocol://location/path`); a bad guess errors `invalid store protocol` naming no alternatives —
  an onboarding wall. *(personas 1,4,6.)* → list valid schemes in help + the error.
- **[NIT] `fetch --dryrun` is silent** while stage/push/checkout print a "dry-run: would…" line.
  *(arg-matrix #9.)*

## 6. NEW — correctness/behaviour findings beyond planned DX scope (operator decision at sign-off)

These exceed "DX polish"; the judge surfaces them for the operator to **fix in 1.8.0 / defer /
investigate** at `dx-findings-signoff`:

- **[BLOCKER?] `snapdir id` reading a manifest from stdin is non-deterministic.** A byte-identical
  210 KB manifest yields a *different* id on every cold-cache run, and `snapdir manifest <dir> |
  snapdir id` never round-trips to `snapdir id <dir>`; warm-cache "stability" is a memoized wrong
  answer. *(persona-2 F1.)* **Needs verification** — if real, this is a content-addressing
  correctness bug, not DX. *(Note: snapshot-id-from-directory IS deterministic — `id <dir>` =
  `ce2b03…8399` across all runs/personas; the defect is specifically the stdin path.)*
- **[MAJOR] A nonexistent/typo'd `--to`/`--store` is silently treated as an empty store** → `diff`
  returns exit 0 with a fabricated full `D`/`A` delta and empty stderr; you can't distinguish a path
  typo from a real tree wipe. Worst class for unattended CI. *(persona-2 F2.)*
- **[MAJOR] Recovery gaps:** `fetch`/`pull`/`--force` will **not** restore a *missing/purged* cache
  object (they report `CACHED`/succeed while leaving the cache broken; only `flush-cache` + `fetch`
  works, undiscoverable); and **`verify-cache` is blind to *missing* objects** (silent exit 0 when an
  object is deleted — only detects *corrupt content*). *(persona-5 F1,F2.)*
- **[MINOR] `sync` miscounts:** its output/`--dryrun` count **file references** (2089) but label them
  "object(s)" when only 90 unique objects exist, and a first sync into an **empty** store reports
  "1976 skipped." Misleading for capacity/runtime estimates. *(persona-4 F1.)*

## 7. Severity-ranked master list

- **Blocker (verify):** id-from-stdin non-determinism (§6).
- **Major:** accepted-but-ignored flag surface (§2); `--debug` ghost (§2); `--paths` no-op (§2);
  `manifest --id` ignored→re-walks cwd (§2); `defaults` useless+ignores-flags (§3); progress
  garbage/0% (§4); missing-store/split error hints (§5); `verify --help` "staged" mismatch (§5);
  integrity errors lack file path (§5); silent-empty-store→fabricated diff (§6); recovery gaps —
  fetch won't restore missing, verify-cache missing-blind (§6).
- **Minor:** `--verbose` 4/16 gap (§2); value-validation inconsistency (§2); `checkout --linked`
  plain file (§2); `--catalog` accepts-anything (§2); `--walk-jobs` not echoed (§4); silent-success
  inconsistency (§5); `file://` scheme absent from help (§5); sync miscount (§6).
- **Nit:** `--max-jobs` w/o `--adaptive`; `--dryrun` bogus protocols; progress hidden non-TTY;
  `fetch --dryrun` silent; `-h` ≈ `--help`; one-at-a-time required-arg errors.

## 8. Recommended 1.8.0 scope (locks at `dx-findings-signoff`)

The **4 planned clusters cover the bulk** of the DX findings and are confirmed in scope:
1. **Arg hygiene (approach B)** — §2 in full. Naturally absorbs `--paths`, `manifest --id`,
   `checkout --linked`, `--catalog`, value-validation as part of per-command arg groups + native
   rejection.
2. **`defaults` rewrite** — §3.
3. **Progress observability** — §4 (fix the files-vs-bytes denominator + 0%/bar).
4. **Error-message hints + consistency** — §5 (store hints, `verify --help` fix, path-in-integrity-
   errors, silent-success consistency, `file://` in help/errors).

**Judge's recommendation for the §6 NEW findings (operator to decide at sign-off):**
- **Fold into existing clusters** (low marginal cost): `manifest --id`, `--paths`, `checkout
  --linked`, `verify --help` text → already inside clusters 1/4.
- **Add to 1.8.0 (correctness, CI-facing):** silent-empty-store→fabricated diff, and the
  `sync` miscount — both are "silent-wrong" and cheap to make loud.
- **Verify-then-decide:** id-from-stdin non-determinism — reproduce first; if a real bug, it's a
  must-fix (content-addressing) and may justify its own gate.
- **Consider / could defer:** the recovery gaps (fetch-won't-restore-missing, verify-cache
  missing-blind) — these are functional, larger than DX, and may warrant a dedicated decision.

Frozen manifest format stays untouched; snapshot ids byte-identical (all proposed fixes are CLI/
progress/error-message surface, none touch merkle/manifest/excludes).
