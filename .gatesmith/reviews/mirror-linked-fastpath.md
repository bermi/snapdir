# Design lock — linked-mode checksum-reuse fast path (Phase 32)

> Gate: `mirror-linked-fastpath-design` (human ✋). Operator-proposed 2026-06-19.
> Plan: `~/.claude/plans/cosmic-knitting-tide.md` (Linked-mode section).
> Status: PROPOSED — awaiting operator approval.

## Problem / opportunity

In `--linked` mode a checkout's destination entries are **symlinks into the
content-addressed object store** (`.objects/<h0:3>/<h3:6>/<h6:9>/<h9:>`), whose
path **mechanically encodes the file's BLAKE3 hash**
(`crates/snapdir-core/src/store.rs:79-118`). Therefore a re-snapshot
(`snapdir id` / `manifest`) of a linked tree can **recover each file's content
checksum directly from the symlink target's object path — without reading or
hashing a single byte.** Precedent for trusting a content-addressed object's
address already exists: `CopyTrust::TrustedObject`
(`crates/snapdir-stores/src/file_store.rs:100-112`) skips re-hashing on the
fetch/clone path.

## Verified findings (read-only investigation, this conversation)

1. **Hash recovery is mechanical** — the sharded object path is invertible to the
   full 64-hex BLAKE3 (`store.rs:79-118`); no helper exists yet but the inverse is
   trivial (cf. `manifest_id_from_shard_segments`, `stream.rs:190-203`).
2. **CORRECTION to the naive premise (load-bearing):** when walk follows a
   symlink it records the **symlink's OWN `lstat` mode and size**, NOT the
   target's (`crates/snapdir-core/src/walk.rs:562-567,614-619`). The checksum is
   read *through* the link (correct content hash) but `size = lstat(symlink).len()`
   and `mode` = the symlink's. The original file's mode+size live ONLY in the
   source manifest. ⇒ a linked re-snapshot does **not** reproduce the original
   snapshot id by itself. **This fast path is CHECKSUM-ONLY.**
3. **Object store always addresses by plain BLAKE3** (`file_store.rs:42,264`). A
   non-default `--checksum-bin` (md5/sha256) or keyed `SNAPDIR_MANIFEST_CONTEXT`
   makes the embedded hash the **wrong algorithm** ⇒ must re-hash.
4. **Trusting an address bypasses verify-on-read** (`clone_skip.rs:53-67`) ⇒ the
   existing strict override `SNAPDIR_VERIFY_COPIES=1` must force a content re-hash.

## Locked design

### Eligibility predicate (ALL must hold; else fall back to normal read+hash)
- entry is a **symlink** whose canonical target is an **object under a KNOWN
  local `.objects` store** and parses to a valid 64-hex object key;
- the requested checksum algorithm == the store's addressing algorithm —
  **plain, non-keyed BLAKE3** (no `--checksum-bin`, no `SNAPDIR_MANIFEST_CONTEXT`);
- **not** `SNAPDIR_VERIFY_COPIES=1` (strict override forces content re-hash).

### Failure-mode fallbacks (never trust, never panic)
- non-default / keyed checksum algo → re-hash content;
- target escaped the store / not an object path → normal followed-symlink hash;
- dangling target (object GC'd/missing) → typed `WalkError` (no panic);
- strict-verify on → re-hash content;
- mixed tree → per-entry decision.

### Scope decision (operator-locked)
- **CHECKSUM-ONLY.** The fast path recovers the content checksum; it does NOT, by
  itself, reproduce the original snapshot id (SIZE/PERMISSIONS still come from
  walk's `lstat`). Faithful round-trip of a linked tree needs the source
  manifest — documented limitation, NOT a goal of this path.

### KEYSTONE invariant
- The recovered checksum is **byte-identical** to the hash that reading the
  content would produce, on a healthy store ⇒ a manifest computed with the fast
  path is identical to one computed by hashing, for eligible entries. Adversary
  must pin this (incl. proving NO content read on the default path, e.g. corrupt
  the object's bytes while keeping its address → default fast path yields the
  address hash without reading the garbage; `SNAPDIR_VERIFY_COPIES=1` re-hashes
  and ERRORS).

### Lane / shape
- **Lane = `core`.** Additive, **opt-in**: the walk/hash path gains an optional
  "object-store roots" hint defaulting to **none**, so existing call sites compile
  unchanged and there is **NO frozen manifest-format change** (compute path only).
  CLI passes the local store root when known.

## Out of scope (this triple)
- Wiring the hint from the CLI for real (a later/small cli touch) — the triple
  proves + lands the core capability behind the opt-in hint.
- Any change to mode/size fidelity of linked re-snapshots.

## Gate triple that follows this lock
`mirror-linked-fastpath-spec-tests` (adversary) → `mirror-linked-fastpath-impl`
(core) → `mirror-linked-fastpath-review` (adversary). `mirror-docs` already
depends on the review so the behavior + its checksum-only caveat get documented.
