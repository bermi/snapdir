# 0002 — Freeze the manifest format and on-disk layout

Status: Accepted, 2026-06

## Context

The manifest is the interoperability boundary of snapdir. Its exact byte layout,
sort order, checksum rules, and the content-addressable directory layout all had to
match the Bash oracle so that caches and stores written by either tool remain mutually
readable. Without an explicit freeze, downstream components (stores, catalog, CLI) would
build against a moving target.

## Decision

Freeze the manifest format and layout once the core hashing was in place, exactly as
read from the Bash source:

- **Manifest line:** `PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH`, single-space
  separated, sorted by path (`sort -k5`). Empty lines are stripped; `#` lines are
  comments excluded from the checksum.
- `PATH_TYPE` is `F` or `D`; directory paths end in `/`; relative entries are prefixed
  `./`, `--absolute` keeps the full path.
- `PERMISSIONS` are octal POSIX (`stat -f '%A'` on macOS, `stat -c '%a'` on Linux).
- `SIZE` is content bytes; directory size is the sum of member sizes (directory
  metadata excluded).
- **Directory checksum** (the `D ./` line's `CHECKSUM` field): take the direct
  children's checksums, `cut`/`sort -u`/concatenate with no separators, then re-hash.
- **Content-addressable layout:** objects at
  `.objects/<h[0:3]>/<h[3:6]>/<h[6:9]>/<h[9:]>` and manifests at
  `.manifests/<id[0:3]>/<id[3:6]>/<id[6:9]>/<id[9:]>` — a 3-level shard, mirrored in
  the cache and in every store.

The freeze is guarded by the Rust golden tests
(`crates/snapdir-core/tests/compat_golden.rs`), which assert the frozen byte layout so
any accidental change to the format-defining code fails the test suite.

## Alternatives considered

- **A new, cleaner Rust-native format.** Rejected: it would break interoperability,
  which is the hard constraint of the whole port.
- **Document the format without an enforced guard.** Rejected: golden tests make
  accidental drift in the core files a hard failure rather than a silent change.

## Consequences

- Downstream components build against an immutable contract.
- Any change to the frozen byte layout fails the golden tests and requires explicit
  approval.
- The format carries forward a few oracle quirks (octal perms, dir-size summation)
  that are now contractually fixed rather than free to "improve".
