# `.gatesmith/pending-tests/` — adversary test drop-zone

Staging area for the **adversarial test separation** workflow (see
`.gatesmith/PM_PROMPT.md` → "Adversarial test separation" and
`.gatesmith/templates/adversary.md`).

Flow for a testable code feature `<feature>`:

1. **`<feature>-spec-tests`** (owner `adversary`, black-box) writes
   `<feature>.rs` here — critical tests authored **from the gate SPEC only**,
   with zero visibility into the (not-yet-existing) implementation. Staged here so
   the cargo workspace keeps compiling.
2. **`<feature>-impl`** (the lane owner) **moves** `<feature>.rs` into
   `crates/<crate>/tests/<feature>.rs`, wiring/fixing only compile-shape, and
   implements `src/` until it passes. The staged file must be gone after this gate.
3. **`<feature>-tests-review`** (owner `adversary`) reviews the landed test against
   the staged original, restoring any weakened assertion. A strengthened test that
   fails the current code reopens `<feature>-impl`.

Files here are transient: present only between a feature's `-spec-tests` and
`-impl` gates. A lingering `<feature>.rs` means an impl gate hasn't taken it over yet.
