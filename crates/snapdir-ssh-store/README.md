# snapdir-ssh-store

`ssh://` and `sftp://` external stores for
[snapdir](https://github.com/snapdir/snapdir) — content-addressable directory
snapshots — driving the **system OpenSSH client** (no SSH reimplementation,
zero crypto dependencies) through snapdir's documented emit-command store
contract.

Ships two binaries:

- `snapdir-ssh-store` — the `ssh://` engine; requires a POSIX shell on the
  remote host and (in a later phase) accelerates transfers when a compatible
  `snapdir` exists remotely.
- `snapdir-sftp-store` — the `sftp://` engine; speaks pure SFTP and works
  against restricted accounts (`ForceCommand internal-sftp` chroots).

Both enforce an un-weakenable modern-only security floor (OpenSSH >= 8.5,
pinned key-exchange/cipher/host-key algorithm lists, `BatchMode`,
`StrictHostKeyChecking`) — user-supplied extra options can never weaken it.

It is part of the snapdir project. Full documentation and the CLI are at
**[snapdir.org](https://snapdir.org)**; the source lives in the
[canonical repository](https://github.com/snapdir/snapdir).
