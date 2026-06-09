# stores handoff for pack-wire-format @ 2026-06-09T21:42:00Z

## Summary

Built `crates/snapdir-stores/src/pack.rs` (NEW, 1512 lines): the SNAPPACK 1 wire format per the approved Phase-4 spec, with the normative grammar + invariants in the module docs.

- **Constants** (single source of truth for the wire): `WIRE_VERSION = 1`, `WIRE_CAPS = ["objects-needed", "send-pack", "receive-pack"]`, `WIRE_MAGIC = "SNAPPACK 1\n"` (a unit test pins magic == `SNAPPACK {WIRE_VERSION}\n`), `MAX_HEADER_BYTES = 128`, `MAX_MANIFEST_BYTES = 64 MiB`, plus the shared `is_hex64` validator (`^[0-9a-f]{64}$`).
- **`write_pack(source: &dyn StreamStore, ids, manifest_id, out)`** — validates every id/manifest-id BEFORE emitting a single byte (fail closed); fetches + serializes the manifest UP FRONT (fail fast) but emits it LAST, serialized exactly as `file_store.rs::write_manifest` stores it (`to_string()` + trailing `\n`, the byte form `snapshot_id` hashes — ids round-trip, raw-hash re-verified against the id); obj records in input order, each re-verified to hash to its id after `get_object` (belt-and-braces over the store's read verify); any failure (incl. `ObjectNotFound`) aborts before `end` so consumers of the partial stream fail too. Returns `PackWriteReport{objects_written, manifest_written}`.
- **`read_pack(input, sink: &mut dyn PackSink)`** — header lines capped at 128 bytes WHILE reading (byte-at-a-time over an internal `BufReader`, rejected at the cap without buffering more); exact-version magic check; strict single-space/decimal-u64 header grammar; every payload flows through a `HashingTake` (length-limited reader + incremental `blake3::Hasher`), so verification is reader-owned and O(1)-memory; duplicates are verified-but-skipped (write-once; mismatch on ANY record — present or not — aborts the stream); claimed-vs-actual mismatch removes the staged temp and files NOTHING under the claimed address; manifest payload buffered (64 MiB cap + prealloc guard vs lying `len`), raw-hash-verified, `Manifest::parse`d, AND re-rendered through `snapdir_core::merkle::snapshot_id` to match the claimed id; held and committed ONLY at the `end` trailer (EOF before `end` = hard error, manifest never committed); records after the manifest rejected. Returns `PackReadReport{objects_written, objects_skipped, manifest_committed}`.
- **`PackSink` trait + two impls**: `StreamSink<'a>(&'a dyn StreamStore)` (buffers one obj record, commits via the store's own verify-before-write `put_object`, manifest via `put_manifest`) and `FileSink<'a>(&'a FileStore)` (streams payloads through `io::copy` into a temp sibling of `root().join(object_path(checksum))`, atomic rename on hash match — O(1) memory per record; `Drop` removes any orphaned temp as a last resort). The temp-sibling discipline is mirrored locally (file_store.rs's helper is private; `FileStore::root()` is already public, so no new accessor was needed).
- **`StreamStore::objects_needed`** added to stream.rs as a DEFAULTED method: validates every checksum via `pack::is_hex64` before the FIRST probe (fail closed, nothing returned on any invalid entry), then loops `has_object`, returning the absent subset preserving input order; dedup documented as the caller's job; batched S3/GCS overrides marked as a follow-up in the doc comment. All existing impls (FileStore/S3/GCS/B2) compile unchanged — workspace-wide `cargo check --workspace --all-targets --locked` green.
- lib.rs: `pub mod pack;` + re-exports + module-doc bullet, consistent with the stream/sync style.
- 23 gate tests in `pack::tests` (all matched by the `pack` filter): FileSink + StreamSink roundtrips (incl. 0-byte and 3 MiB objects), empty pack, manifest-only pack, header cap / bad magic / bad version / uppercase / 63 / 65 / non-hex / garbage-len / >u64 / extra-token / double-space, security (claimed-X-bytes-Y → nothing at X, no manifest, ZERO files incl. temps in the sink), mismatch-on-present-object aborts, truncation before `end` (objects filed, fully-read manifest NOT committed) + mid-payload truncation (temp removed), duplicate idempotency (1 written / 1 skipped), record-after-manifest, manifest wrong-id + unparseable-payload + over-cap, write-side missing-object/invalid-id/input-order, `objects_needed` complement-in-order + fail-closed.

**ONE DEVIATION (needs PM sign-off):** `blake3 = "1.8.5"` was added to `crates/snapdir-stores/Cargo.toml`, which touched **Cargo.lock by exactly one line** (the `blake3` entry in snapdir-stores' deps list — out of lane). Why: the spec's INCREMENTAL-BLAKE3 / O(1)-memory requirement is unimplementable via `snapdir_core::merkle::Hasher` (`hash_hex(&[u8])` only hashes complete buffers, and core is out of my lane). This is NOT a new supply-chain edge: it is the exact version requirement snapdir-core already declares, the lock graph gains no crate, and the deny.toml posture is unchanged. The alternative (full buffering per record) would violate a normative gate requirement.

## Files changed

```
 Cargo.lock                          |  1 +     (out-of-lane: +"blake3" in snapdir-stores' lock deps — see deviation)
 crates/snapdir-stores/Cargo.toml    |  9 +++++++
 crates/snapdir-stores/src/lib.rs    | 11 ++++++++
 crates/snapdir-stores/src/stream.rs | 51 +++++++++++++++++++++++++++++++++++++
 crates/snapdir-stores/src/pack.rs   | 1512 ++++ (NEW, untracked)
```

## Local verification result

`cargo test -p snapdir-stores pack --locked` (last 30 lines):

```
test pack::tests::pack_preseeded_object_is_skipped_but_verified ... ok
test pack::tests::pack_mismatched_object_files_nothing_and_leaves_no_temp ... ok
test pack::tests::pack_objects_needed_returns_absent_subset_in_input_order ... ok
test pack::tests::pack_write_invalid_id_emits_nothing ... ok
test pack::tests::pack_manifest_only_stream_completes_interrupted_push ... ok
test pack::tests::pack_write_emits_records_in_input_order ... ok
test pack::tests::pack_write_missing_object_aborts_before_end ... ok
test pack::tests::pack_roundtrip_stream_sink_generic ... ok
test pack::tests::pack_truncated_before_end_files_objects_but_never_manifest ... ok
test pack::tests::pack_truncated_mid_payload_keeps_earlier_objects_drops_partial ... ok
test pack::tests::pack_roundtrip_file_sink_streams_objects_and_manifest ... ok

test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 136 filtered out; finished in 0.41s

     Running tests/adaptive_wire.rs ... 0 passed; 0 failed; 8 filtered out
     Running tests/backoff_wire.rs  ... 0 passed; 0 failed; 6 filtered out
     Running tests/shim_external_store.rs ... 0 passed; 0 failed; 5 filtered out
```

Also green:
- `cargo test -p snapdir-stores --locked` — 159 lib + 8 + 6 + 5 integration + 1 doctest, 0 failed (no regressions).
- `cargo fmt --check -p snapdir-stores` — clean.
- `cargo clippy -p snapdir-stores --all-targets --locked -- -D warnings` — clean.
- `cargo doc -p snapdir-stores --no-deps` — 6 warnings, byte-identical to the set already on HEAD (verified via stash diff; zero new warnings from this change; the pre-existing ones are in b2_store/retry/transfer/lib's sync bullet).
- `cargo check --workspace --all-targets --locked` — whole workspace compiles (existing StreamStore impls + cli consumers unchanged).

## Reuse check / Blockers

- **Deps:** zero NEW crates in the graph; `blake3` (already pinned by snapdir-core at the same `1.8.5` requirement) added as a direct dep of snapdir-stores for incremental hashing — one-line Cargo.lock delta is the only out-of-lane edit; deny.toml untouched/unaffected. PM: please confirm this is acceptable or tell me to rework (the rework would have to violate the O(1)/incremental requirement).
- **Temp-sibling discipline:** mirrored from file_store.rs (`{name}.{pid}.{counter}.tmp` in the target's own directory, atomic same-filesystem rename, removal on every failure path + `FileSink::Drop` backstop); `file_store.rs` itself untouched. Frozen sharded keys reused via `snapdir_core::store::object_path`/`manifest_path`; manifest serialization matches `write_manifest` byte-for-byte (`to_string()` + `\n`).
- **Existing StreamStore impls compile unchanged** — `objects_needed` is defaulted; proven by the workspace check.
- **Cross-lane needs:** none for this gate. Follow-ups already doc-noted: batched `objects_needed` overrides for S3/GCS; send-pack CLI two-pass streaming rides the later `cli-plumbing` gate.

Ready for PM verification: YES
