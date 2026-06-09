//! SNAPPACK 1 — the snapdir pack wire format.
//!
//! A pack is a single self-verifying byte stream that carries raw
//! content-addressed objects (and at most one manifest) between two snapdir
//! processes, e.g. `snapdir send-pack | ssh host 'snapdir receive-pack'` — the
//! acceleration path of the upcoming `ssh://` store. Both ends of the pipe are
//! snapdir itself, so the format is deliberately minimal (no tar semantics, no
//! entry names, no padding).
//!
//! # Grammar (normative)
//!
//! ```text
//! stream   := "SNAPPACK 1\n" record* "end\n"
//! record   := "obj " hex64 " " len "\n" payload(len)
//!           | "manifest " hex64 " " len "\n" payload(len)   ; at most one; must be the LAST record
//! hex64    := 64 lowercase hex chars, regex ^[0-9a-f]{64}$ (validated on read AND write)
//! len      := decimal u64
//! payload  := exactly len raw bytes, no padding/terminator
//! ```
//!
//! # Invariants
//!
//! - **Header memory bound:** every header line (including its terminating
//!   `\n`) is at most [`MAX_HEADER_BYTES`] bytes. The reader rejects a longer
//!   line as soon as the cap is hit, without buffering more.
//! - **Verify-before-file:** an `obj` payload streams through an INCREMENTAL
//!   BLAKE3 hasher while it is staged; it is committed at its claimed
//!   content-address only if the computed hash equals the claimed `hex64`. A
//!   mismatch removes the staged bytes (temp file) and aborts the WHOLE stream
//!   with [`StoreError::Integrity`] — a corrupt stream taints everything after
//!   it, so nothing past the bad record is trusted.
//! - **Manifest-last / commit-at-`end`:** the optional `manifest` record must
//!   be the last record (any record after it is rejected), its payload is
//!   buffered (capped at [`MAX_MANIFEST_BYTES`]), and it is committed to the
//!   sink only after the `end` trailer has been read. EOF before `end` is a
//!   hard error and the manifest is NEVER committed — so a truncated stream or
//!   dropped connection can file (verified) objects but can never make the
//!   snapshot observable, preserving the store-wide manifest-last invariant.
//! - **Idempotent duplicates:** a duplicate `obj` record is skipped
//!   (write-once), but its bytes are still read and hash-verified — the stream
//!   cannot seek, and a hash mismatch on ANY record (present or not) aborts.
//! - **No path input:** the on-disk location of every payload is derived
//!   exclusively from the validated claimed checksum
//!   ([`snapdir_core::store::object_path`] /
//!   [`snapdir_core::store::manifest_path`]); there is no entry-name concept,
//!   so the path-traversal class is structurally absent.
//!
//! # Memory profile
//!
//! [`read_pack`] into a [`FileSink`] is O(1) memory per record regardless of
//! object size: payload bytes stream through a fixed-size buffer into a temp
//! sibling of the final object path (the same temp+atomic-rename discipline as
//! `file_store.rs`) while the incremental hasher runs. The generic
//! [`StreamSink`] buffers ONE object record at a time (its
//! [`StreamStore::put_object`] primitive takes whole buffers); the manifest
//! record is always buffered, capped at [`MAX_MANIFEST_BYTES`].
//!
//! [`write_pack`] reads one object at a time via
//! [`StreamStore::get_object`] (one whole object buffered at a time; the
//! send-pack CLI layers any further streaming on top in a later gate).

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use snapdir_core::manifest::Manifest;
use snapdir_core::merkle::{snapshot_id, Blake3Hasher, Hasher};
use snapdir_core::store::{manifest_path, object_path, StoreError};

use crate::file_store::FileStore;
use crate::stream::StreamStore;

/// The pack wire-format version this build speaks. Single source of truth for
/// the wire: the capability line (`snapdir version --capabilities`) bakes this
/// value, and [`read_pack`] negotiates on an exact integer match only.
pub const WIRE_VERSION: u32 = 1;

/// The plumbing capabilities this build advertises alongside [`WIRE_VERSION`].
pub const WIRE_CAPS: &[&str] = &["objects-needed", "send-pack", "receive-pack"];

/// The exact magic line that opens every pack stream (version baked in; a
/// unit test pins it to [`WIRE_VERSION`]).
pub const WIRE_MAGIC: &str = "SNAPPACK 1\n";

/// Hard cap on a header line, INCLUDING its terminating `\n`. The reader
/// rejects a longer line the moment the cap is reached — this bounds reader
/// memory before any validation happens. (The longest valid header —
/// `manifest <hex64> <u64::MAX>\n` — is 95 bytes, so the cap is comfortable.)
pub const MAX_HEADER_BYTES: usize = 128;

/// Hard cap on a `manifest` record's payload, which (unlike `obj` payloads)
/// is buffered in memory until the `end` trailer commits it.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

/// Cap on the up-front `Vec` preallocation for a buffered payload, so a
/// header that LIES about a huge `len` (while sending few bytes) cannot force
/// a giant allocation before a single payload byte arrives. The buffer still
/// grows organically with the bytes actually received.
const STAGE_PREALLOC_CAP: u64 = 8 * 1024 * 1024;

/// Returns `true` when `s` is a syntactically valid snapdir content address:
/// exactly 64 lowercase hex characters (`^[0-9a-f]{64}$`).
///
/// This is the wire's single checksum validator — used by [`write_pack`]
/// (before emitting a record), [`read_pack`] (on every record header), and
/// [`StreamStore::objects_needed`] (fail-closed input validation).
#[must_use]
pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What [`write_pack`] emitted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackWriteReport {
    /// Number of `obj` records emitted (duplicates in `ids` emit duplicate
    /// records — deduplication is the caller's job).
    pub objects_written: u64,
    /// Whether a `manifest` record was emitted (i.e. `manifest_id` was given).
    pub manifest_written: bool,
}

/// What [`read_pack`] filed into its sink.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackReadReport {
    /// Objects verified and newly committed to the sink.
    pub objects_written: u64,
    /// Objects whose bytes were read and hash-verified but NOT rewritten
    /// because the sink already had them (idempotent duplicates).
    pub objects_skipped: u64,
    /// Whether a manifest was committed (only ever `true` after a verified
    /// `manifest` record AND the `end` trailer).
    pub manifest_committed: bool,
}

/// Where [`read_pack`] files verified records.
///
/// The reader owns the framing, the incremental hashing, and the
/// verify-before-commit decision; a sink only stages bytes and
/// commits/aborts on the reader's instruction:
///
/// 1. [`has_object`](Self::has_object) — skip-but-verify gate for duplicates.
/// 2. [`stage_object`](Self::stage_object) — pull the (length-limited) payload
///    into staging (temp file / memory buffer). Called only for absent
///    objects; the reader hashes every byte the sink pulls.
/// 3. [`commit_object`](Self::commit_object) on hash match, or
///    [`abort_object`](Self::abort_object) on mismatch/truncation/error —
///    abort must leave NOTHING behind (no temp files, no partial objects).
/// 4. [`put_manifest`](Self::put_manifest) — called only after the `end`
///    trailer, with an id the reader has already verified.
pub trait PackSink {
    /// Returns `true` when the sink already holds `checksum` (the record's
    /// bytes will then be verified-and-discarded rather than re-written).
    fn has_object(&mut self, checksum: &str) -> Result<bool, StoreError>;

