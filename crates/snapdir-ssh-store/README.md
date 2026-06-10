# snapdir-ssh-store

`ssh://` and `sftp://` external stores for
[snapdir](https://github.com/snapdir/snapdir) — content-addressable directory
snapshots — driving the **system OpenSSH client** (no SSH reimplementation,
zero crypto dependencies) through snapdir's emit-command external-store
contract: each subcommand transfers nothing itself, it *prints a bash script*
on stdout that the snapdir CLI runs.

Ships two binaries (install both with `cargo install snapdir-ssh-store`; they
must be on `PATH` where the snapdir CLI runs):

- `snapdir-ssh-store` — the `ssh://` engine. Requires a POSIX shell on the
  remote host; transfers run as a batched existence probe plus a single
  `tar | ssh` pipeline, and **auto-accelerate** to a SNAPPACK pack stream when
  a wire-compatible `snapdir` is installed on the remote (graceful fallback
  otherwise).
- `snapdir-sftp-store` — the `sftp://` engine. Speaks pure SFTP — no remote
  shell, no `tar` — so it works against restricted accounts, including
  `ForceCommand internal-sftp` chroots.

`ssh://` and `sftp://` are distinct schemes, not aliases. Use `ssh://` when
the remote gives you a shell; use `sftp://` for restricted accounts.

## URL grammar

```text
ssh://[user@]host[:port]/abs/base/path
sftp://[user@]host[:port]/abs/base/path
```

- Embedded passwords (`user:pw@`) are rejected — authenticate with an SSH key
  (`IDENTITY_FILE`) or an ssh-agent (`BatchMode=yes` would make a password
  unusable anyway).
- `user`: `[A-Za-z0-9._-]+`, not starting with `-`; `host`:
  `[A-Za-z0-9.-]+`, not starting with `-`, or a bracketed IPv6 literal
  (`[::1]`); `port`: 1–65535.
- The base path is taken as literal bytes (never percent-decoded), the
  trailing `/` is trimmed, control characters are rejected, and the bare root
  `/` is rejected.

## Configuration

Each engine reads its own env family: `SNAPDIR_SSH_STORE_*` for `ssh://`,
`SNAPDIR_SFTP_STORE_*` for `sftp://`.

| Variable (suffix) | Default | Meaning |
| --- | --- | --- |
| `IDENTITY_FILE` | — | Private key path; also sets `IdentitiesOnly=yes` |
| `KNOWN_HOSTS` | — | `UserKnownHostsFile` override |
| `PORT` | — | Remote port; a port in the store URL wins |
| `CONNECT_TIMEOUT` | `10` | `ConnectTimeout` seconds |
| `JOBS` | `4` | Transfer parallelism; falls back to `SNAPDIR_JOBS`, then `SNAPDIR_MAX_JOBS` |
| `CONTROL_PERSIST` | `60` | `ControlMaster` linger seconds |
| `UMASK` | `077` | Umask for remote writes (`ssh://` engine only; the sftp engine uses explicit `chmod 600` instead) |
| `EXTRA_OPTS` | — | Whitespace-separated `Key=Value` ssh options, appended **last** |

Every operation multiplexes its ssh/sftp invocations over one
`ControlMaster=auto` connection (one TCP + auth handshake per operation),
closed on exit with `ControlPersist` as the leak backstop. Anything the
engines don't set — `ProxyJump`, agent, `~/.ssh/config` host blocks — keeps
working.

## The security floor

Every `ssh`/`sftp` invocation starts with this exact ordered flag list:

```text
-o BatchMode=yes
-o StrictHostKeyChecking=yes
-o PasswordAuthentication=no
-o KbdInteractiveAuthentication=no
-o ClearAllForwardings=yes
-o KexAlgorithms=sntrup761x25519-sha512@openssh.com,curve25519-sha256,curve25519-sha256@libssh.org
-o Ciphers=chacha20-poly1305@openssh.com,aes256-gcm@openssh.com,aes128-gcm@openssh.com
-o HostKeyAlgorithms=ssh-ed25519-cert-v01@openssh.com,ssh-ed25519,rsa-sha2-512-cert-v01@openssh.com,rsa-sha2-256-cert-v01@openssh.com,rsa-sha2-512,rsa-sha2-256,ecdsa-sha2-nistp256-cert-v01@openssh.com,ecdsa-sha2-nistp384-cert-v01@openssh.com,ecdsa-sha2-nistp521-cert-v01@openssh.com,ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521
-o ConnectTimeout=<CONNECT_TIMEOUT>
```

Config-derived options (`Port`, `User`, `IdentityFile` + `IdentitiesOnly`,
`UserKnownHostsFile`) come next, and `EXTRA_OPTS` tokens always come **last**.
OpenSSH resolves every option first-obtained-value-wins (and command-line
options beat `~/.ssh/config`), so the floor structurally cannot be weakened:
`EXTRA_OPTS="StrictHostKeyChecking=no"` is inert because the floor's `=yes`
was already obtained. `MACs` is deliberately not pinned — every floor cipher
is AEAD, so OpenSSH ignores MAC negotiation for them.

### OpenSSH ≥ 8.5 policy

The local `ssh -V` is checked at emit time and anything older than **8.5** —
or unparsable, including non-OpenSSH clients — **fails closed**, with no
override env. 8.5 (March 2021) is the oldest release shipping every algorithm
on the floor (notably `sntrup761x25519-sha512@openssh.com`). This excludes,
for example, the stock clients of RHEL/CentOS 8 (OpenSSH 8.0), Ubuntu 20.04
(8.2), Debian 11 (8.4), and macOS 11 and older; upgrade the local
OpenSSH client to use these stores. The remote *server* is not version-gated
— it only needs to offer at least one algorithm from each pinned list.

## Acceleration (`ssh://` only)

The emitted `ssh://` scripts embed both a "dumb" tar-pipeline path and an
accelerated SNAPPACK path, and pick one at runtime. A push probes the remote
in ONE round trip (manifest presence + `snapdir version --capabilities`); the
accelerated path is taken only when the remote's capability line carries the
exact `wire=1` token and the needed capabilities
(`objects-needed`/`receive-pack` for push, `send-pack` for fetch) — wire
negotiation is an exact integer match, never a semver comparison. Accelerated
push then diffs the object list remotely (`snapdir objects-needed`) and
streams only the missing objects through one
`snapdir send-pack | ssh 'snapdir receive-pack'` pipe, with the manifest as
the last record, committed remotely only after the verified `end` trailer.
Accelerated fetch streams `ssh 'snapdir send-pack --ids -'` into a local
`snapdir receive-pack`, which BLAKE3-verifies every record — the remote
stream is untrusted. Full protocol:
[docs/rust-port/ssh-wire-protocol.md](https://github.com/snapdir/snapdir/blob/main/docs/rust-port/ssh-wire-protocol.md).

Probe or diff failures (ssh reachable, plumbing absent) fall back to the dumb
path — nothing has been written yet and both paths produce byte-identical
stores. A failure of the pack stream itself exits nonzero and is never
silently retried on the dumb path; re-running the push/fetch resumes
incrementally.

Runtime toggles (read by the emitted script):

| Variable | Effect |
| --- | --- |
| `SNAPDIR_SSH_NO_ACCEL=1` | Always use the dumb path |
| `SNAPDIR_SSH_FORCE_ACCEL=1` | Error (exit 1) instead of falling back when the remote lacks the plumbing |
| `SNAPDIR_SSH_PULL_SENDALL=1` | Accelerated fetch requests the FULL object list instead of the local cache diff |
| `SNAPDIR_SSH_LOCAL_SNAPDIR=<path>` | Test/debug: which LOCAL `snapdir` binary anchors the pipe ends (default: `snapdir` on `PATH`) |

## Troubleshooting

- **`SNAPDIR_SSH_FORCE_ACCEL=1, but <host> does not offer the accelerated
  plumbing`** — the remote has no `snapdir` on `PATH`, or one that predates
  (or postdates) `wire=1`. The error echoes what the probe returned; install
  or upgrade snapdir on the host, or unset `SNAPDIR_SSH_FORCE_ACCEL`.
- **Host-key failures are fail-closed by design.** `StrictHostKeyChecking=yes`
  is part of the un-weakenable floor, so an unknown or changed host key always
  fails — `EXTRA_OPTS="StrictHostKeyChecking=no"` cannot bypass it. Add the
  host key to your known-hosts file (point `KNOWN_HOSTS` at a custom one if
  needed) before pushing.
- **`cannot parse OpenSSH version` / `OpenSSH <x>.<y> is too old`** — the
  local client is below the 8.5 floor (or isn't OpenSSH); upgrade it. There is
  no override.
- **`snapdir sync` rejects ssh/sftp stores** — by design: external stores have
  no in-process streaming surface. Use `push`/`fetch`/`pull`/`checkout`.

It is part of the snapdir project. Full documentation and the CLI are at
**[snapdir.org](https://snapdir.org)**; the source lives in the
[canonical repository](https://github.com/snapdir/snapdir).

## License

MIT
