# Gatesmith journal

Append-only audit log. One line per verdict, written BEFORE `gates.yaml` is
mutated so a crashed tick still leaves a forensics trail. Format:

```
<UTC-ISO> gate=<id> owner=<agent> verdict=<pass|fail|out-of-lane> git=<sha> evidence=<paths> note=<one-line>
```

`GATE-BUMP` entries record any approved change to a gate's pass_criteria.

---
