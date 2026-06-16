# snapdir 1.8.0 — Adversarial CLI DX/UX Review: Full Report

**Phase 30 · 2026-06-15 → 2026-06-16 · branch `dev` → release `1.8.0`**

---

## 1. Executive summary

You asked for an **adversarial DX/UX review** of the `snapdir` CLI: stand up a sandbox,
have independent QA-engineer agents poke holes in the experience **without being told the
known issues**, judge and capture the feedback, then ship a **minor (1.8.0)** with the fixes —
all under the gatesmith adversarial model (test-author ≠ feature-author ≠ reviewer).

**Outcome:** a reproducible unbiased harness rediscovered all three of your known smells
**plus** several you hadn't flagged; the findings were judged into a scoped report; **seven fix
workstreams** were implemented as adversarial triples; an **independent loop-closer** rebuilt the
binary and black-box re-verified **12/12 findings RESOLVED**; and 1.8.0 was prepped, the
`release-verify/1.8.0` branch built off upstream/main, pushed (full CI mirror green), and
`bench-verify.yml` is green.

- **Gates:** 37 in Phase 30, 33 passed (the remaining 4 are the human-checkpoint release steps).
- **Code commits:** 22 (`80896de`…`ca3de8a`), gatesmith-free + cherry-pickable.
- **Calibration:** 3/3 — the sealed personas independently rediscovered every known smell.
- **Keystone held throughout:** frozen manifest format untouched, snapshot ids byte-identical.

> **One genuinely breaking change** ships in 1.8.0: global flags must now follow their
> subcommand (`snapdir push --store X`, not `snapdir --store X push`). This is the deliberate
> consequence of per-command argument validation.

---

## 2. The unbiased-discovery contract (why this is trustworthy)

The persona agents were **sealed**: each got only a realistic goal + the sandbox + the CLI and
`--help` — no source, no list of known problems, no cross-talk. The three known smells lived only
in the **judge's calibration set**. The judge then measured **calibration coverage**: did the
unbiased agents independently rediscover them? If they hadn't, that would have indicted the
*harness*, not absolved the bugs.

**They did — 3/3:**

| Known smell (yours) | Independently rediscovered by | Quote |
|---|---|---|
| Progress stuck at 0% / useless | persona-3 (perf tuner) | "denominator is the byte count not files; the bar never fills; % is frozen at 0%" |
| `defaults` is cruft / missing real knobs | persona-6 (newcomer) | "prints just 4 env vars… none of the defaults a newcomer wants" |
| Flags do nothing (`--debug`/`--verbose`) | persona-3 / persona-1 | "`--debug` prints *less* than `--verbose`"; "`SAVED` only with `--verbose`, inconsistent" |

And they surfaced **more** than the seed set, including CI-hostile silent-wrong behaviours
(see §4).

---

## 3. Process: discovery → judge → scope-lock → fix → independently verify

```
A. DISCOVERY (black-box vs installed 1.7.0, no compile)
   dx-sandbox-setup ─ reproducible sandbox (2089 files / 29 MB) + scenario catalog
   dx-arg-matrix-audit ─ systematic (command × flag) audit
   dx-qa-personas ─ 6 SEALED personas (parallel, isolated)
   dx-judge-synthesis ─ deduped, severity-ranked findings + CALIBRATION 3/3
   dx-findings-signoff ─ ✋ you locked the scope (4 clusters + 4 §6 correctness findings)

B. FIXES (7 adversarial triples: spec → impl → review)
   1. argument hygiene (approach B)          5. progress observability (core + cli)
   2. defaults rewrite                       6. recovery gaps
   3. error-message hints                    7. help-text gaps (caught by the loop-closer)
   4. id-from-stdin (investigate → fix)
   dx-fix-verify ─ INDEPENDENT loop-closer: rebuild + black-box re-check → 12/12 RESOLVED
   dx-complete ─ CHANGELOG

C. RELEASE 1.8.0
   release-prep-1.8.0 ✓ → release-verify-branch ✋ → PR ✋ → tag+crates ✋ → phase30-complete ✋
```

