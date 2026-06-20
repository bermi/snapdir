# Committed iai-callgrind baseline references

This directory holds **human-readable committed reference snapshots** of the
deterministic instruction-count perf gate (`benches/benches/iai_hot.rs`,
iai-callgrind). They are NOT the machine baseline iai-callgrind compares against
at runtime — that lives under `target/iai/` (in `target/`, which is not
committed; CI persists/restores it). These JSON files are a tracked, reviewable
record of the *expected* `Ir` (instructions retired) and `EstimatedCycles` for a
benched hot path, captured from a known-good run, so a reviewer can see the
order-of-magnitude a change is measured against without having to run valgrind.

## How a baseline relates to the 5% soft-limit gate

`iai_hot.rs` wires a **5% soft limit** on both `Ir` and `EstimatedCycles`
(`callgrind_5pct()` → `Callgrind::soft_limits([(Ir, 5.0), (EstimatedCycles,
5.0)])`). On each run iai-callgrind compares the freshly measured counts against
its saved baseline and **FAILS** the bench when either metric regresses by more
than 5%. The numbers in these JSON files are the reference point: if a change
moves `prune_set`'s `Ir` more than ~5% off the committed `ir` here, expect the
gate to fire (and, if the change is justified, refresh both the machine baseline
*and* the committed reference here in the same commit, explaining why).

## Recording / refreshing

These numbers come from the **sanctioned Docker runner** (valgrind is Linux-only;
this is a macOS host with no native valgrind):

```sh
bash benches/run-iai-docker.sh
```

It runs `cargo bench -p snapdir-benches --bench iai_hot` inside the pinned
`rust:1.91-slim-bookworm` image (pinned `linux/amd64` so counts match CI) with
`iai-callgrind-runner 0.16.1`. Read the `prune_set` group's reported `Ir` and
`EstimatedCycles` from its output and update the matching JSON. On a brand-new
bench (no prior baseline) the first run simply ESTABLISHES the baseline — that is
the run these references were captured from.

## Files

- `mirror_prune_set.json` — the Phase-32 `mirror::prune_set` group
  (`checkout/sync --delete` set-difference) over a fixed tiny `mixed` manifest +
  a constant planted-extraneous `DestEntry` listing.
