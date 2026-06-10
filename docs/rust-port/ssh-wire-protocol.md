# snapdir SSH wire protocol — SNAPPACK 1 and the plumbing subcommands

> This document is the normative description of the wire protocol behind the
> `ssh://` store's acceleration path: the **SNAPPACK 1** pack stream, the
> capability/negotiation line, and the three hidden plumbing subcommands
> (`objects-needed`, `send-pack`, `receive-pack`). The implementation of
> record is `crates/snapdir-stores/src/pack.rs` (format + reader/writer) and
> `crates/snapdir-cli/src/cli.rs` (the plumbing CLI). If this prose and the
> code disagree, the code and its tests win — report the discrepancy.

The pack stream carries raw content-addressed objects (and at most one
manifest) between two snapdir processes, e.g.
`snapdir send-pack | ssh host 'snapdir receive-pack'`. Both ends of the pipe
are snapdir itself, so the format is deliberately minimal: no tar semantics,
no entry names, no padding.

---

## 1. SNAPPACK 1 grammar

```text
stream   := "SNAPPACK 1\n" record* "end\n"
record   := "obj " hex64 " " len "\n" payload(len)
          | "manifest " hex64 " " len "\n" payload(len)   ; at most one; must be the LAST record
hex64    := 64 lowercase hex chars, regex ^[0-9a-f]{64}$ (validated on read AND write)
len      := decimal u64
payload  := exactly len raw bytes, no padding/terminator
```

The magic line is exactly `SNAPPACK 1\n` — the version is baked into the
magic (`pack::WIRE_MAGIC`, pinned by test to `pack::WIRE_VERSION = 1`).

### 1.1 Limits

- **Header cap — 128 bytes.** Every header line, *including* its terminating
  `\n`, is at most `MAX_HEADER_BYTES = 128` bytes. The reader rejects a longer
  line the moment the cap is hit, without buffering more — this bounds reader
  memory before any validation happens. (The longest valid header,
  `manifest <hex64> <u64::MAX>\n`, is 95 bytes.)
- **Manifest cap — 64 MiB.** A `manifest` record's payload (which, unlike
  `obj` payloads, is buffered in memory until the `end` trailer commits it) is
  capped at `MAX_MANIFEST_BYTES = 64 MiB`.

### 1.2 Reader invariants

- **Streaming + incremental BLAKE3.** Every payload streams through an
  incremental BLAKE3 hasher while it is staged. On `file://` sinks, `obj`
  payload bytes stream through a fixed-size buffer into a temp sibling of the
  final object path (the same temp + atomic-rename discipline as the file
  store) — O(1) memory per record regardless of object size.
- **Verify-before-file.** An object is committed at its claimed
  content-address only if the computed hash equals the claimed `hex64`. A
  mismatch removes the staged bytes and aborts the WHOLE stream — everything
  after a corrupt record is tainted. The on-disk location is derived
  exclusively from the validated claimed checksum (the sharded
  `object_path`/`manifest_path` layout); there is no entry-name concept, so
  the path-traversal class is structurally absent.
- **Truncation never commits.** The optional `manifest` record must be the
  last record (any record after it is rejected); its payload is buffered and
  committed to the sink **only after the `end` trailer has been read**. EOF
  before `end` is a hard error and the manifest is NEVER committed — a
  truncated stream or dropped connection can file (verified) objects but can
  never make the snapshot observable, preserving the store-wide manifest-last
  invariant on the receiving side.
- **Manifest double-check.** The manifest payload must hash to the claimed
  snapshot id, must parse, and the parsed manifest must re-render to the same
  id — a payload that raw-hashes correctly but is not the canonical
  serialization is rejected.
- **Idempotent duplicates.** A duplicate `obj` record (or one whose object the
  sink already holds) is skipped (write-once), but its bytes are still
  consumed and hash-verified — the stream cannot seek, and a hash mismatch on
  ANY record aborts.

### 1.3 Writer invariants

- Every id (and the optional manifest id) is validated against
  `^[0-9a-f]{64}$` **before any byte is written**.
- The manifest is fetched and re-verified up front (fail fast) but emitted
  **last**; object records are emitted in input order, each re-verified to
  hash to its id before its record is written.
- Any failure — including a missing requested object — aborts **before** the
  `end` trailer is emitted, so a consumer of the partial stream fails too: no
  silent partial transfer.
- Duplicates in the input emit duplicate records; deduplication is the
  caller's job (the CLI plumbing dedupes, preserving first-occurrence order).

---

## 2. Capability line and negotiation

`snapdir version --capabilities` (a hidden flag on the `version` subcommand)
prints one line:

```text
snapdir <semver> wire=<u32> caps=<csv>
```

e.g. `snapdir 1.4.0 wire=1 caps=objects-needed,send-pack,receive-pack`.

- The grammar is space-separated `key=value` fields after the program name and
  semver; **unknown fields are ignored** by consumers (forward-compatible).
- **Negotiation keys on the exact `wire` integer match — NEVER on the
  semver.** A probe accepts a remote only when its line carries the exact
  ` wire=1 ` token and every required capability as a member of the `caps=`
  comma list (push requires `objects-needed,receive-pack`; fetch requires
  `send-pack`).