The sandbox (`utils/dx/build-sandbox.sh`, dev-only) is deterministic — `snapdir id` on it is
stable across rebuilds (`ce2b0312…8399`), so every finding is reproducible.

---

## 4. Findings (judged) and how each was fixed

The judge's report (`.gatesmith/reviews/dx-findings-1.8.0.md`) ranked findings across six
categories. Below: each finding, its severity, and the fix + commit.

### Cluster 1 — Argument hygiene (approach B)
**Root cause:** every flag was declared clap `global = true`, with an in-code comment admitting
per-command validation was deferred and never done. So inapplicable flags were silently accepted
everywhere.

| Finding | Sev | Fix | Commit |
|---|---|---|---|
| Dozens of accepted-but-ignored flags (e.g. `--limit-rate` on `id`) | Major | Per-command arg groups; clap natively rejects inapplicable flags (exit 2) + scoped `--help` | `4280b7b`, `92653c4` |
| `--debug` a universal ghost (0 reads) | Major | Removed | `4280b7b` |
| `--paths` filtered nothing on `manifest`/`id` | Major | Removed (false promise) | `4280b7b` |
| `manifest --id` silently re-walked cwd | Major | Now rejected (not in manifest's group) | `4280b7b` |
| `--color`/`--limit-rate` accepted garbage | Minor | `--color` → ValueEnum; `--limit-rate` validator | `4280b7b` |
| `--verbose` honored by only 4/16 commands | Minor | Kept universal + documented | `4280b7b` |

Tests: `dx_args.rs` 33 cases. **Reopen #1 happened here** (see §5).

### Cluster 2 — `defaults` rewrite
`snapdir defaults` printed ~4 legacy env lines and ignored its own `--cache-dir`. Now it prints
**every effective knob with resolved value + `source=flag|env|default`** (cache-dir, store, jobs,
walk-jobs, limit-rate, retries, fsync, clonefile, …) and reflects overrides. Tests: `dx_defaults.rs`
26 cases. Commit `9c31b95` + review `8cc87ca` (which also fixed an obsolete `parity.rs` test).

### Cluster 3 — Progress observability (the headline)
The "files" denominator was actually the **byte count**, the bar never filled, % frozen at 0% —
because discovery was silent and `objects_total` was never set before hashing.
**Fix:** `core` adds `Phase::Discovering` + an `objects_discovered` counter and sets
`set_total(file_count)` **before** hashing; `cli` renders a visible "discovering N files" then a
determinate "NN% done/total **files**". Snapshot ids byte-identical (meter is output-orthogonal).
Tests: `dx_progress.rs` 13 PTY cases. Commits `b8cea2b` (core) + `ce3299d` (cli) + `7261f97` (review).

### Cluster 4 — Error-message hints + §6 silent-wrong
| Finding | Sev | Fix | Commit |
|---|---|---|---|
| Missing `--store` → bare error | Major | Names `--store`/`SNAPDIR_STORE` | `15b01d7` |
| Split snapshot w/o `--objects-store` → raw `object not found` | Major | Hints the objects-store (only when no pool supplied) | `15b01d7` |
| **§6** typo'd store silently treated as empty → fabricated full diff (exit 0) | Major | `list_manifest_ids` errors on a nonexistent root | `8b0a4b2` |
| **§6** `sync` counted file-refs as "objects" + "N skipped" into empty dest | Major | Dedup by checksum → unique counts; skipped only = already-present | `8b0a4b2` |

Tests: `dx_errors.rs` 15 cases. **Lane-split** (stores + cli) — see §5.

### id-from-stdin (investigate → REAL-BUG → fix)
You asked to investigate the "non-deterministic `id`" finding. **Verdict: real bug** — `snapdir id`
with no path was documented to read a manifest from stdin but silently **walked the cwd**, so
`manifest <dir> | id` hashed wherever you stood. **Fix:** it now reads stdin (reusing the frozen
`snapshot_id`), so `manifest <dir> | id` == `id <dir>` byte-identical; no-path-on-a-TTY errors
loudly. Tests: `dx_id_stdin.rs` 8 cases. Commit `dc6b389` + review `a5abd66`.

### Recovery gaps (§6)
- `fetch`/`pull` reported `CACHED` and left a cache with a **missing object** broken (only the
  obscure `flush-cache`+`fetch` healed it). Now fetch verifies every object is present and
  re-fetches the missing ones.
- `verify-cache` only caught **corrupt** content, silently passing a **deleted** object. Now it
  reports missing objects with a **non-zero exit and the affected file path**
  (`Missing object <hash> for ./b.txt`).

Lane re-routed cli (the bug was the CLI fetch short-circuit, not stores). Tests: `dx_recovery.rs`
17 cases. Commit `b82282f` + review `82e5310`.

### Help-text gaps (caught by the loop-closer — see §6)
- `verify --help` falsely said "Verify the integrity of a **staged** snapshot" (it checks the
  *store*). Fixed to "…a snapshot in a store (requires --store/--id)" — `f933ef8`.
- An invalid store URI gave no guidance. The router error now names the form: `file://<path>`
  for a local store, or `<scheme>://…` for an external helper — `4039237`.

Tests: `dx_helptext.rs` 7 cases.

---

## 5. Process highlights — where the harness earned its keep

The adversarial separation and the independent verifications caught real problems **before**
release:

1. **Reopen #1 (arg hygiene):** the snapshot-regen gate ran the *full* cli suite (which the impl
   gate's narrow check didn't) and exposed **3 real regressions** in the approach-B restructure —
   catalog logging dropped from manifest/stage/push, plumbing commands lost `--store`, and
   `sync --from` lost its `SNAPDIR_STORE` env fallback. Per the reopen rule, the *impl* was
   reopened and fixed (`92653c4`), the test never weakened.
