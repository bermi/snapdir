# 0003 — Snapshot ID is BLAKE3 of the `#`-stripped manifest text

Status: Accepted, 2026-06

## Context

The snapshot ID is the primary handle for a stored tree: it names the manifest object
and is what `snapdir id` prints. Early planning material (and the rustdoc/tests written
against it) stated that the snapshot ID equals the root directory checksum — the
`CHECKSUM` field of the `D ./` manifest line. Golden testing against the oracle
contradicted this.

## Decision

The snapshot ID is the BLAKE3 hash of the **entire `#`-stripped manifest text**,
including the trailing newline the oracle's `echo` appends:

```
manifest | grep -v '^#' | b3sum --no-names
```

It is therefore **not** the root directory checksum; it is the hash of the whole
manifest document, not of any single line's checksum field. The root directory
checksum remains a distinct value (the `D ./` line's `CHECKSUM`).

The Rust core exposes `snapshot_id(manifest, hasher)` computing
`blake3(manifest.to_string())` (Display plus trailing newline). Golden IDs such as
`c678a299…` and `8af03a1b…` are reproduced byte-for-byte, distinct from the root
checksums `dba5865c…` and `4a0732cf…`.

## Alternatives considered

- **Snapshot ID = root directory checksum** (the original doc statement). Rejected:
  it disagrees with the live oracle and would have produced wrong, non-interoperable
  IDs.

## Consequences

- The incorrect "= root dir checksum" statement was corrected everywhere (plan,
  rustdoc, tests) before the contract was frozen.
- A dedicated `snapshot_id` function exists rather than reusing the directory-checksum
  path.
- This was caught precisely because the port diffed against the live oracle (ADR-0001)
  rather than trusting the docs.