    /// Stages the payload for `checksum` by reading `payload` to EOF (the
    /// reader has already limited it to exactly `len` bytes). Must not make
    /// the object observable at its final address yet. On error the sink must
    /// clean up after itself or tolerate the follow-up
    /// [`abort_object`](Self::abort_object) call.
    fn stage_object(
        &mut self,
        checksum: &str,
        len: u64,
        payload: &mut dyn Read,
    ) -> Result<(), StoreError>;

    /// Commits the staged payload at its (reader-verified) content-address.
    fn commit_object(&mut self, checksum: &str) -> Result<(), StoreError>;

    /// Discards any staged payload for `checksum`, leaving no trace (best
    /// effort; must tolerate nothing being staged).
    fn abort_object(&mut self, checksum: &str);

    /// Commits the manifest under `id`. Called only after the `end` trailer of
    /// a fully verified stream (manifest-last survives truncation).
    fn put_manifest(&mut self, id: &str, manifest: &Manifest) -> Result<(), StoreError>;
}

/// Generic [`PackSink`] over any [`StreamStore`]: buffers one `obj` payload at
/// a time in memory, then files it via the store's verify-before-write
/// [`put_object`](StreamStore::put_object) (so the store's own integrity
/// discipline re-checks the commit). Use [`FileSink`] for `file://`-rooted
/// sinks to get O(1) memory per record.
pub struct StreamSink<'a> {
    store: &'a dyn StreamStore,
    staged: Option<(String, Vec<u8>)>,
}

impl<'a> StreamSink<'a> {
    /// Wraps `store` as a pack sink.
    #[must_use]
    pub fn new(store: &'a dyn StreamStore) -> Self {
        Self {
            store,
            staged: None,
        }
    }
}

impl PackSink for StreamSink<'_> {
    fn has_object(&mut self, checksum: &str) -> Result<bool, StoreError> {
        self.store.has_object(checksum)
    }

    fn stage_object(
        &mut self,
        checksum: &str,
        len: u64,
        payload: &mut dyn Read,
    ) -> Result<(), StoreError> {
        // Defensive: a stage while something else is staged means the reader
        // sequencing was violated; drop the stale staging rather than leak it.
        self.staged = None;
        // Preallocate at most STAGE_PRELLOC_CAP so a lying `len` cannot force a
        // huge allocation; the buffer grows with the bytes actually received
        // (which the reader caps at the true `len`).
        let prealloc = usize::try_from(len.min(STAGE_PREALLOC_CAP)).unwrap_or(0);
        let mut buf = Vec::with_capacity(prealloc);
        payload.read_to_end(&mut buf)?;
        self.staged = Some((checksum.to_owned(), buf));
        Ok(())
    }

    fn commit_object(&mut self, checksum: &str) -> Result<(), StoreError> {
        match self.staged.take() {
            Some((staged_checksum, bytes)) if staged_checksum == checksum => {
                // `put_object` re-verifies bytes-hash-to-address before writing
                // (the store's own verify-before-write discipline).
                self.store.put_object(checksum, bytes)
            }
            _ => Err(protocol(format!(
                "internal pack sink error: commit of {checksum} without a matching staged payload"
            ))),
        }
    }

    fn abort_object(&mut self, _checksum: &str) {
        self.staged = None;
    }

    fn put_manifest(&mut self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        self.store.put_manifest(id, manifest)
    }
}

/// File-backed [`PackSink`] over a [`FileStore`]: `obj` payloads stream
/// through a fixed-size buffer straight into a unique temp sibling of the
/// final object path, then an atomic rename commits on hash match — O(1)
/// memory per record regardless of object size.
///
/// This mirrors `file_store.rs`'s private `temp_sibling`/persist discipline
/// (temp file in the SAME directory so the rename is an atomic,
/// same-filesystem move; a partially-written object is never visible at its
/// content-address; a failed record removes its temp file).
pub struct FileSink<'a> {
    store: &'a FileStore,
    staged: Option<StagedFile>,
}

/// A staged-but-uncommitted object payload on disk.
struct StagedFile {
    checksum: String,
    tmp: PathBuf,
    target: PathBuf,
}

impl<'a> FileSink<'a> {
    /// Wraps `store` as a streaming, file-backed pack sink.
    #[must_use]
    pub fn new(store: &'a FileStore) -> Self {
        Self {
            store,
            staged: None,
        }
    }
}

impl Drop for FileSink<'_> {
    /// Last-resort cleanup: never leave a stray temp file in `.objects/` even
    /// if the reader bails between stage and commit/abort.
    fn drop(&mut self) {
        if let Some(staged) = self.staged.take() {
            let _ = fs::remove_file(&staged.tmp);
        }
    }
}

impl PackSink for FileSink<'_> {
    fn has_object(&mut self, checksum: &str) -> Result<bool, StoreError> {
        StreamStore::has_object(self.store, checksum)
    }

    fn stage_object(
        &mut self,
        checksum: &str,
        _len: u64,
        payload: &mut dyn Read,
    ) -> Result<(), StoreError> {
        // Defensive: drop (and remove) any stale staging first.
        self.abort_object(checksum);

        // The on-disk location derives EXCLUSIVELY from the validated claimed
        // checksum — never from any stream-supplied name.
        let target = self.store.root().join(object_path(checksum));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = temp_sibling(&target);
        let mut file = fs::File::create(&tmp)?;
        // `io::copy` streams through a fixed-size buffer (O(1) memory); the
        // reader-side incremental hasher sees every byte we pull here.
        let copied = io::copy(payload, &mut file);
        drop(file);
        if let Err(err) = copied {
            // Failed mid-write: remove the temp file, leave nothing behind.
            let _ = fs::remove_file(&tmp);
            return Err(err.into());
        }
        self.staged = Some(StagedFile {
            checksum: checksum.to_owned(),
            tmp,
            target,
        });
        Ok(())
    }

    fn commit_object(&mut self, checksum: &str) -> Result<(), StoreError> {
        match self.staged.take() {
            Some(staged) if staged.checksum == checksum => {
                // Atomic rename into the final content-addressed location; the
                // reader has already verified the streamed bytes hash to
                // `checksum`, so this is the rename-on-match step.
                fs::rename(&staged.tmp, &staged.target)?;
                Ok(())
            }
            other => {
                if let Some(staged) = other {
                    let _ = fs::remove_file(&staged.tmp);
                }
                Err(protocol(format!(
                    "internal pack sink error: commit of {checksum} without a matching staged payload"
                )))
            }
        }
    }

    fn abort_object(&mut self, _checksum: &str) {
        if let Some(staged) = self.staged.take() {
            let _ = fs::remove_file(&staged.tmp);
        }
    }

    fn put_manifest(&mut self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        // FileStore::put_manifest re-verifies snapshot_id(manifest) == id and
        // writes via its own temp+atomic-rename path.
        self.store.put_manifest(id, manifest)
    }
}

