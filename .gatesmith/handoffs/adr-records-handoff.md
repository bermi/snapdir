# docs handoff for adr-records @ 2026-06-03

## Summary

Created `docs/adr/` capturing the port's architecture decisions in MADR format.
Authored an index (`0000-index.md`) with a short intro to ADRs and a table linking
every record (number, title, status = Accepted), plus 26 numbered ADR files
(`0001`…`0026`). Each ADR carries a `Status: Accepted` line dated `2026-06` and the four
sections `## Context`, `## Decision`, `## Alternatives considered`, `## Consequences`.

Content was mined from `.gatesmith/journal.md`, `docs/rust-port/PLAN.md`, and
`crates/snapdir-stores/Cargo.toml` (TLS rationale), and kept accurate and plain. Notable
relationships preserved: ADR-0024 (retire the Bash oracle) supersedes ADR-0001
(differential-oracle methodology), and the index marks ADR-0001 as superseded; ADR-0003
records the corrected snapshot-ID rule (BLAKE3 of the `#`-stripped manifest text, not the
root dir checksum); ADR-0023 documents the operator-approved scoped B2 gate; ADR-0026
captures the latest-deps + 3-day cooldown decision.

## Files changed

git diff --stat (tracked changes): no ADR files are tracked yet; they are new/untracked
under `docs/adr/`. New files:

```
docs/adr/0000-index.md
docs/adr/0001-differential-oracle-methodology.md
docs/adr/0002-manifest-format-freeze.md
docs/adr/0003-snapshot-id-is-blake3-of-manifest-text.md
docs/adr/0004-ring-tls-provider.md
docs/adr/0005-native-in-process-cloud-stores.md
docs/adr/0006-b2-over-s3-compatible-endpoint.md
docs/adr/0007-redb-catalog.md
docs/adr/0008-catalog-json-output-lock.md
docs/adr/0009-gcs-notfound-classification.md
docs/adr/0010-unix-only-drop-windows.md
docs/adr/0011-cargo-dist-musl-static-packaging.md
docs/adr/0012-scratch-docker-image.md
docs/adr/0013-coverage-floor-75.md
docs/adr/0014-remove-verify-purge.md
docs/adr/0015-all-14-subcommands-wired.md
docs/adr/0016-rust-only-public-docs.md
docs/adr/0017-gatesmith-pm-orchestration.md
docs/adr/0018-no-false-passes.md
docs/adr/0019-frozen-interface-sha-locks.md
docs/adr/0020-interop-diff-keystone-gate.md
docs/adr/0021-performance-secondary-to-correctness.md
docs/adr/0022-testing-strategy.md
docs/adr/0023-b2-scope-rust-and-format-compat.md
docs/adr/0024-retire-the-bash-oracle.md
docs/adr/0025-keep-native-certs.md
docs/adr/0026-latest-deps-with-release-age-cooldown.md
```

`git status --porcelain docs/adr/` → `?? docs/adr/` (27 new files).

## Local verification result

```
$ test -f docs/adr/0000-index.md && [ "$(ls docs/adr/0*.md 2>/dev/null | wc -l | tr -d ' ')" -ge 20 ] && for f in docs/adr/000[1-9]-*.md docs/adr/00[1-9][0-9]-*.md; do grep -qiE '## *(context|decision)' "$f" || exit 1; done
$ echo $?
0
```

Exit 0. Additionally confirmed every ADR file contains all four headings
(`## Context`, `## Decision`, `## Alternatives considered`, `## Consequences`) and that
the count of numbered ADRs is 26 (>= 25 required, >= 20 for the gate).

## Reuse check / Blockers

- All new content is under `docs/adr/` only. No edits to the frozen oracle scripts,
  `utils/qa-fixtures/`, or `crates/**`. `git diff --stat` shows only a pre-existing
  `.claude/ralph-loop.local.md` modification that was already present in the working tree
  before this gate (not authored by this lane); all of this gate's work is the untracked
  `docs/adr/` directory.
- No rustdoc/crates changes needed.
- No blockers.

Ready for PM verification: YES
