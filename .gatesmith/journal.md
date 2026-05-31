# Gatesmith journal

Append-only audit log. One line per verdict, written BEFORE `gates.yaml` is
mutated so a crashed tick still leaves a forensics trail. Format:

```
<UTC-ISO> gate=<id> owner=<agent> verdict=<pass|fail|out-of-lane> git=<sha> evidence=<paths> note=<one-line>
```

`GATE-BUMP` entries record any approved change to a gate's pass_criteria.

---
2026-05-31T01:16:12Z gate=bootstrap-ready owner=generic verdict=pass git=904b48a evidence=.gatesmith/evidence/bootstrap-ready-2026-05-31T01:16:12Z.log note=plan+ledger+templates present; tree clean, no code change needed
2026-05-31T01:18:04Z LANE-FENCE-EXC gate=scaffold-workspace owner=ci note=ci authorized to create trivial crate stubs under crates/snapdir-{core,catalog,stores,cli}/ per PM_PROMPT documented exception; required by gate files_exist + ci.md stub authorization
2026-05-31T01:18:04Z gate=scaffold-workspace owner=ci verdict=pass git=3799bb1 evidence=.gatesmith/evidence/scaffold-workspace-2026-05-31T01:18:04Z.log note=cargo build --workspace --locked exit0; workspace+toolchain pinned (1.96.0), ring stance noted, no aws-lc-rs, trivial stubs only
