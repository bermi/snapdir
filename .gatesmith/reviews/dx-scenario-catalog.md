# DX/UX scenario catalog — Phase 30 adversarial CLI review

This catalog drives an **adversarial, black-box** DX/UX review of the **installed**
`snapdir` binary. Each persona resolves ONE realistic goal against a shared,
reproducible sandbox and reports the friction they hit. The catalog states the
GOAL only — it never tells a persona what to look for or what to criticize.

## Sandbox

Built by `utils/dx/build-sandbox.sh` (deterministic, idempotent, pure shell — no
compile). (Re)build it with:

```sh
sh utils/dx/build-sandbox.sh
```

It materializes (under `.gatesmith/evidence/dx-sandbox/`, gitignored):

- `tree/` — a realistic source tree to snapshot. Layout:
  - `small/d00..d15/f*.bin` — ~2000 small files (~3 KB each) across 16 subdirs (non-trivial walk).
  - `large/big00..big02.bin` — 3 large files (~8 MB each); a snapshot takes a noticeable moment.
  - `deep/d000/.../d011/leaf.bin` — a 12-level deep chain.
  - `fan/c0000..c0063.bin` — a 64-sibling wide fan-out dir.
  - `dup/dup000..dup011.bin` — 12 byte-identical files (same checksum).
  - `edge/` — empty files (`empty_a.bin`, `empty_b.bin`, `sub/empty_c.bin`) and empty dirs (`empty_dir_a`, `empty_dir_b`).
  - `node_modules/`, `skip/` — excludable subdirs (for `--exclude` / `--paths`).
  - `README.md`, `config.toml`, `keep/` — ordinary top-level files.
- `store/` — an empty local file store to push/pull against.
- `objects-store/` — an empty split object pool (shared objects).

The tree is byte-deterministic: rebuilding yields an identical snapshot id, so
findings are reproducible.

## How to invoke snapdir

- Use the **installed** `snapdir` on `PATH` (it resolves to **1.7.0**). Confirm
  with `snapdir version`.
- This is **black-box**: a persona may read only `snapdir --help` and
  `snapdir <command> --help`. **Never read the source, the crates, or the docs/**
  to figure out how something works — discover it the way a real user would.
- Run from the repo root and address the sandbox by path, e.g.
  `snapdir id .gatesmith/evidence/dx-sandbox/tree`.
- Each persona works in a scratch area it creates under
  `.gatesmith/evidence/dx-personas/<persona>/` and pushes only to the sandbox's
  `store/` and `objects-store/` (or its own fresh dirs) — never mutate `tree/`.

## Reporting contract

> Report any friction, confusion, surprising behavior, or broken flags you
> encounter — with severity and exact repro.

Capture the exact command, the exact output, and what you expected vs. what
happened. Resolve your goal as a real user would; do not consult the source.

## QA goals (one per persona)

1. **First-time backup → restore on a "new machine."** Take a first-ever backup
   of `tree/`: stage it and push it to the local `store/`. Then simulate moving
   to a fresh machine — into a brand-new empty directory with a clean cache —
   and restore (check out / pull) the snapshot from `store/`. Confirm the
   restored copy matches the original.

2. **CI scripter / automation.** You are wiring snapdir into a CI pipeline that
   must make decisions without a human watching. Drive the relevant commands
   non-interactively and build your automation around their stdout and exit
   codes (and machine-readable output where a command offers it, e.g. the diff
   command). Demonstrate detecting "changed vs. unchanged" between two states of
   a tree programmatically.

3. **Performance tuner.** Snapshot the full `tree/` (including the large files)
   and work out how to make repeated snapshots and pushes faster — exercise
   whatever knobs the tool exposes for concurrency / throughput, and compare a
   tuned run against a default run.

4. **Operator: shared object store + sync.** Stand up a workflow that separates
   manifests from content objects using the shared `objects-store/` pool, push a
   snapshot through it, then set up a SECOND store and use snapdir to copy a
   snapshot directly from one store to the other. Confirm the second store can
   serve a full restore.

5. **Disaster recovery.** After backing up `tree/` to a store, simulate
   corruption (damage or delete data in the cache and/or the store), then use
   snapdir's integrity and verification commands to detect the damage and assess
   what is and isn't recoverable. Report how clearly the tool surfaces the
   problem.

6. **Newcomer orientation.** You have never used snapdir. Using ONLY
   `snapdir --help`, the per-command `--help`, and `snapdir defaults`, build a
   mental model of what the tool does and how its pieces fit, then carry out one
   simple end-to-end task you chose yourself. Report where the built-in help did
   or didn't get you there.
