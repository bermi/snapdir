# DX Arg Matrix Audit — `snapdir` CLI (phase 30, black-box)

## Method

- **Binary:** installed `snapdir` on PATH, `snapdir --version` => `snapdir 1.7.0`
  (`/Users/bermi/.asdf/installs/rust/1.78.0/bin/snapdir`).
- **Discipline:** BLACK-BOX (adversary Mode A). Ground truth = the binary's own
  `--help` and observed runtime behavior. **No crate `src/` was read.** Help text
  enumerated via `snapdir <cmd> --help`; trycmd filenames listed only to confirm
  the public subcommand set.
- **Sandbox:** `.gatesmith/evidence/dx-sandbox/` — `tree/` (real fixture dir),
  `store/` + `objects-store/` (file stores), plus isolated `cache/`, `cache_fetch/`,
  `out1..out4/` created during the audit (the user's real cache was never touched;
  every run used an explicit `SNAPDIR_CACHE_DIR` under the sandbox).
- **Classification** per (command × flag): each flag run WITH vs WITHOUT, comparing
  exit code, stdout byte count, stderr, and on-disk effect.
  - **EFFECTIVE** — observably changes behavior/output.
  - **SILENT-NOOP** — accepted (exit 0, no error) but nothing observable changes.
  - **REJECTED** — command errors when given the flag/value.
  - **N/A** — flag not offered on that command.

### Structural note (the root cause of most smells)

`snapdir --help` advertises ~28 "global" options, and **clap flattens the ENTIRE
global set onto EVERY subcommand's `--help`** — so `id`, `manifest`, `defaults`,
`diff`, etc. all *advertise and accept* `--limit-rate`, `--adaptive`, `--jobs`,
`--max-retries`, `--keep`, `--purge`, `--linked`, `--dryrun`, `--debug`, … even
when the command performs no transfer, no staging, and no network I/O. Acceptance
is therefore **not** evidence of effect. The matrix below is the empirical filter.

---

## Per-command findings (representative evidence)

Byte counts are stdout unless noted. `SB` = sandbox path, `T=$SB/tree`,
`STORE=file://$SB/store`, `ID=ce2b03…8399` (the staged tree).

### `manifest` (local, read-only)
| flag | class | evidence |
|---|---|---|
| `--absolute` | EFFECTIVE | paths switch from `./README.md` to `/Users/.../tree/README.md` |
| `--checksum-bin md5sum` | EFFECTIVE | stdout 210391 → 142167 (shorter md5 digests) |
| `--exclude node_modules` | EFFECTIVE | 210391 → 209883 |
| **`--paths <P>`** | **SILENT-NOOP** | `--paths README`, `--paths README.md`, `--paths large`, and even `--paths zzz_nomatch_zzz` ALL return the identical 210391 bytes. Help says "Only include paths matching PATTERN" — **filters nothing.** Confirmed via `id`: hash unchanged for every `--paths` value (see below). |
| `--no-follow` | INCONCLUSIVE | identical output, but the sandbox `tree/` contains **no symlinks** (`find -type l` empty), so follow-vs-nofollow can't differ here |
| `--debug` | SILENT-NOOP | 210391 == baseline, zero stderr |
| `--verbose` | SILENT-NOOP | 210391 == baseline, zero stderr (contrast: verbose *does* fire on stage/push/fetch/checkout) |
| `--limit-rate 1K`, `--jobs 2`, `--adaptive`, `--max-*`, `--no-progress`, `--keep`, `--purge`, `--linked`, `--dryrun` | SILENT-NOOP | accepted, 210391 == baseline (no transfer/stage happens in `manifest`) |
| `--checksum-bin sha999` | REJECTED | exit 1, `snapdir: unsupported --checksum-bin 'sha999'` |

### `id` (local hash; reads dir or stdin)
Ran **15 flags** against `id $T`; baseline stdout = 64 bytes (the hash).
**Every** flag — `--debug --verbose --limit-rate 1K --jobs 1 --walk-jobs 1
--max-retries 9 --adaptive --no-progress --dryrun --linked --keep --purge --force
--max-requests 1` — returned exit 0 with the **identical 64-byte hash and zero
stderr**: all **SILENT-NOOP**. `--exclude node_modules` => **EFFECTIVE** (hash
`ce2b03…` → `b65f04…`). `--paths README` / `--paths zzz_nomatch` => **SILENT-NOOP**
(hash unchanged), the clean proof that `--paths` is inert.

### `stage` (writes local cache)
| flag | class | evidence |
|---|---|---|
| `--verbose` | EFFECTIVE | stderr `transfers: 12 concurrent` |
| `--dryrun` | EFFECTIVE | stderr `dry-run: would stage ce2b03…8399 into the local cache (no writes performed)` |
| `--debug` | SILENT-NOOP | no extra output vs baseline |
| `--quiet` | EFFECTIVE (suppressive) | suppresses the verbose/banner line |