/// Emits a SNAPPACK 1 stream: magic, one `obj` record per entry of `ids` IN
/// INPUT ORDER, then (if `manifest_id` is given) the `manifest` record LAST,
/// then the `end` trailer.
///
/// Fail-closed discipline:
///
/// - Every id (and `manifest_id`) is validated against `^[0-9a-f]{64}$` BEFORE
///   any byte is written.
/// - The manifest is fetched and serialized up front (fail fast) but emitted
///   last; its serialized bytes (`Manifest` text + trailing `\n`, exactly the
///   stored byte form `file_store.rs` writes) must hash back to `manifest_id`.
/// - Each object's bytes are re-verified to hash to its id after
///   [`get_object`](StreamStore::get_object) (belt and braces over the store's
///   own read verification) before its record is written.
/// - Any failure — including a missing object — aborts BEFORE the `end`
///   trailer is emitted, so a consumer of the partial stream also fails
///   (no silent partial transfer).
///
/// Duplicates in `ids` emit duplicate records (the reader handles them
/// idempotently); deduplication is the caller's job.
pub fn write_pack(
    source: &dyn StreamStore,
    ids: &[String],
    manifest_id: Option<&str>,
    mut out: impl Write,
) -> Result<PackWriteReport, StoreError> {
    // Validate EVERY checksum before emitting anything (fail closed).
    for id in ids {
        if !is_hex64(id) {
            return Err(protocol(format!(
                "invalid object checksum {id:?}: expected 64 lowercase hex characters"
            )));
        }
    }
    if let Some(id) = manifest_id {
        if !is_hex64(id) {
            return Err(protocol(format!(
                "invalid manifest id {id:?}: expected 64 lowercase hex characters"
            )));
        }
    }

    let hasher = Blake3Hasher::new();

    // Fetch + serialize the manifest UP FRONT (fail fast before streaming
    // megabytes of objects), but emit it LAST. Serialization matches
    // `file_store.rs::write_manifest` exactly — `to_string()` + trailing `\n`,
    // the byte form `snapshot_id` hashes — so ids round-trip.
    let manifest_payload: Option<(&str, Vec<u8>)> = match manifest_id {
        Some(id) => {
            let manifest = source.get_manifest(id)?;
            let mut text = manifest.to_string();
            text.push('\n');
            let bytes = text.into_bytes();
            let actual = hasher.hash_hex(&bytes);
            if actual != id {
                return Err(StoreError::Integrity {
                    address: manifest_path(id),
                    expected: id.to_owned(),
                    actual,
                });
            }
            Some((id, bytes))
        }
        None => None,
    };

    out.write_all(WIRE_MAGIC.as_bytes())?;
    let mut report = PackWriteReport::default();

    for id in ids {
        // `get_object` already verifies the read; re-verify here anyway so a
        // non-verifying StreamStore impl can never push corrupt bytes onto the
        // wire under a clean address (fail closed).
        let bytes = source.get_object(id)?;
        let actual = hasher.hash_hex(&bytes);
        if actual != *id {
            return Err(StoreError::Integrity {
                address: object_path(id),
                expected: id.clone(),
                actual,
            });
        }
        writeln!(out, "obj {id} {}", bytes.len())?;
        out.write_all(&bytes)?;
        report.objects_written += 1;
    }

    if let Some((id, bytes)) = manifest_payload {
        writeln!(out, "manifest {id} {}", bytes.len())?;
        out.write_all(&bytes)?;
        report.manifest_written = true;
    }

    out.write_all(b"end\n")?;
    out.flush()?;
    Ok(report)
}

/// Consumes a SNAPPACK 1 stream from `input`, filing verified records into
/// `sink`. See the [module docs](crate::pack) for the full invariant list;
/// in short:
///
/// - every record header is validated (magic, version, `hex64`, decimal len,
///   [`MAX_HEADER_BYTES`] cap);
/// - every payload is incrementally BLAKE3-hashed and must match its claimed
///   checksum (mismatch ⇒ staged bytes discarded, whole stream aborted);
/// - duplicate objects are verified-but-skipped (write-once);
/// - the manifest (if any) must be the last record and is committed ONLY
///   after the `end` trailer; EOF before `end` is a hard error and never
///   commits the manifest.
pub fn read_pack(input: impl Read, sink: &mut dyn PackSink) -> Result<PackReadReport, StoreError> {
    let mut input = BufReader::new(input);

    check_magic(&read_header_line(&mut input)?)?;

    let mut report = PackReadReport::default();
    let mut pending_manifest: Option<(String, Manifest)> = None;

    loop {
        let line = read_header_line(&mut input)?;
        if line == "end" {
            // The `end` trailer is the ONLY place a manifest commits:
            // truncation anywhere above has already errored out, so a
            // committed manifest proves the whole stream verified.
            if let Some((id, manifest)) = pending_manifest.take() {
                sink.put_manifest(&id, &manifest)?;
                report.manifest_committed = true;
            }
            return Ok(report);
        }
        if pending_manifest.is_some() {
            return Err(protocol(format!(
                "record after the manifest record (only the `end` trailer may follow it): {line:?}"
            )));
        }
        let (kind, checksum, len) = parse_record_header(&line)?;
        match kind {
            RecordKind::Obj => read_obj_record(&mut input, sink, &checksum, len, &mut report)?,
            RecordKind::Manifest => {
                pending_manifest = Some(read_manifest_record(&mut input, &checksum, len)?);
            }
        }
    }
}

/// A record header's type tag.
enum RecordKind {
    Obj,
    Manifest,
}

