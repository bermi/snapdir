# 0010 — Unix-only: drop the Windows target

Status: Accepted, 2026-06

## Context

The manifest format encodes octal POSIX permissions and follows symlinks. The Rust
walk and CLI use `std::os::unix::fs::PermissionsExt`, which does not exist on
`windows-msvc`. The release dry-run caught this: with the dist profile fixed, 6 of 7
targets built, but `x86_64-pc-windows-msvc` failed to compile for this structural
reason. The CI test matrix's Windows legs were likewise red.

## Decision

snapdir is a Unix tool. Drop the Windows target everywhere:

- Remove `x86_64-pc-windows-msvc` from the release build matrix and from the cargo-dist
  target list (and the now-dead `.zip`/PowerShell-installer bits).
- Drop `windows-latest` from the CI test matrix.

Keep the 6 Unix targets: `x86_64`/`aarch64` `unknown-linux-gnu`, `x86_64`/`aarch64`
`unknown-linux-musl` (static), and `x86_64`/`aarch64` `apple-darwin`.

## Alternatives considered

- **Port the permission/symlink handling to Windows** (`#[cfg]` shims, ACL mapping).
  Rejected: the manifest's octal-perms-and-symlinks model is a frozen Unix contract
  (ADR-0002); a faithful Windows mapping would be a separate, divergent behaviour with
  its own fixtures, and snapdir is fundamentally a Unix tool.
- **Keep Windows in CI but mark it allowed-to-fail.** Rejected: a perpetually red leg
  is noise, not a signal.

## Consequences

- The release ships 6 Unix targets, including the static-musl build.
- CI is green-able again on `ubuntu` + `macos` (+ the musl static leg).
- Windows users are out of scope; this is documented rather than half-supported.
- The release dry-run again proved its worth by catching a real target/scope issue
  before any publish.
