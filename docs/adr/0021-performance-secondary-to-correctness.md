# 0021 — Performance is secondary to byte-identical output

Status: Accepted, 2026-06

## Context

A native Rust implementation with in-process BLAKE3, a parallel walk, and memory-mapped
hashing is much faster than the Bash version, which forks `b3sum` per file. But the
whole value of the port depends on byte-for-byte compatibility (ADR-0002, ADR-0020).
Performance optimizations must never change a single output byte.

## Decision

Treat performance as secondary to correctness, with byte-identical output as the hard
guard. The performance gate's verification compares Bash vs Rust `manifest` stdout with
`cmp` first; any drift is a hard fail with a diff, before any timing is reported. Timing
is measured over representative corpora (many small files, few large files) using
hyperfine when present and a portable median-of-N fallback otherwise.

Measured results: ~33.6× faster on the many-small-files corpus and ~2.69× on the
few-large-files corpus, with output byte-identical on both. The speedup comes purely
from the in-process walk and BLAKE3 — no change to the frozen core was needed.

## Alternatives considered

- **Optimize aggressively, accept minor output differences.** Rejected outright: any
  output difference breaks interop, which is the point of the port.
- **Skip a performance gate.** Rejected: a measurable, output-guarded performance story
  is part of the value proposition and is worth proving.

## Consequences

- The performance gate cannot pass if output drifts, regardless of speed.
- Parallelism/mmap are defaults that were validated to be output-neutral.
- Performance is a reported benefit, not a tuning knob allowed to trade away
  correctness.
