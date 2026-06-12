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

---

## 5. SNAPPACK 1Z — zstd transport encoding

SNAPPACK 1Z is an **additive** transport encoding: the same record grammar,
the whole post-magic body compressed once. It is opt-in, sniffed on read, and
never changes the wire version.

```text
stream := "SNAPPACK 1Z\n" zstd_frame( record* "end\n" )
```

- **Magic.** The stream opens with the exact line `SNAPPACK 1Z\n`
  (`pack::WIRE_MAGIC_ZSTD`). The trailing `Z` is a transport-encoding marker,
  **not** a new format version: `pack::WIRE_VERSION` stays `1` and the
  capability line still reports `wire=1` (see negotiation below).
- **Single-frame framing.** Everything after the magic is **one** zstd frame.
  Decompressing it yields the *verbatim* SNAPPACK 1 body — `record* "end\n"`,
  the unchanged v1 record grammar, byte-for-byte. There is no per-record
  framing, no chunking, no length prefix: the whole body is one frame.
- **Decompressed-bytes bounds.** The reader sniffs the magic, wraps the
  remaining input in a streaming zstd decoder, and feeds the **decompressed**
  bytes to the *same* parser as v1. Every bound applies to the decompressed
  stream exactly as in §1.1: `MAX_HEADER_BYTES = 128` (header line incl. `\n`),
  `MAX_MANIFEST_BYTES = 64 MiB` (buffered `manifest` payload), and the
  lying-`len` preallocation cap. Object payloads still stream through the
  O(1)-memory temp-sibling path.
- **Decompression-bomb reasoning.** A hostile peer can craft a frame that
  decompresses to far more than it sends, but the cost is **CPU only**: every
  decompressed byte still flows through the incremental BLAKE3 hasher and must
  match its claimed `hex64`, and the header/manifest caps above bound buffered
  memory regardless of how large the frame inflates. A bomb cannot file a
  forged object (the hash will not match), cannot commit a manifest (truncation
  never reaches `end`), and cannot exhaust memory (the caps are enforced on the
  decompressed bytes). It can only waste decode CPU on a stream that is then
  rejected.
- **Compression level.** The encoder writes at `DEFAULT_ZSTD_LEVEL = 3` by
  default and accepts `MIN_ZSTD_LEVEL..=MAX_ZSTD_LEVEL` = `1..=19`; the library
  is environment-free and **clamps** the requested level into that range in
  `write_pack` (an out-of-range request is clamped, not rejected). The level
  affects only the sender's frame; the reader needs no level — zstd is
  self-describing.

### 5.1 Negotiation — sniff on read, advertise to write

The receiver is **always** ready for 1Z: `read_pack` sniffs the magic line and
accepts `SNAPPACK 1\n` **and** `SNAPPACK 1Z\n` forever — there is no flag, no
mode, no version gate on the read side. Forward and backward compatible by
construction.

The **sender** emits 1Z only when negotiation confirms BOTH ends support it:

- `snappack-zstd` is appended to `pack::WIRE_CAPS` (so `version --capabilities`
  advertises it), but **`wire=1` is unchanged** — an integer version bump would
  force older peers into a dumb fallback, whereas an additive *capability token*
  is silently ignored by a peer that lacks it. A 1.5.0 peer simply never sees a
  1Z stream and the v1 acceleration is still taken.
- In the `ssh://` engine, `snappack-zstd` (the `ZSTD_CAP` const) is **never**
  part of the `wire=1`/`objects-needed,receive-pack` acceleration gate; it is a
  second, independent check layered on top. The emitted push script probes the
  **local** binary's caps (`snapdir version --capabilities`, guarded so an older
  local that rejects the flag degrades to v1 cleanly) into `snapdir_local_zstd`,
  and chooses `--pack-format zstd` only when `snapdir_local_zstd = 1` **AND** the
  remote's `caps=` list contains `snappack-zstd`. Otherwise it sends v1.
- The `--pack-format zstd` token is always a **static literal** baked into the
  script — no environment value is ever interpolated into a baked remote command
  line. Fetch bakes **two** static remote `send-pack` variants (with and without
  ` --pack-format zstd`) and picks one at runtime by the same caps check.

The net effect: 1.5.0 ↔ 1.5.0 stays v1 (the cap is absent on both ends),
1.5.0 ↔ newer falls back to v1 (with the v1 acceleration still taken), and
newer ↔ newer negotiates 1Z — all without any wire-version change.

---

## 6. Durability

The receive side is batched and manifest-last, and the durability guarantee is
deliberately scoped to **no more than git claims**.

- **Batch model.** `read_pack` files every verified `obj` as it streams, then
  in the `end` arm calls a single `flush_barrier()` that forces **every object
  this pack committed** to stable storage, and only **then** commits the
  manifest via `put_manifest`. There are exactly two full syncs per pack (the
  object barrier, then the durable manifest write) — durability is amortized
  across the whole pack rather than paid per object.
- **Journal-ordering argument.** Because the barrier runs **before**
  `put_manifest`, a durable manifest provably implies durable objects: any
  manifest that survives a crash is backed by objects that also survived. A
  crash mid-stream can leave (verified) loose objects on disk but never an
  observable snapshot — the manifest-last invariant (§1.2) holds across the
  crash boundary, exactly as it holds across a dropped connection. The barrier
  runs unconditionally (even for a manifest-only / empty pack) so the ordering
  contract is independent of object count.
- **`SNAPDIR_FSYNC`.** The receive-pack CLI seam reads `SNAPDIR_FSYNC` and
  selects the sink's durability mode:
  - unset / empty / `batch` ⇒ `Durability::Batch` (the **default**) — the
    batched barrier above is active;
  - `off` ⇒ `Durability::Off` — the barrier is a no-op (byte-identical to the
    pre-durability behavior; rely on the OS to flush);
  - any other value is a **hard error** (fail closed, no silent downgrade).
- **Measured cost.** The default `batch` is not free: fsync-ing many tiny
  files before the manifest costs **~20%** on a small-files receive — measured
  **v1 +19.5%, zstd +29.9%** on a 5,000 × 4 KiB push received over this
  SNAPPACK path on a Linux CI runner. This is the price of crash-safety-by-
  default, not a bug — it is a fixed per-object fsync cost and is therefore
  worst on a small-files-dominated receive; a snapshot of fewer/larger objects
  pays proportionally less. The cost lands **only** on this receive-pack path
  (the ssh/store side accepting a push); the ordinary `file://`/S3/GCS push
  path is untouched. `SNAPDIR_FSYNC=off` trades the guarantee for the speed
  (a crash mid-receive can then leave a corrupt snapshot); the operator
  decision is to **keep `batch` the default** and accept the cost.
- **Non-journaling-fs caveat.** On a journaling filesystem with sane mount
  options, the fsync ordering above gives the manifest-last crash-consistency
  argument real teeth. On filesystems or mount configurations that do not order
  metadata and data the way fsync assumes, the guarantee weakens — this is the
  **same caveat git carries**, and snapdir claims no more than git does about
  crash safety. `SNAPDIR_FSYNC` controls when we ask the OS to flush; it cannot
  make a filesystem honor an ordering it does not implement.