### `push` (transfer to store)
| flag | class | evidence |
|---|---|---|
| `--verbose` | EFFECTIVE | stderr `transfers: 12 concurrent` |
| `--dryrun` | EFFECTIVE | stderr `dry-run: would push ce2b03…8399 to file://…/store (no writes performed)` |
| `--debug` | SILENT-NOOP | no extra output |
| `--no-progress` | SILENT-NOOP* | no observable diff — progress line is already suppressed in a non-TTY pipe, so this can't be distinguished here |
| `--limit-rate 1K`, `--jobs 1` | SILENT-NOOP* | accepted, no output change; effect (if any) is internal timing only — not observable black-box on a fast local file store |

### `fetch` (transfer from store)
| flag | class | evidence |
|---|---|---|
| `--verbose` | EFFECTIVE | stderr `transfers: 12 concurrent` + `CACHED: ce2b03…8399` |
| `--debug` | SILENT-NOOP | no extra output |
| `--dryrun` | SILENT-NOOP | exit 0, **no** "dry-run: would…" line (contrast stage/push/checkout, which DO print one) — fetch dry-run is silent |
| `--no-progress`, `--limit-rate 1K` | SILENT-NOOP* | non-TTY, no observable diff |

### `checkout` (materializes files)
| flag | class | evidence |
|---|---|---|
| `--verbose` | EFFECTIVE | stderr `transfers: 12 concurrent` |
| `--dryrun` | EFFECTIVE | stderr `dry-run: would check out ce2b03…8399 to …/out3 (no writes performed)` |
| **`--linked`** | INCONCLUSIVE→NOOP | `checkout --linked` produced `out4/README.md` as a **regular file** (`-rw-r--r--`, link count 1, distinct inode from both the cache object and the plain-copy `out1`). Help says "Use symlinks instead of copies" but no symlink and no hardlink was created — on this run it was indistinguishable from a copy |
| `--debug` | SILENT-NOOP | — |

### `verify` (needs `--store`)
- Without `--store`: REJECTED-ish — exit 1, `missing --store option` (regardless of `--verbose`/`--debug`).
- With `--store`: exit 0 but **completely silent**, even with `--verbose` and `--debug`. => **`--verbose` is a NOOP on verify** (verbose gap).

### `verify-cache` (local)
- Baseline: exit 0, silent. `--verbose` and `--limit-rate 1K`: still exit 0, **silent**. => `--verbose` NOOP (verbose gap); transfer flags NOOP.

### `flush-cache`
- All flags advertised; none have an observable effect beyond the cache flush itself. Transfer/network flags (`--limit-rate`, `--jobs`, `--adaptive`, `--max-*`) SILENT-NOOP.