- Older snapdir versions error on the unknown `--capabilities` flag; the probe
  treats any output without a parsable capability line as "no acceleration" —
  clean degradation with no special code on that path. Plain `snapdir version`
  output is unchanged.

---

## 3. The plumbing subcommands

All three are hidden from the documented CLI surface and fail closed: every
checksum input is validated against `^[0-9a-f]{64}$` before any store access
or any output.

### 3.1 `snapdir objects-needed --store <url>`

- **stdin:** candidate object checksums, one per line.
- **stdout:** exactly the subset the store does NOT hold, one per line, in
  first-occurrence input order (input is deduped, order preserved).
- **Fail-closed:** ANY malformed line errors (nonzero exit) before the store
  is even resolved, with NOTHING on stdout — a malformed request is never
  partially answered. Empty input is valid and prints nothing.
- Routes through the same stream-store resolver as `sync`, so it works for
  `file://`, `s3://`, `gs://`, and `b2://`; external `snapdir-*-store` URLs
  are rejected.

### 3.2 `snapdir send-pack --store <url> --ids <FILE|-> [--manifest-id <id>]`

- **input:** `--ids` names a file listing one checksum per line (`-` reads
  stdin). The list is validated and deduped (first-occurrence order) before
  any store work; a malformed list emits not a single pack byte.
- **stdout:** the raw SNAPPACK stream — the byte stream is the entire stdout
  contract; the `sent pack: …` summary goes to stderr (suppressed by
  `--quiet`).
- With `--manifest-id`, that snapshot's manifest rides the pack as the LAST
  record.
- **exit:** nonzero on any failure, per the writer invariants above (abort
  before `end`, so the piped consumer fails too).

### 3.3 `snapdir receive-pack --store <url> [--require-manifest <id>]`

- **stdin:** a SNAPPACK stream, consumed per the reader invariants above.
  `file://` stores stream each payload through the O(1)-memory temp-sibling
  sink; every other stream store buffers one record at a time.
- `--require-manifest <id>` (validated up front) fails the command — after
  the read — unless the stream committed a manifest with EXACTLY that id;
  without the flag an objects-only stream is success.
- **stdout:** silent. The `received pack: …` summary goes to stderr
  (suppressed by `--quiet`).
- **exit:** nonzero on any protocol violation, hash mismatch, truncation, or
  an unmet `--require-manifest`.

---

## 4. Accelerated dataflows (`ssh://` store)

The emitted `ssh://` scripts embed BOTH the dumb tar pipeline and the
accelerated path and dispatch at script runtime (emit time has no
connection). `SNAPDIR_SSH_NO_ACCEL=1` — or no usable local `snapdir` binary —
forces the dumb path; `SNAPDIR_SSH_FORCE_ACCEL=1` turns a failed negotiation
into a designed error instead of a fallback. Both paths produce byte-identical
stores (the dumb-vs-accel oracle gate).

### 4.1 Push — 3 round trips

1. **Combined probe** (one round trip): `test -f <manifest path>` (`manifest=0|1`)
   plus `command -v snapdir && snapdir version --capabilities || echo 'caps
   none'`. A present manifest short-circuits (`Manifest already exists on
   store.`, exit 0). A probe *transport* failure surfaces the real ssh exit
   code — connectivity never masquerades as absence or as "no capabilities".
2. **Diff**: the manifest's deduped object checksums go to the remote
   `snapdir objects-needed --store file://<base>`, which answers with exactly
   the absent subset.
3. **Stream**: one local
   `snapdir send-pack --store file://<staging> --ids <missing> --manifest-id <id>`
   piped into the remote
   `snapdir receive-pack --store file://<base> --require-manifest <id>`.
   The manifest rides the pack as the last record and the remote commits it
   only after the verified `end` trailer — the manifest-last invariant is
   preserved end-to-end with no fourth round trip. An EMPTY missing set still
   streams the manifest-only pack (this is what completes a previously
   interrupted push).

### 4.2 Fetch — 2 round trips

1. **Caps-only probe** (skipped entirely when the emit-time cache diff leaves
   nothing to fetch).
2. **Stream**: the runtime-chosen id list (the emit-time cache diff, or the
   full list under `SNAPDIR_SSH_PULL_SENDALL=1`) feeds
   `ssh 'snapdir send-pack --store file://<base> --ids -'` piped into a LOCAL
   `snapdir receive-pack --store file://<cache>` (no `--require-manifest` —
   the manifest already arrived via `get-manifest-command`). The remote
   stream is fully untrusted: the local receive-pack incrementally
   BLAKE3-verifies every record in O(1) memory.

### 4.3 Fallback policy

- A probe or diff failure (ssh reachable, plumbing absent/broken) falls back
  to the dumb path: nothing has been written yet, and the dumb path is
  idempotent.
- A failure of the send|receive **stream itself** exits nonzero with a retry
  hint and NEVER silently retries on the dumb path — a mid-stream failure is
  likely environmental and would hit the dumb path too, and a user retry
  resumes incrementally for free (objects already filed are verified-then-
  skipped; the manifest was never committed).
