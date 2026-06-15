# DX/UX review artifacts (tracked, dev-only)

This directory holds the **judged, durable** artifacts of the Phase-30 adversarial CLI DX/UX
review. Unlike `.gatesmith/evidence/` (gitignored raw transcripts), these files are committed on
`dev` so the findings and the fix scope are auditable. They are gatesmith-internal and **never
ship upstream** (the release-verify branch excludes all `.gatesmith/`).

- `dx-scenario-catalog.md` — the reproducible QA sandbox + the realistic scenarios the personas resolve (`dx-sandbox-setup`).
- `dx-arg-matrix.md` — systematic (command × flag) audit: effective / silent-noop / rejected / n-a (`dx-arg-matrix-audit`).
- `dx-findings-1.8.0.md` — the consolidated, deduped, severity-ranked findings + calibration-coverage (`dx-judge-synthesis`); the input the operator signs off (`dx-findings-signoff`).

Raw per-persona friction logs and the fix-verify log live under the gitignored
`.gatesmith/evidence/dx-personas/` and `.gatesmith/evidence/dx-fix-verify.log`.