### `locations` / `ancestors` / `revisions` (catalog queries)
- Require a catalog: without one, exit 1 `error: Missing SNAPDIR_CATALOG or --catalog` (so `--verbose`/`--debug` can't be exercised pre-catalog).
- With `--catalog {file,sqlite,json,default}`: each accepted, exit 0, empty output (nothing cataloged in the sandbox). `--catalog` => EFFECTIVE (gates the command). Transfer/walk flags on these query commands: SILENT-NOOP.

### `defaults` ("Print default settings and arguments")
- Output is **near-static** (5 lines: cache-dir + 4 SNAPDIR_* paths).
- `--jobs 3`, `--limit-rate 5M`, `--max-retries 99`, `--store $STORE`, `--exclude foo`, **and even `--cache-dir /tmp/xyz`** all produce **byte-identical** output (verified with `diff`). The one printed `--cache-dir=` line reflects the `SNAPDIR_CACHE_DIR` **env var**, not the `--cache-dir` **flag**. => On the command whose JOB is to surface effective settings, **essentially every flag is SILENT-NOOP and the override flags it prints don't reflect the flags you pass.**

### `sync` (store→store) and `diff` (manifests-only)
- `diff --json` => EFFECTIVE (`[]` porcelain → JSON). `diff --all` => EFFECTIVE (51723 bytes of equal paths emitted). `diff --exit-code` => EFFECTIVE semantics (advertised). `diff --on-conflict` => has clap-validated enum.
- `diff` transfer/walk flags `--walk-jobs 1`, `--limit-rate 1K`, `--linked` => SILENT-NOOP (diff reads MANIFESTS ONLY — expected, but still accepted-and-ignored).
- `sync` requires `--from`/`--to`; transfer flags plausibly effective but not exercised end-to-end here.

### Value-validation inconsistency (cross-cutting)
| flag | bad value | result |
|---|---|---|
| `--jobs notanumber` | REJECTED | exit 2, `invalid value … invalid digit found in string` |
| `--adaptive=2.0` | REJECTED | exit 2, `fraction must be in (0.0, 1.0]` |
| `--max-retries -1` | REJECTED (parse) | exit 2, `unexpected argument '-1'` |
| **`--limit-rate bogus`** | **ACCEPTED** | exit 0, no error — bad rate string silently swallowed |
| **`--color bogus`** | **ACCEPTED** | exit 0, no error — despite help listing `auto/always/never` |

---

## Smells summary

1. **Ghost-ish flag: `--paths`.** On `manifest`/`id` it is a total SILENT-NOOP —
   identical output and identical manifest ID for matching, partial, and
   deliberately non-matching patterns. Help promises "Only include paths matching
   PATTERN."
   Repro: `snapdir id --paths zzz_nomatch_zzz $T` == `snapdir id $T` (same hash);
   `snapdir manifest --paths README $T` == `snapdir manifest $T` (210391 bytes).
   Contrast `--exclude`, which works (`id --exclude node_modules` changes the hash).

2. **`--debug` appears to be a universal no-op.** Tested on `manifest`, `id`,
   `stage`, `push`, `fetch`, `checkout`, `verify` — **zero** observable difference
   anywhere (no extra stdout/stderr, no exit change). Candidate ghost flag globally.
   Repro: `snapdir stage --debug $T` produces the same output as `snapdir stage $T`.

3. **`--verbose` coverage gaps.** EFFECTIVE on `stage`/`push`/`fetch`/`checkout`
   (emits `transfers: N concurrent`, and `CACHED:`/dry-run lines). **NOOP on**
   `manifest`, `id`, `verify` (with `--store`, exit 0 but silent), and
   `verify-cache`. So `--verbose` is advertised identically on all 16 commands but
   actually fires on only 4.
   Repro: `snapdir verify --id $ID --store $STORE --verbose` → silent;
   `snapdir manifest --verbose $T` → byte-identical to baseline.

4. **`defaults` doesn't reflect the flags you pass.** `snapdir defaults
   --cache-dir /tmp/xyz` (and `--jobs/--limit-rate/--max-retries/--store/--exclude`)
   is byte-identical to bare `snapdir defaults`. The command meant to print
   effective settings ignores the very override flags it accepts.
   Repro: `diff <(snapdir defaults) <(snapdir defaults --cache-dir /tmp/xyz)` → no diff.

5. **Accepted-but-ignored transfer/network flags on local & manifest-only commands.**
   `--limit-rate`, `--adaptive`, `--max-jobs`, `--max-retries`, `--retry-*`,
   `--max-requests`, `--jobs`, `--walk-jobs`, `--no-progress` are advertised and
   accepted on `id`, `manifest`, `defaults`, `verify-cache`, `flush-cache`,
   `locations`/`ancestors`/`revisions`, and (transfer/walk subset) on `diff` —
   none of which transfer bytes over a network. All SILENT-NOOP.
   Repro: `snapdir id --limit-rate 1M --adaptive --max-retries 9 $T` == `snapdir id $T`.

6. **Staging/transfer flags on non-staging commands.** `--keep`, `--purge`,
   `--linked`, `--force`, `--dryrun` are advertised on `id`/`manifest`/`defaults`/
   query commands where there is no staging dir to keep, no objects to purge, and
   no copy to link. SILENT-NOOP.

7. **Help/behavior mismatch: `checkout --linked`.** Help: "Use symlinks instead of
   copies." Observed: `out4/README.md` materialized as a **plain regular file**
   (not a symlink, not a hardlink — link count 1, inode distinct from the cache
   object). On this run `--linked` was indistinguishable from a copy.

8. **Inconsistent value validation.** `--jobs`/`--adaptive`/`--max-retries` reject
   bad values (exit 2), but `--limit-rate bogus` and `--color bogus` are silently
   accepted (exit 0) — `--color` even has a documented `auto/always/never` enum
   that isn't enforced.

9. **`fetch --dryrun` is silent** while `stage`/`push`/`checkout --dryrun` each
   print a "dry-run: would …" line — an inconsistency in the dry-run UX.

### Quick tallies (probed cells)
- **EFFECTIVE (confirmed):** `manifest --absolute/--checksum-bin/--exclude`,
  `id --exclude`, `stage --verbose/--dryrun/--quiet`, `push --verbose/--dryrun`,
  `fetch --verbose`, `checkout --verbose/--dryrun`, `verify --store`,
  `diff --json/--all`, `--catalog` (gates queries). (~16)
- **REJECTED (correct):** `--jobs`/`--adaptive`/`--max-retries` bad values,
  `--checksum-bin` bad value, `verify`/`fetch`/`push` w/o required store. (~7)
- **SILENT-NOOP (the DX smell):** dozens — every transfer/network/staging flag on
  `id`/`manifest`/`defaults`/`verify-cache`/`flush-cache`/query commands;
  `--paths` everywhere; `--debug` everywhere; `--verbose` on 4 commands;
  `defaults` ignoring all overrides; `--limit-rate bogus`/`--color bogus` accepted.

*Note: cells marked `SILENT-NOOP*` (e.g. `--no-progress`, `--limit-rate` on real
transfers) are "no OBSERVABLE black-box effect on a local file store in a non-TTY";
their internal effect can't be confirmed or denied without source — flagged
conservatively as not-observable rather than proven-dead.*
