# Gatesmith journal

Append-only audit log. One line per verdict, written BEFORE `gates.yaml` is
mutated so a crashed tick still leaves a forensics trail. Format:

```
<UTC-ISO> gate=<id> owner=<agent> verdict=<pass|fail|out-of-lane> git=<sha> evidence=<paths> note=<one-line>
```

`GATE-BUMP` entries record any approved change to a gate's pass_criteria.

---
2026-05-31T01:16:12Z gate=bootstrap-ready owner=generic verdict=pass git=904b48a evidence=.gatesmith/evidence/bootstrap-ready-2026-05-31T01:16:12Z.log note=plan+ledger+templates present; tree clean, no code change needed
