# 0027 — SSH/SFTP stores via system OpenSSH + wire-versioned plumbing

Status: Accepted, 2026-06

## Context

Users want to push/pull snapshots to plain SSH hosts, including restricted
accounts that only offer SFTP (`ForceCommand internal-sftp` chroots). snapdir
already routes any unknown `--store` scheme to an external `snapdir-<scheme>-store`
binary via the emit-command contract, so new schemes need no router change.
The decision drivers:

- **No new crypto dependencies.** The deny.toml posture is deliberately pinned
  to the ring-backed rustls stack (ADR-0004); a native SSH implementation
  (russh) would import a large independent crypto tree.
- **Performance.** Per-object round trips over SSH are prohibitive; OpenSSH's
  `ControlMaster` multiplexing amortizes one TCP+auth handshake per operation,
  and a remote manifest-diff turns O(N) existence probes into O(1) round trips
  when snapdir is installed remotely.
- **Restricted accounts.** A meaningful share of SSH endpoints are
  sftp-only chroots with no shell, so a pure-SFTP engine must exist alongside
  the shell engine.
- **Integrity.** Remote writes must keep the verify-then-rename and
  manifest-last disciplines the native stores guarantee.

## Decision

Ship a new workspace crate `crates/snapdir-ssh-store` (sole dependency
`snapdir-core`) with two binaries implementing the emit-command contract over
the **system OpenSSH client**:

- `snapdir-sftp-store` (`sftp://`) — pure SFTP batchfiles; works with no
  remote shell.
- `snapdir-ssh-store` (`ssh://`) — remote POSIX shell + a batched-probe /
  single-`tar | ssh`-pipeline dumb path, plus a runtime-negotiated
  acceleration: hidden CLI plumbing (`snapdir version --capabilities`,
  `objects-needed`, `send-pack`, `receive-pack`) speaking the custom
  **SNAPPACK 1** pack format (`docs/rust-port/ssh-wire-protocol.md`), with
  the remote end BLAKE3-verifying every record and committing the manifest
  only after the verified `end` trailer.

Both engines emit an **ordered, un-weakenable security floor** ahead of every
ssh/sftp invocation (modern-only kex/AEAD ciphers/host keys,
`StrictHostKeyChecking=yes`, `BatchMode=yes`; user `EXTRA_OPTS` are appended
last, so OpenSSH's first-obtained-value-wins rule makes them unable to weaken
the floor), and fail closed below OpenSSH 8.5. Compatibility negotiation keys
on an exact `wire=<u32>` integer match — never on semver.

## Alternatives considered

- **Native russh `StreamStore` (in-process SSH).** Deferred, not rejected: it
  is the only way to support `sync` over ssh, but it imports a large crypto
  tree into the deny.toml ring posture for a use case nobody has asked for
  yet. Revisit when a sync-to-ssh need exists.
- **Raw `tar | ssh` without remote verification.** Rejected: a dumb remote
  `tar -x` cannot hash payloads or commit the manifest last, breaking the
  verify-then-rename discipline every other store guarantees (and exposing
  the tar entry-name attack surface).
- **The `tar` crate for the pack format.** Rejected: both ends of the pack
  pipe are snapdir itself, so a ~40-line custom format (SNAPPACK) avoids tar
  semantics — entry names, padding, permission metadata — entirely, with zero
  new dependencies and no path-traversal class.

## Consequences

- `snapdir sync` does not support `ssh://`/`sftp://` (external stores have no
  in-process streaming surface); `push`/`fetch`/`pull`/`checkout` all work.
  Documented limitation.
- Wire compatibility is governed by the `wire` integer, independent of
  release versions; older/newer remotes degrade gracefully to the dumb path,
  which stays byte-identical to the accelerated path (oracle-gated).
- The local OpenSSH client must be ≥ 8.5 (fail-closed, no override); stock
  clients on e.g. RHEL 8 / Ubuntu 20.04 / Debian 11 are excluded by policy.
- The transport inherits the user's SSH ecosystem (agent, `~/.ssh/config`,
  `ProxyJump`, known_hosts) instead of reimplementing it; host-key trust is
  fail-closed and cannot be disabled through snapdir.