/// Reads, verifies, and files (or verified-skips) one `obj` payload.
fn read_obj_record(
    input: &mut dyn Read,
    sink: &mut dyn PackSink,
    checksum: &str,
    len: u64,
    report: &mut PackReadReport,
) -> Result<(), StoreError> {
    let present = sink.has_object(checksum)?;
    let mut payload = HashingTake::new(input, len);

    if present {
        // Idempotent duplicate / pre-seeded object: the stream cannot seek, so
        // the bytes are still consumed AND hash-verified, but not re-written.
        payload.drain()?;
    } else if let Err(err) = sink.stage_object(checksum, len, &mut payload) {
        sink.abort_object(checksum);
        return Err(err);
    }

    if payload.remaining() > 0 {
        if !present {
            sink.abort_object(checksum);
        }
        return Err(if payload.hit_eof() {
            protocol(format!(
                "truncated pack stream: EOF inside the payload of object {checksum} \
                 ({} of {len} bytes missing)",
                payload.remaining()
            ))
        } else {
            protocol(format!(
                "internal pack sink error: sink consumed only {} of {len} payload bytes \
                 for object {checksum}",
                len - payload.remaining()
            ))
        });
    }

    // Verify the streamed bytes hash to the CLAIMED address. A mismatch files
    // nothing under the claimed checksum (the staged temp is removed) and
    // aborts the whole stream — everything after a corrupt record is tainted.
    let actual = payload.finalize_hex();
    if actual != checksum {
        if !present {
            sink.abort_object(checksum);
        }
        return Err(StoreError::Integrity {
            address: object_path(checksum),
            expected: checksum.to_owned(),
            actual,
        });
    }

    if present {
        report.objects_skipped += 1;
    } else {
        sink.commit_object(checksum)?;
        report.objects_written += 1;
    }
    Ok(())
}

/// Reads and verifies one `manifest` payload, returning it for the
/// commit-at-`end` step (it is NEVER committed here).
fn read_manifest_record(
    input: &mut dyn Read,
    id: &str,
    len: u64,
) -> Result<(String, Manifest), StoreError> {
    if len > MAX_MANIFEST_BYTES {
        return Err(protocol(format!(
            "manifest record of {len} bytes exceeds the {MAX_MANIFEST_BYTES}-byte cap"
        )));
    }
    let mut payload = HashingTake::new(input, len);
    // Bounded by the cap check above (and the prealloc guard for lying
    // headers), so buffering the manifest is safe.
    let mut buf = Vec::with_capacity(usize::try_from(len.min(STAGE_PREALLOC_CAP)).unwrap_or(0));
    payload.read_to_end(&mut buf)?;
    if payload.remaining() > 0 {
        return Err(protocol(format!(
            "truncated pack stream: EOF inside the payload of manifest {id} \
             ({} of {len} bytes missing)",
            payload.remaining()
        )));
    }

    // 1) The raw payload bytes must hash to the claimed snapshot id (the
    //    stored manifest byte form is exactly what `snapshot_id` hashes).
    let actual = payload.finalize_hex();
    if actual != id {
        return Err(StoreError::Integrity {
            address: manifest_path(id),
            expected: id.to_owned(),
            actual,
        });
    }

    // 2) The payload must PARSE, and the parsed manifest must re-render to the
    //    same snapshot id — rejecting a payload that raw-hashes correctly but
    //    is not the canonical serialization (it would not round-trip).
    let text = String::from_utf8(buf).map_err(|err| StoreError::Backend {
        message: format!("manifest {id} payload is not valid UTF-8"),
        source: Some(Box::new(err)),
    })?;
    let manifest = Manifest::parse(&text)?;
    let rendered_id = snapshot_id(&manifest, &Blake3Hasher::new());
    if rendered_id != id {
        return Err(StoreError::Integrity {
            address: manifest_path(id),
            expected: id.to_owned(),
            actual: rendered_id,
        });
    }

    Ok((id.to_owned(), manifest))
}

/// Builds a protocol-violation error (malformed/truncated stream, bad header,
/// cap exceeded, …). Hash mismatches use [`StoreError::Integrity`] instead.
fn protocol(message: impl Into<String>) -> StoreError {
    StoreError::Backend {
        message: message.into(),
        source: None,
    }
}

/// Reads one `\n`-terminated header line (returned WITHOUT the `\n`),
/// enforcing the [`MAX_HEADER_BYTES`] cap while reading — an over-long line is
/// rejected the moment the cap is hit, without buffering more. EOF at any
/// point inside a header position is a hard truncation error (`end` is the
/// only legitimate way to finish a stream).
fn read_header_line(input: &mut impl BufRead) -> Result<String, StoreError> {
    let mut line: Vec<u8> = Vec::with_capacity(32);
    loop {
        let mut byte = [0u8; 1];
        let n = input.read(&mut byte)?;
        if n == 0 {
            return Err(protocol(if line.is_empty() {
                "truncated pack stream: unexpected EOF before the `end` trailer".to_owned()
            } else {
                format!(
                    "truncated pack stream: EOF inside a header line (read {:?} so far)",
                    String::from_utf8_lossy(&line)
                )
            }));
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() >= MAX_HEADER_BYTES {
            return Err(protocol(format!(
                "header line exceeds the {MAX_HEADER_BYTES}-byte cap"
            )));
        }
    }
    String::from_utf8(line).map_err(|err| StoreError::Backend {
        message: "header line is not valid UTF-8".to_owned(),
        source: Some(Box::new(err)),
    })
}

/// Validates the magic line (already stripped of its `\n`). Negotiation is on
/// the exact `wire` integer only: a different version — newer OR older — is
/// rejected, and the caller falls back to the dumb path.
fn check_magic(line: &str) -> Result<(), StoreError> {
    let Some(version) = line.strip_prefix("SNAPPACK ") else {
        return Err(protocol(format!(
            "bad pack magic {line:?} (expected {:?})",
            WIRE_MAGIC.trim_end()
        )));
    };
    if version != WIRE_VERSION.to_string() {
        return Err(protocol(format!(
            "unsupported pack wire version {version:?}: this build speaks wire={WIRE_VERSION}"
        )));
    }
    Ok(())
}

/// Parses a record header line into `(kind, hex64, len)`, enforcing the exact
/// single-space token grammar, the `^[0-9a-f]{64}$` checksum shape, and a
/// strictly-decimal `u64` length.
fn parse_record_header(line: &str) -> Result<(RecordKind, String, u64), StoreError> {
    let mut parts = line.split(' ');
    let kind = match parts.next() {
        Some("obj") => RecordKind::Obj,
        Some("manifest") => RecordKind::Manifest,
        _ => {
            return Err(protocol(format!(
                "unknown record header {line:?} (expected `obj`, `manifest`, or `end`)"
            )));
        }
    };
    let (Some(checksum), Some(len_token), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(protocol(format!(
            "malformed record header {line:?} (expected `<kind> <hex64> <len>`)"
        )));
    };
    if !is_hex64(checksum) {
        return Err(protocol(format!(
            "invalid checksum {checksum:?} in record header: expected 64 lowercase hex characters"
        )));
    }
    if len_token.is_empty() || !len_token.bytes().all(|b| b.is_ascii_digit()) {
        return Err(protocol(format!(
            "invalid payload length {len_token:?} in record header: expected a decimal u64"
        )));
    }
    let len: u64 = len_token.parse().map_err(|_| {
        protocol(format!(
            "payload length {len_token:?} does not fit in a u64"
        ))
    })?;
    Ok((kind, checksum.to_owned(), len))
}

/// A length-limited reader that incrementally BLAKE3-hashes everything read
/// through it. This is how the reader keeps verification O(1)-memory while a
/// sink streams the payload to disk: the sink pulls bytes, the hasher sees
/// every one of them, and [`finalize_hex`](Self::finalize_hex) yields the
/// digest once the payload is exhausted.
struct HashingTake<'a> {
    inner: &'a mut dyn Read,
    remaining: u64,
    hit_eof: bool,
    hasher: blake3::Hasher,
}