2. **Lane re-routes from black-box probing:** the recovery cluster was scoped to `stores` but the
   adversary's probing proved it was a **cli** short-circuit (`flush-cache`+`fetch` healed → the
   stores API was fine); and the errors/help-text clusters were **split** cli+stores when a
   finding lived in the stores router. Each re-route avoided a wasted impl round-trip.
3. **Flaky-test self-correction:** a progress capture-ceiling test (the adversary's own new case)
   failed 5/5 in isolation on this fast machine because `finish()` clears the final PTY frame; it
   was a test-capture artifact, not an impl defect — relaxed to a reliably-capturable threshold
   without weakening the real guarantee.
4. **Loop-closer found 2 unpinned findings** (§6) — and I discovered the gate's own verification
   was too weak (it would have rubber-stamped a NOT-PASS log) and strengthened it.
5. **Pre-push CI mirror caught fmt + clippy** that the gate checks missed: `cargo fmt --all`, a
   clippy `--all-features` cleanup (allow `too_many_arguments` on the now-8-arg `discover_dir`;
   doc/style allows in test prose), and a `_typos.toml` allowlist (`deprecat` substring,
   `unparseable`). Applied to **both** dev and the verify branch (`54e7ecf`, `ca3de8a`).

---

## 6. Independent loop-closer — the proof

`dx-fix-verify` rebuilt the 1.8.0 binary and black-box re-checked **all 12 prioritized findings**
the same unbiased way they were discovered. First run: **10/12** (it caught the 2 help-text gaps
the errors-spec never pinned → a help-text triple was scheduled). After the fix, re-run:

```
OVERALL: 12/12 RESOLVED — PASS
```

Evidence: `.gatesmith/evidence/dx-fix-verify.log`. The strengthened gate requires `PASS` present
**and** zero `NOT-RESOLVED`/`NOT PASS` anywhere.

---

## 7. 1.8.0 release status

| Step | Status |
|---|---|
| `release-prep-1.8.0` — bump 1.7.0→1.8.0 (5 pins) + CHANGELOG fold | ✅ `f9788b8` |
| `release-verify/1.8.0` built off upstream/main, ZERO `.gatesmith`, tip 1.8.0 | ✅ `fb67069` |
| Pushed to origin (`bermi/snapdir`), **full pre-push CI mirror passed** | ✅ |
| `bench-verify.yml` on the fork | ✅ success (run 27587056321) |
| `release-verify-branch-1.8.0` gate | ⏳ **awaiting your approval** |
| Upstream PR → squash-merge (irreversible) | ⏳ you drive |
| Tag `v1.8.0` + 6-crate publish (**TP pre-check on all 6 first!**) | ⏳ you drive |
| `phase30-complete` | ⏳ |

**Carry-over for the release CI:** a pre-existing `adaptive::controller_driver_throttle…` timing
flake (out of DX scope; 1.7.0 shipped with it) can intermittently red a full test run — if the PR
CI trips on *only* that test, it's the flake; re-run clears it.

---

## 8. Commit ledger (1.8.0 / Phase 30, gatesmith-free)

```
ca3de8a style: cargo fmt + clippy --all-features cleanup for the Phase-30 DX changes
54e7ecf typos: allowlist 'deprecat' (substring) + 'unparseable' (valid spelling) in dx tests
f9788b8 release: 1.8.0 — bump version + fold CHANGELOG
e08c733 docs(changelog): Phase-30 CLI DX/UX fixes under [Unreleased]
4194b56 test(cli): review help-text — +4 cases (dx_helptext 3→7)
f933ef8 fix(cli): verify --help no longer falsely says 'staged'
4039237 fix(stores): invalid-store-protocol error names valid schemes (file://)
82e5310 test(cli): review recovery — +6 cases (dx_recovery 11→17)
b82282f fix(cli): fetch restores missing cache objects; verify-cache detects missing
7261f97 test(cli): review progress — +5 cases (dx_progress 8→13), harden capture thresholds
ce3299d feat(cli): render the discovery phase + a file-count progress %; wire id/manifest
b8cea2b feat(core): emit discovery progress + set file-count total before hashing
a5abd66 test(cli): review id-from-stdin — +6 cases (dx_id_stdin 2→8)
dc6b389 fix(cli): 'id' with no PATH reads a manifest from stdin (was silently walking cwd)
755e745 test: review errors cluster — +8 impl-revealed cases (dx_errors 11→15)
15b01d7 feat(cli): actionable hints for missing-store and split-store errors
8b0a4b2 fix(stores): error on nonexistent store + count unique objects in sync
8cc87ca test: review defaults cluster — +7 dx_defaults cases, fix obsolete parity test
9c31b95 feat(cli): rewrite 'defaults' to print effective config with source tags
376f93e test(cli): adversary review — +13 impl-revealed arg-hygiene cases (dx_args 20→33)
bd6ce47 test(cli): regenerate help snapshots + adapt integration tests for approach-B
92653c4 fix(cli): restore catalog logging, plumbing --store, sync --from env after approach-B
4280b7b feat(cli)!: per-command argument groups (approach B)   ← BREAKING
80896de chore(dx): deterministic QA sandbox builder for the CLI DX/UX review
```

Net diff on the release branch: **49 files, +8229 / −1741** (excludes `.gatesmith/` and the
dev-only `utils/dx/` sandbox tooling).

---

## 9. What ships in 1.8.0 (user-facing)

- **BREAKING:** global flags must follow their subcommand; each command accepts only its
  applicable flags (inapplicable → clear error); per-command `--help` shows only its own flags.
- **Added:** `snapdir id` reads a manifest from stdin (so `manifest <dir> | id` round-trips).
- **Changed:** `defaults` prints effective config with source; live progress shows discovery +
  a real file-count `%`; `sync` reports unique objects copied (no false skips on an empty dest).
- **Removed:** `--debug` and `--paths` (both no-ops).
- **Fixed:** invalid `--color`/`--limit-rate`/store-URI now rejected; `manifest --id` honored;
  `diff`/`sync` error on a nonexistent store; `fetch`/`pull` restore cache-missing objects;
  `verify-cache` reports missing objects with the file path; clearer missing-store / split-store /
  `verify --help` messages.

Frozen manifest format untouched; snapshot ids byte-identical; default behaviour unchanged when
the new flags are unset.
