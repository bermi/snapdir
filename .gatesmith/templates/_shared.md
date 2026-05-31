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
