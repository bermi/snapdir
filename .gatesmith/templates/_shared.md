<!--
  Shared snippet referenced by every lane template. Not spawned directly.
  Each lane template repeats the "Current gate" / "Your task" / "Handoff" blocks
  so the PM's runtime substitution ({{gate_id}} etc.) works per spawn.
-->

# Shared rules for every snapdir-rs lane teammate

- **The Bash oracle is frozen.** Never edit `snapdir`, `snapdir-manifest`,
  `snapdir-file-store`, `snapdir-s3-store`, `snapdir-b2-store`, `snapdir-gcs-store`,
  `snapdir-sqlite3-catalog`, `snapdir-test`, or anything under `utils/qa-fixtures/`.
  They are the interop oracle and the behavioral source of truth. You may READ them.
- **Pin to the scripts, not the docs.** `docs/` carries known bugs (`--link` vs
  `--linked`, `verify-transactions` vs `ensure-no-errors`). Match real script behavior.
- **Zero runtime dependencies in the shipped binary.** No shelling out to `b3sum`,
  `gcloud`, `aws`, `b2`, or `sqlite3`. Everything is in-process Rust. External
  binaries are allowed ONLY in the test/oracle harness, never in `crates/`.
- **Minimum change to pass the gate.** Don't gold-plate; don't reimplement what a
  sibling crate already exposes — depend on it.
- You do **not** commit. The PM commits after the lane fence + verification pass.
- Stay strictly inside your lane directory. An out-of-lane diff fails the fence.

## Adversarial test separation (overrides any "add tests" line below)

Tests are authored by an independent **`adversary`** teammate, never by the lane
owner who writes the feature. The cycle, coordinated by the PM across separate
gates, is:

1. **`<feature>-spec-tests` (adversary, black-box).** From the gate SPEC only —
   with **zero visibility into the implementation** (it does not exist yet) — the
   adversary writes a critical suite (non-happy paths, edge cases, performance
   validations, contract correctness) into the drop-zone
   `.gatesmith/pending-tests/<feature>.rs`. It does **not** read the feature's
   `src/`.
2. **`<feature>-impl` (lane owner).** You implement the feature in your lane's
   `src/` **and take over** the staged tests: move `.gatesmith/pending-tests/<feature>.rs`
   into `crates/<crate>/tests/<feature>.rs` and fix only what's needed to wire them
   (compile shape, a genuinely wrong assumption). You may **not** author NET-NEW
   tests beyond the staged file, add inline `#[test]`/`#[cfg(test)]` to `src/`, or
   weaken an assertion to go green — if a test exposes a real bug, fix the **code**.
3. **`<feature>-tests-review` (adversary).** The adversary reviews your landed tests
   against the staged originals, restores any weakened assertion, and adds cases the
   now-visible implementation reveals. If that strengthening fails against your code,
   the PM reopens `<feature>-impl` and you fix `src/` — the test stays the adversary's.

**Therefore:** lane owners write **production source only**. All test code is
adversary-originated and adversary-finalized. Prefer integration tests against the
public API (in `crates/<crate>/tests/`) over inline unit tests, so the adversary can
own them without touching `src/`. The per-lane "Add or extend tests" instruction
below is **superseded** by this section for any gate that is part of an
adversary/impl/review triple.