impl<'a> HashingTake<'a> {
    fn new(inner: &'a mut dyn Read, len: u64) -> Self {
        Self {
            inner,
            remaining: len,
            hit_eof: false,
            hasher: blake3::Hasher::new(),
        }
    }

    /// Payload bytes not yet read. Non-zero after EOF means truncation.
    fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Whether the underlying stream hit EOF while payload bytes were still
    /// owed (distinguishes a truncated stream from a sink that under-read).
    fn hit_eof(&self) -> bool {
        self.hit_eof
    }

    /// Lowercase hex BLAKE3 digest of every byte read through this reader.
    fn finalize_hex(&self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }

    /// Reads (and hashes) the rest of the payload through a fixed-size buffer,
    /// discarding the bytes — the verified-skip path for duplicate objects.
    fn drain(&mut self) -> io::Result<()> {
        let mut buf = [0u8; 8 * 1024];
        loop {
            if self.read(&mut buf)? == 0 {
                return Ok(());
            }
        }
    }
}

impl Read for HashingTake<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let cap = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let n = self.inner.read(&mut buf[..cap])?;
        if n == 0 {
            self.hit_eof = true;
            return Ok(0);
        }
        self.hasher.update(&buf[..n]);
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Builds a unique temp sibling path for `target` (same directory, so the
/// final rename stays on one filesystem). Mirrors the private
/// `file_store.rs::temp_sibling` discipline — pid + a process-monotonic
/// counter so concurrent stages never collide.
fn temp_sibling(target: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp_name = format!("{file_name}.{pid}.{n}.tmp");
    match target.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapdir_core::manifest::{ManifestEntry, PathType};
    use snapdir_core::store::Store;
    use std::fs;

    // A tiny temp-dir helper so tests don't pull in a dev-dependency (same
    // pattern as file_store.rs tests).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "snapdir-pack-test-{}-{tag}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Deterministic multi-MB payload (exercises the streaming path).
    fn big_payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect()
    }

    /// Files `payloads` as objects in a fresh `FileStore`, returning the store
    /// (+ its tempdir guard) and the content-addresses in payload order.
    fn seed_store(tag: &str, payloads: &[Vec<u8>]) -> (TempDir, FileStore, Vec<String>) {
        let dir = TempDir::new(tag);
        let store = FileStore::from_root(dir.path());
        let hasher = Blake3Hasher::new();
        let ids = payloads
            .iter()
            .map(|p| {
                let checksum = hasher.hash_hex(p);
                store.put_object(&checksum, p.clone()).expect("seed object");
                checksum
            })
            .collect();
        (dir, store, ids)
    }

    /// Builds a manifest whose file entries reference `payloads` (real BLAKE3
    /// checksums) and returns it with its snapshot id.
    fn manifest_for(payloads: &[Vec<u8>]) -> (Manifest, String) {
        let hasher = Blake3Hasher::new();
        let mut manifest = Manifest::new();
        manifest.push(ManifestEntry::new(PathType::Directory, "700", "x", 0, "./"));
        for (i, payload) in payloads.iter().enumerate() {
            manifest.push(ManifestEntry::new(
                PathType::File,
                "600",
                hasher.hash_hex(payload),
                u64::try_from(payload.len()).unwrap(),
                format!("./obj-{i:02}"),
            ));
        }
        let manifest = Manifest::from_entries(manifest.entries().to_vec());
        let id = snapshot_id(&manifest, &hasher);
        (manifest, id)
    }

    /// The serialized (stored) byte form of a manifest: text + trailing `\n`.
    fn manifest_bytes(manifest: &Manifest) -> Vec<u8> {
        let mut text = manifest.to_string();
        text.push('\n');
        text.into_bytes()
    }

    /// Recursively collects every regular FILE under `dir` (used to prove
    /// no stray temp files survive a failed stream).
    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(files_under(&path));
            } else {
                files.push(path);
            }
        }
        files
    }

    /// Hand-builds a raw record (`<kind> <hex> <len>\n<payload>`).
    fn raw_record(kind: &str, hex: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = format!("{kind} {hex} {}\n", payload.len()).into_bytes();
        out.extend_from_slice(payload);
        out
    }

    /// Hand-builds a full stream: magic + records + `end\n`.
    fn raw_stream(records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = WIRE_MAGIC.as_bytes().to_vec();
        for record in records {
            out.extend_from_slice(record);
        }
        out.extend_from_slice(b"end\n");
        out
    }

    fn hex_of(bytes: &[u8]) -> String {
        Blake3Hasher::new().hash_hex(bytes)
    }

    // --- wire constants ----------------------------------------------------

    #[test]
    fn pack_wire_constants_are_consistent() {
        assert_eq!(WIRE_MAGIC, format!("SNAPPACK {WIRE_VERSION}\n"));
        assert_eq!(WIRE_VERSION, 1);
        assert_eq!(WIRE_CAPS, &["objects-needed", "send-pack", "receive-pack"]);
    }

    #[test]
    fn pack_is_hex64_validates_shape() {
        let valid = "0123456789abcdef".repeat(4);
        assert!(is_hex64(&valid));
        assert!(!is_hex64(&valid[..63])); // 63 chars
        assert!(!is_hex64(&format!("{valid}0"))); // 65 chars
        assert!(!is_hex64(&valid.to_uppercase())); // uppercase
        assert!(!is_hex64(&format!("g{}", &valid[1..]))); // non-hex
        assert!(!is_hex64("")); // empty
    }

    // --- roundtrips ----------------------------------------------------------

    #[test]
    fn pack_roundtrip_file_sink_streams_objects_and_manifest() {
        // Includes a 0-byte object and a multi-MB object (streaming path).
        let payloads = vec![
            Vec::new(),
            b"hello pack\n".to_vec(),
            big_payload(3 * 1024 * 1024 + 7),
        ];
        let (a_dir, a, ids) = seed_store("rt-a", &payloads);
        let (manifest, man_id) = manifest_for(&payloads);
        a.put_manifest(&man_id, &manifest).expect("seed manifest");

        let mut pack = Vec::new();
        let wrote = write_pack(&a, &ids, Some(&man_id), &mut pack).expect("write_pack");
        assert_eq!(wrote.objects_written, 3);
        assert!(wrote.manifest_written);

        let b_dir = TempDir::new("rt-b");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let read = read_pack(pack.as_slice(), &mut sink).expect("read_pack");
        assert_eq!(read.objects_written, 3);
        assert_eq!(read.objects_skipped, 0);
        assert!(read.manifest_committed);

        // B's objects are byte-equal to A's at the identical sharded keys.
        for (id, payload) in ids.iter().zip(&payloads) {
            let key = object_path(id);
            assert_eq!(
                fs::read(b_dir.path().join(&key)).expect("b object"),
                *payload,
                "object {key} byte-equal"
            );
            assert_eq!(
                fs::read(a_dir.path().join(&key)).expect("a object"),
                fs::read(b_dir.path().join(&key)).expect("b object"),
            );
        }
        // Manifest present in B and round-trips to the same id + entries.
        let back = b.get_manifest(&man_id).expect("manifest in B");
        assert_eq!(back, manifest);
        assert_eq!(snapshot_id(&back, &Blake3Hasher::new()), man_id);
        // No temp litter anywhere.
        assert!(
            !files_under(b_dir.path())
                .iter()
                .any(|p| p.to_string_lossy().ends_with(".tmp")),
            "no stray temp files after a clean stream"
        );
    }

    #[test]
    fn pack_roundtrip_stream_sink_generic() {
        let payloads = vec![b"alpha\n".to_vec(), b"beta\n".to_vec()];
        let (_a_dir, a, ids) = seed_store("ss-a", &payloads);
        let (manifest, man_id) = manifest_for(&payloads);
        a.put_manifest(&man_id, &manifest).expect("seed manifest");

        let mut pack = Vec::new();
        write_pack(&a, &ids, Some(&man_id), &mut pack).expect("write_pack");

        let b_dir = TempDir::new("ss-b");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = StreamSink::new(&b);
        let read = read_pack(pack.as_slice(), &mut sink).expect("read_pack");
        assert_eq!(read.objects_written, 2);
        assert!(read.manifest_committed);

        for (id, payload) in ids.iter().zip(&payloads) {
            assert_eq!(b.get_object(id).expect("object"), *payload);
        }
        assert_eq!(b.get_manifest(&man_id).expect("manifest"), manifest);
    }

    #[test]
    fn pack_empty_stream_roundtrips() {
        // "SNAPPACK 1\nend\n" is a valid, empty pack.
        let payloads: Vec<Vec<u8>> = Vec::new();
        let (_a_dir, a, ids) = seed_store("empty-a", &payloads);
        let mut pack = Vec::new();
        let wrote = write_pack(&a, &ids, None, &mut pack).expect("write_pack");
        assert_eq!(wrote, PackWriteReport::default());
        assert_eq!(pack, b"SNAPPACK 1\nend\n");

        let b_dir = TempDir::new("empty-b");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let read = read_pack(pack.as_slice(), &mut sink).expect("read_pack");
        assert_eq!(read, PackReadReport::default());
    }

    #[test]
    fn pack_manifest_only_stream_completes_interrupted_push() {
        // Empty object set + manifest: the manifest-only pack that finishes an
        // interrupted push whose objects all landed earlier.
        let payloads = vec![b"already there\n".to_vec()];
        let (_a_dir, a, _ids) = seed_store("mo-a", &payloads);
        let (manifest, man_id) = manifest_for(&payloads);
        a.put_manifest(&man_id, &manifest).expect("seed manifest");

        let mut pack = Vec::new();
        let wrote = write_pack(&a, &[], Some(&man_id), &mut pack).expect("write_pack");
        assert_eq!(wrote.objects_written, 0);
        assert!(wrote.manifest_written);

        let b_dir = TempDir::new("mo-b");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let read = read_pack(pack.as_slice(), &mut sink).expect("read_pack");
        assert!(read.manifest_committed);
        assert_eq!(b.get_manifest(&man_id).expect("manifest"), manifest);
    }

    // --- header edge cases ---------------------------------------------------

    #[test]
    fn pack_rejects_oversized_header_line() {
        let mut stream = WIRE_MAGIC.as_bytes().to_vec();
        stream.extend_from_slice("o".repeat(200).as_bytes());
        stream.extend_from_slice(b"\nend\n");
        let b_dir = TempDir::new("hdr-cap");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let err = read_pack(stream.as_slice(), &mut sink).expect_err("must reject");
        assert!(err.to_string().contains("128-byte cap"), "got: {err}");
    }

    #[test]
    fn pack_rejects_bad_magic_and_bad_version() {
        let b_dir = TempDir::new("magic");
        let b = FileStore::from_root(b_dir.path());
        for stream in [
            &b"SNAPHACK 1\nend\n"[..],
            &b"GARBAGE\nend\n"[..],
            &b"SNAPPACK 2\nend\n"[..],
            &b"SNAPPACK 01\nend\n"[..], // non-canonical version token
            &b"SNAPPACK \nend\n"[..],
            &b""[..], // empty input: truncated before the magic
        ] {
            let mut sink = FileSink::new(&b);
            assert!(
                read_pack(stream, &mut sink).is_err(),
                "stream {:?} must be rejected",
                String::from_utf8_lossy(stream)
            );
        }
    }

    #[test]
    fn pack_rejects_malformed_checksums_and_lengths() {
        let valid = "0123456789abcdef".repeat(4);
        let headers = [
            format!("obj {} 0", valid.to_uppercase()),   // uppercase
            format!("obj {} 0", &valid[..63]),           // 63 chars
            format!("obj {valid}0 0"),                   // 65 chars
            format!("obj g{} 0", &valid[1..]),           // non-hex
            format!("obj {valid} 12x"),                  // garbage len
            format!("obj {valid} +5"),                   // sign is not decimal
            format!("obj {valid} 99999999999999999999"), // > u64::MAX
            format!("obj {valid}"),                      // missing len
            format!("obj {valid} 0 extra"),              // trailing token
            format!("blob {valid} 0"),                   // unknown kind
            format!("obj  {valid} 0"),                   // double space
        ];
        let b_dir = TempDir::new("hdr-bad");
        let b = FileStore::from_root(b_dir.path());
        for header in headers {
            let mut stream = WIRE_MAGIC.as_bytes().to_vec();
            stream.extend_from_slice(header.as_bytes());
            stream.extend_from_slice(b"\nend\n");
            let mut sink = FileSink::new(&b);
            assert!(
                read_pack(stream.as_slice(), &mut sink).is_err(),
                "header {header:?} must be rejected"
            );
        }
    }

    // --- security: hash-mismatch fails closed --------------------------------

    #[test]
    fn pack_mismatched_object_files_nothing_and_leaves_no_temp() {
        // A record CLAIMS checksum X but its bytes hash to Y: hard error,
        // nothing filed at X's path, no manifest committed, no temp litter.
        let claimed = hex_of(b"good bytes");
        let evil = b"evil bytes"; // same length as "good bytes"
        let (manifest, man_id) = manifest_for(&[b"good bytes".to_vec()]);
        let stream = raw_stream(&[
            raw_record("obj", &claimed, evil),
            raw_record("manifest", &man_id, &manifest_bytes(&manifest)),
        ]);

        let b_dir = TempDir::new("sec");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let err = read_pack(stream.as_slice(), &mut sink).expect_err("must abort");
        match err {
            StoreError::Integrity {
                expected, actual, ..
            } => {
                assert_eq!(expected, claimed);
                assert_eq!(actual, hex_of(evil));
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
        drop(sink);

        // NOTHING filed under the claimed checksum; no manifest; no temp files
        // (or any other file) anywhere in the sink store.
        assert!(!StreamStore::has_object(&b, &claimed).unwrap());
        assert!(!b_dir.path().join(object_path(&claimed)).exists());
        assert!(matches!(
            b.get_manifest(&man_id),
            Err(StoreError::ManifestNotFound { .. })
        ));
        assert_eq!(
            files_under(b_dir.path()),
            Vec::<PathBuf>::new(),
            "no file (object, manifest, or stray temp) may survive"
        );
    }

    #[test]
    fn pack_mismatch_aborts_even_when_object_already_present() {
        // Skip-but-verify: a record for an ALREADY-PRESENT object whose stream
        // bytes are corrupt still aborts the whole stream (and everything
        // after the bad record is dropped).
        let good = b"present object\n".to_vec();
        let payloads = vec![good.clone()];
        let (b_dir, b, ids) = seed_store("dup-bad", &payloads);

        let corrupt = b"PRESENT OBJECT\n"; // same length, different bytes
        let later = b"later payload\n";
        let stream = raw_stream(&[
            raw_record("obj", &ids[0], corrupt),
            raw_record("obj", &hex_of(later), later),
        ]);

        let mut sink = FileSink::new(&b);
        let err = read_pack(stream.as_slice(), &mut sink).expect_err("must abort");
        assert!(matches!(err, StoreError::Integrity { .. }));
        drop(sink);

        // The pre-existing object is untouched; the record AFTER the corrupt
        // one was never processed.
        assert_eq!(b.get_object(&ids[0]).unwrap(), good);
        assert!(!StreamStore::has_object(&b, &hex_of(later)).unwrap());
        assert!(
            !files_under(b_dir.path())
                .iter()
                .any(|p| p.to_string_lossy().ends_with(".tmp")),
            "no stray temp files"
        );
    }

    // --- truncation ----------------------------------------------------------

    #[test]
    fn pack_truncated_before_end_files_objects_but_never_manifest() {
        // Cut the stream after all records but WITHOUT the `end` trailer: the
        // verified objects ARE filed, the manifest is NOT committed even
        // though its record was fully read before the cut.
        let payloads = vec![b"one\n".to_vec(), b"two\n".to_vec()];
        let (_a_dir, a, ids) = seed_store("trunc-a", &payloads);
        let (manifest, man_id) = manifest_for(&payloads);
        a.put_manifest(&man_id, &manifest).expect("seed manifest");

        let mut pack = Vec::new();
        write_pack(&a, &ids, Some(&man_id), &mut pack).expect("write_pack");
        assert!(pack.ends_with(b"end\n"));
        let cut = &pack[..pack.len() - b"end\n".len()];

        let b_dir = TempDir::new("trunc-b");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let err = read_pack(cut, &mut sink).expect_err("truncation is a hard error");
        assert!(err.to_string().contains("truncated"), "got: {err}");
        drop(sink);

        // The N verified objects ARE filed (a retry resumes incrementally)...
        for (id, payload) in ids.iter().zip(&payloads) {
            assert_eq!(b.get_object(id).unwrap(), *payload);
        }
        // ...but the manifest must NEVER be committed (manifest-last).
        assert!(matches!(
            b.get_manifest(&man_id),
            Err(StoreError::ManifestNotFound { .. })
        ));
        assert!(
            !files_under(b_dir.path())
                .iter()
                .any(|p| p.to_string_lossy().ends_with(".tmp")),
            "no stray temp files"
        );
    }

    #[test]
    fn pack_truncated_mid_payload_keeps_earlier_objects_drops_partial() {
        let payloads = vec![b"first object\n".to_vec(), big_payload(256 * 1024)];
        let (_a_dir, a, ids) = seed_store("midcut-a", &payloads);

        let mut pack = Vec::new();
        write_pack(&a, &ids, None, &mut pack).expect("write_pack");
        // Cut inside the SECOND object's payload.
        let cut = &pack[..pack.len() - 100_000];

        let b_dir = TempDir::new("midcut-b");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let err = read_pack(cut, &mut sink).expect_err("mid-payload truncation");
        assert!(err.to_string().contains("truncated"), "got: {err}");
        drop(sink);

        // Object 1 (fully verified before the cut) is filed; the partial
        // object 2 is NOT, and its temp file was removed.
        assert_eq!(b.get_object(&ids[0]).unwrap(), payloads[0]);
        assert!(!StreamStore::has_object(&b, &ids[1]).unwrap());
        assert!(
            !files_under(b_dir.path())
                .iter()
                .any(|p| p.to_string_lossy().ends_with(".tmp")),
            "partial payload temp must be removed"
        );
    }

    // --- duplicates ------------------------------------------------------------

    #[test]
    fn pack_duplicate_object_records_are_idempotent_write_once() {
        let payload = b"duplicated payload\n".to_vec();
        let checksum = hex_of(&payload);
        let stream = raw_stream(&[
            raw_record("obj", &checksum, &payload),
            raw_record("obj", &checksum, &payload),
        ]);

        let b_dir = TempDir::new("dup");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let read = read_pack(stream.as_slice(), &mut sink).expect("duplicates are fine");
        assert_eq!(read.objects_written, 1, "write-once");
        assert_eq!(
            read.objects_skipped, 1,
            "second record verified-but-skipped"
        );
        assert_eq!(b.get_object(&checksum).unwrap(), payload);
    }

    #[test]
    fn pack_preseeded_object_is_skipped_but_verified() {
        // The sink already holds the object: the record's bytes are consumed
        // (the stream cannot seek) and verified, but not rewritten.
        let payload = b"already in the store\n".to_vec();
        let (_b_dir, b, ids) = seed_store("preseed", std::slice::from_ref(&payload));

        let stream = raw_stream(&[raw_record("obj", &ids[0], &payload)]);
        let mut sink = FileSink::new(&b);
        let read = read_pack(stream.as_slice(), &mut sink).expect("read_pack");
        assert_eq!(read.objects_written, 0);
        assert_eq!(read.objects_skipped, 1);
        assert_eq!(b.get_object(&ids[0]).unwrap(), payload);
    }

    // --- manifest rules ----------------------------------------------------------

    #[test]
    fn pack_rejects_record_after_manifest() {
        let payload = b"object after manifest\n".to_vec();
        let (manifest, man_id) = manifest_for(std::slice::from_ref(&payload));
        let stream = raw_stream(&[
            raw_record("manifest", &man_id, &manifest_bytes(&manifest)),
            raw_record("obj", &hex_of(&payload), &payload),
        ]);

        let b_dir = TempDir::new("after-man");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let err = read_pack(stream.as_slice(), &mut sink).expect_err("must reject");
        assert!(err.to_string().contains("after the manifest"), "got: {err}");
        // The pending manifest must NOT have been committed.
        assert!(matches!(
            b.get_manifest(&man_id),
            Err(StoreError::ManifestNotFound { .. })
        ));
    }

    #[test]
    fn pack_manifest_payload_must_hash_to_claimed_id() {
        // (a) claimed id != raw hash of the payload -> Integrity.
        let (manifest, _real_id) = manifest_for(&[b"whatever\n".to_vec()]);
        let wrong_id = hex_of(b"some other bytes");
        let stream = raw_stream(&[raw_record(
            "manifest",
            &wrong_id,
            &manifest_bytes(&manifest),
        )]);
        let b_dir = TempDir::new("man-bad-id");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let err = read_pack(stream.as_slice(), &mut sink).expect_err("must reject");
        assert!(matches!(err, StoreError::Integrity { .. }));
        assert!(matches!(
            b.get_manifest(&wrong_id),
            Err(StoreError::ManifestNotFound { .. })
        ));

        // (b) raw hash matches the claimed id but the payload is not a
        // parseable manifest -> rejected, nothing committed.
        let garbage = b"not a manifest at all\n".to_vec();
        let garbage_id = hex_of(&garbage);
        let stream = raw_stream(&[raw_record("manifest", &garbage_id, &garbage)]);
        let mut sink = FileSink::new(&b);
        assert!(read_pack(stream.as_slice(), &mut sink).is_err());
        assert!(matches!(
            b.get_manifest(&garbage_id),
            Err(StoreError::ManifestNotFound { .. })
        ));
    }

    #[test]
    fn pack_rejects_manifest_over_64mib_cap() {
        let big_len: u64 = MAX_MANIFEST_BYTES + 1;
        let claimed = hex_of(b"irrelevant");
        let mut stream = WIRE_MAGIC.as_bytes().to_vec();
        stream.extend_from_slice(format!("manifest {claimed} {big_len}\n").as_bytes());
        // No payload needed: the cap check fires on the header alone.
        let b_dir = TempDir::new("man-cap");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        let err = read_pack(stream.as_slice(), &mut sink).expect_err("must reject");
        assert!(err.to_string().contains("cap"), "got: {err}");
    }

    // --- write_pack fail-closed ---------------------------------------------------

    #[test]
    fn pack_write_missing_object_aborts_before_end() {
        let payloads = vec![b"present\n".to_vec()];
        let (_a_dir, a, mut ids) = seed_store("wmiss-a", &payloads);
        // A syntactically valid but ABSENT object id.
        let absent = hex_of(b"never stored");
        ids.push(absent.clone());

        let mut pack = Vec::new();
        let err = write_pack(&a, &ids, None, &mut pack).expect_err("missing object");
        assert!(matches!(err, StoreError::ObjectNotFound { .. }));
        // The partial stream has NO `end` trailer, so a consumer fails too.
        assert!(!pack.ends_with(b"end\n"));
        let b_dir = TempDir::new("wmiss-b");
        let b = FileStore::from_root(b_dir.path());
        let mut sink = FileSink::new(&b);
        assert!(read_pack(pack.as_slice(), &mut sink).is_err());
    }

    #[test]
    fn pack_write_invalid_id_emits_nothing() {
        let (_a_dir, a, _ids) = seed_store("winv-a", &[b"x\n".to_vec()]);
        let mut pack = Vec::new();
        let err = write_pack(&a, &["NOT-HEX".to_owned()], None, &mut pack).expect_err("invalid id");
        assert!(
            err.to_string().contains("invalid object checksum"),
            "got: {err}"
        );
        assert!(pack.is_empty(), "fail closed: not a single byte written");

        // Same for an invalid manifest id.
        let mut pack = Vec::new();
        let err = write_pack(&a, &[], Some("zz"), &mut pack).expect_err("invalid manifest id");
        assert!(
            err.to_string().contains("invalid manifest id"),
            "got: {err}"
        );
        assert!(pack.is_empty());
    }

    #[test]
    fn pack_write_emits_records_in_input_order() {
        let payloads = vec![b"bbb\n".to_vec(), b"aaa\n".to_vec(), b"ccc\n".to_vec()];
        let (_a_dir, a, ids) = seed_store("order-a", &payloads);
        let mut pack = Vec::new();
        write_pack(&a, &ids, None, &mut pack).expect("write_pack");
        let text = String::from_utf8_lossy(&pack);
        let positions: Vec<usize> = ids
            .iter()
            .map(|id| text.find(id.as_str()).expect("record present"))
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "obj records keep input order");
    }

    // --- StreamStore::objects_needed -------------------------------------------

    #[test]
    fn pack_objects_needed_returns_absent_subset_in_input_order() {
        let p1 = b"seeded one\n".to_vec();
        let p3 = b"seeded three\n".to_vec();
        let (_dir, store, seeded) = seed_store("needed", &[p1, p3]);
        let absent_a = hex_of(b"absent a");
        let absent_b = hex_of(b"absent b");

        // Full ordered list interleaving present + absent.
        let list = vec![
            seeded[0].clone(),
            absent_a.clone(),
            seeded[1].clone(),
            absent_b.clone(),
        ];
        let needed = store.objects_needed(&list).expect("objects_needed");
        assert_eq!(needed, vec![absent_a.clone(), absent_b.clone()]);

        // Dedup is the caller's job: an absent checksum supplied twice is
        // reported twice, still in input order.
        let list = vec![absent_b.clone(), absent_a.clone(), absent_b.clone()];
        let needed = store.objects_needed(&list).expect("objects_needed");
        assert_eq!(needed, vec![absent_b.clone(), absent_a, absent_b]);

        // Everything present -> empty complement.
        assert_eq!(
            store.objects_needed(&seeded).expect("ok"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn pack_objects_needed_invalid_checksum_fails_closed() {
        let (_dir, store, seeded) = seed_store("needed-bad", &[b"x\n".to_vec()]);
        let valid_absent = hex_of(b"absent");
        for bad in [
            "UPPERCASE0000000000000000000000000000000000000000000000000000AA".to_owned(),
            "0123456789abcdef".repeat(4)[..63].to_owned(),
            format!("{}0", "0123456789abcdef".repeat(4)),
            "not hex at all".to_owned(),
            String::new(),
        ] {
            // The invalid entry errors the WHOLE call even when valid entries
            // precede it — nothing is returned (fail closed).
            let list = vec![seeded[0].clone(), valid_absent.clone(), bad.clone()];
            let err = store.objects_needed(&list).expect_err("must fail closed");
            assert!(
                err.to_string().contains("invalid object checksum"),
                "checksum {bad:?}: got {err}"
            );
        }
    }
}
