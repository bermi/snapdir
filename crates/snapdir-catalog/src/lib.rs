//! snapdir catalog library — redb-backed `locations`/`ancestors`/`revisions`.
//!
//! The catalog tracks *which snapshot id was last seen at which location* (a
//! local directory or a store URI) and the chain of `previous_id` links between
//! revisions. It is **private, rebuildable internal state**: there is no on-disk
//! interop with the Bash oracle and no SQLite→redb importer. The ONLY public
//! contract is the *output shape* of the three queries (locked later by the
//! `catalog-compat` gate); this crate returns typed rows whose field sets match
//! the oracle's `json_object` so compat is a thin serialization layer.
//!
//! ## Behavioral source of truth
//!
//! Reproduces the data model and query output of the original
//! `snapdir-sqlite3-catalog`. Its data model is one core table
//! `snapdir_history(location, id, previous_id, created_at)` plus an
//! `snapdir_event_log(event, id, location, created_at)`. `save(location, id)`
//! looks up the location's current head (latest `created_at`), uses it as
//! `previous_id` (NULL if none) and **skips the insert when the head already
//! equals `id`** (no-op). `log(event, id, location)` appends an event-log row
//! then calls `save`. `created_at` is formatted `YYYY-MM-DD HH:MM:SS.SSS`.
//!
//! The three queries (exact field sets from the script's SQL `json_object`):
//! - [`Catalog::locations`] → `{created_at, id, location}` — the latest record
//!   per location.
//! - [`Catalog::ancestors`] → `{created_at, id, location}` where `id` is the
//!   row's `previous_id`; rows where `id == <arg>` and `previous_id IS NOT NULL`,
//!   optionally filtered by `location`, ordered `created_at DESC`.
//! - [`Catalog::revisions`] → `{created_at, id, previous_id}` for a location,
//!   ordered `created_at DESC`.
//!
//! ## redb schema + key design (no SQL planner — fixed range scans)
//!
//! `created_at` strings are formatted `YYYY-MM-DD HH:MM:SS.SSS`, so lexical order
//! **is** chronological order; a monotonic `seq` disambiguates equal timestamps
//! and pins insertion order. Tables (all private; the on-disk schema may evolve
//! freely):
//!
//! - `records: (created_at, seq) -> (location, id, previous_id)` — the primary,
//!   insertion-ordered history (the analogue of `snapdir_history`).
//! - `loc_head: location -> (created_at, seq, id)` — the latest record per
//!   location. Maintained on every `save`; gives O(1) `previous_id` lookup and a
//!   single full-table iteration for `locations` (one row per location by
//!   construction — no self-join).
//! - `by_location: (location, created_at, seq) -> (id, previous_id_opt)` — a
//!   per-location range; `revisions` reverse-scans the `location` prefix.
//! - `by_id: (id, created_at, seq) -> (previous_id_opt, location)` — an
//!   id-keyed range; `ancestors` reverse-scans the `id` prefix and keeps rows
//!   with a non-null `previous_id` (and the optional `location` filter).
//! - `event_log: (created_at, seq) -> (event, id, location)` — the append-only
//!   event log (the analogue of `snapdir_event_log`).
//! - `meta: u8 -> u64` — the monotonic `seq` counter (single key `0`).
//!
//! `created_at DESC` is a reverse range scan. A `None`/NULL `previous_id` is
//! stored as the empty string sentinel and surfaced as `Option::None`.
//!
//! ## Library purity + time injection
//!
//! No `$HOME`/`XDG`/environment is read for behavior — the db path arrives as a
//! parameter ([`Catalog::open`]). `created_at` is injected via the [`Clock`]
//! trait so tests are deterministic; the shipped [`SystemClock`] formats the
//! wall clock as `YYYY-MM-DD HH:MM:SS.SSS`. Errors surface as a typed
//! [`thiserror`] enum.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::Serialize;
use thiserror::Error;

/// `(created_at, seq) -> (location, id, previous_id)`.
type HistVal = (String, String, String);
const RECORDS: TableDefinition<(&str, u64), (&str, &str, &str)> = TableDefinition::new("records");
/// `location -> (created_at, seq, id)`.
const LOC_HEAD: TableDefinition<&str, (&str, u64, &str)> = TableDefinition::new("loc_head");
/// `(location, created_at, seq) -> (id, previous_id)`.
const BY_LOCATION: TableDefinition<(&str, &str, u64), (&str, &str)> =
    TableDefinition::new("by_location");
/// `(id, created_at, seq) -> (previous_id, location)`.
const BY_ID: TableDefinition<(&str, &str, u64), (&str, &str)> = TableDefinition::new("by_id");
/// `(created_at, seq) -> (event, id, location)`.
const EVENT_LOG: TableDefinition<(&str, u64), (&str, &str, &str)> =
    TableDefinition::new("event_log");
/// Single-key meta table holding the monotonic `seq` counter.
const META: TableDefinition<u8, u64> = TableDefinition::new("meta");
const SEQ_KEY: u8 = 0;

/// Errors surfaced by the catalog.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// An underlying redb database error.
    #[error("catalog database error: {0}")]
    Database(#[from] redb::DatabaseError),
    /// A redb transaction error.
    #[error("catalog transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    /// A redb table error.
    #[error("catalog table error: {0}")]
    Table(#[from] redb::TableError),
    /// A redb storage error.
    #[error("catalog storage error: {0}")]
    Storage(#[from] redb::StorageError),
    /// A redb commit error.
    #[error("catalog commit error: {0}")]
    Commit(#[from] redb::CommitError),
}

/// A source of `created_at` timestamps, injectable so behavior is deterministic
/// in tests and the crate carries no hidden global clock state.
///
/// Implementations must return the oracle's millisecond-precision format
/// `YYYY-MM-DD HH:MM:SS.SSS`.
pub trait Clock {
    /// Returns the current timestamp formatted `YYYY-MM-DD HH:MM:SS.SSS`.
    fn now(&self) -> String;
}

/// The shipped production clock: formats the system wall clock (UTC) as
/// `YYYY-MM-DD HH:MM:SS.SSS`, matching the oracle's
/// `STRFTIME('%Y-%m-%d %H:%M:%f', 'NOW')`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> String {
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        format_millis(dur.as_secs(), dur.subsec_millis())
    }
}

/// A clock that returns a fixed list of timestamps in order (deterministic
/// tests). After the list is exhausted it repeats the last value.
#[derive(Debug, Clone)]
pub struct FixedClock {
    stamps: Vec<String>,
    idx: std::cell::Cell<usize>,
}

impl FixedClock {
    /// Builds a clock yielding `stamps[0]`, `stamps[1]`, … on successive `now()`
    /// calls (the last value repeats once exhausted).
    #[must_use]
    pub fn new(stamps: Vec<String>) -> Self {
        Self {
            stamps,
            idx: std::cell::Cell::new(0),
        }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> String {
        let i = self.idx.get();
        let s = self
            .stamps
            .get(i)
            .or_else(|| self.stamps.last())
            .cloned()
            .unwrap_or_default();
        if i + 1 < self.stamps.len() {
            self.idx.set(i + 1);
        }
        s
    }
}

/// Formats `YYYY-MM-DD HH:MM:SS.SSS` from a Unix-second count + millisecond
/// remainder, using a civil-date (Howard Hinnant) conversion. UTC.
fn format_millis(secs: u64, millis: u32) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let rem = secs % 86_400;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;

    // days since 1970-01-01 -> civil (y, m, d), Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}:{second:02}.{millis:03}")
}

/// One history record. Field sets are chosen so the `catalog-compat` gate can
/// serialize directly; queries return whichever subset the oracle SQL emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// `YYYY-MM-DD HH:MM:SS.SSS`.
    pub created_at: String,
    /// The snapshot id (or, for [`Catalog::ancestors`], the row's `previous_id`).
    pub id: String,
    /// The location (absolute dir or store URI).
    pub location: String,
    /// The previous snapshot id for this location (`None` for the first / a
    /// root id). Only populated where the oracle SQL emits it.
    pub previous_id: Option<String>,
}

/// CLI-compat JSON line for the `locations` query:
/// `{"created_at":"…","id":"…","location":"…"}`.
///
/// Field declaration order **is** the JSON key order; serde keeps it. This is a
/// frozen CLI contract (matches the oracle's sqlite `json_object('created_at',
/// …, 'id', …, 'location', …)`), so do not reorder/rename without a proposal.
#[derive(Debug, Serialize)]
struct LocationsLine<'a> {
    created_at: &'a str,
    id: &'a str,
    location: &'a str,
}

/// CLI-compat JSON line for the `ancestors` query:
/// `{"created_at":"…","id":"…","location":"…"}` where `id` is the row's
/// `previous_id` (already projected into [`Record::id`] by
/// [`Catalog::ancestors`]). Same shape as `locations` but a distinct type so the
/// two contracts stay independent.
#[derive(Debug, Serialize)]
struct AncestorsLine<'a> {
    created_at: &'a str,
    id: &'a str,
    location: &'a str,
}

/// CLI-compat JSON line for the `revisions` query:
/// `{"created_at":"…","id":"…","previous_id":…}` — **no** `location`. A NULL
/// `previous_id` (the root revision) renders as JSON `null`, byte-identical to
/// sqlite's `json_object('previous_id', NULL)`.
#[derive(Debug, Serialize)]
struct RevisionsLine<'a> {
    created_at: &'a str,
    id: &'a str,
    previous_id: Option<&'a str>,
}

/// Renders a [`Record`] from [`Catalog::locations`] to its compact CLI-compat
/// JSON line (no trailing newline): `{"created_at":"…","id":"…","location":"…"}`.
///
/// `serde_json`'s `to_string` is compact (no spaces after `:`/`,`); struct field
/// order fixes the key order. Standard JSON escaping is applied to the location.
#[must_use]
pub fn locations_json_line(record: &Record) -> String {
    let line = LocationsLine {
        created_at: &record.created_at,
        id: &record.id,
        location: &record.location,
    };
    // Infallible: all fields are plain strings; serde_json never fails here.
    serde_json::to_string(&line).expect("serialize locations line")
}

/// Renders a [`Record`] from [`Catalog::ancestors`] to its compact CLI-compat
/// JSON line: `{"created_at":"…","id":"…","location":"…"}` (`id` already holds
/// the row's `previous_id`).
#[must_use]
pub fn ancestors_json_line(record: &Record) -> String {
    let line = AncestorsLine {
        created_at: &record.created_at,
        id: &record.id,
        location: &record.location,
    };
    serde_json::to_string(&line).expect("serialize ancestors line")
}

/// Renders a [`Record`] from [`Catalog::revisions`] to its compact CLI-compat
/// JSON line: `{"created_at":"…","id":"…","previous_id":…}`. A `None`
/// `previous_id` renders as `null` (the root revision), matching sqlite.
#[must_use]
pub fn revisions_json_line(record: &Record) -> String {
    let line = RevisionsLine {
        created_at: &record.created_at,
        id: &record.id,
        previous_id: record.previous_id.as_deref(),
    };
    serde_json::to_string(&line).expect("serialize revisions line")
}

/// One recovered store entry for [`Catalog::rebuild`]: the snapshot `id` present
/// in a store and its `created_at` (already formatted `YYYY-MM-DD HH:MM:SS.SSS`,
/// derived from the store object's metadata — file mtime / S3·GCS `LastModified`).
///
/// ## Recoverability boundary
///
/// A bare store (`file://`, `s3://`, `gs://`) holds only `.manifests/<id>`
/// objects, so the only facts recoverable per location are the **set of snapshot
/// ids** present and a per-manifest **`created_at`** from object metadata. The
/// store does **not** record `previous_id`. It is, however, *re-derivable*: the
/// per-location history is a linear chain, so ordering the ids by `created_at`
/// reproduces exactly the chain [`Catalog::save`] would have built when fed the
/// same ids in chronological order. [`Catalog::rebuild`] therefore reconstructs
/// `previous_id` (and the head==id dedup) from `created_at` order alone — no
/// `previous_id` field is needed or accepted here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildEntry {
    /// The snapshot id recovered from the store's `.manifests/<id>` object.
    pub id: String,
    /// `YYYY-MM-DD HH:MM:SS.SSS`, from the store object's metadata.
    pub created_at: String,
}

/// A redb-backed snapdir catalog (single writer, multiple readers).
#[derive(Debug)]
pub struct Catalog {
    db: Database,
}

/// Empty-string sentinel for a NULL `previous_id` on disk.
const NULL_PREV: &str = "";

fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_owned())
    }
}

impl Catalog {
    /// Opens (creating if absent) the redb catalog at `path`. The path is a
    /// parameter — no environment is consulted.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CatalogError> {
        let db = Database::create(path)?;
        // Ensure tables exist so read-only queries on a fresh db don't fail.
        let txn = db.begin_write()?;
        {
            let _ = txn.open_table(RECORDS)?;
            let _ = txn.open_table(LOC_HEAD)?;
            let _ = txn.open_table(BY_LOCATION)?;
            let _ = txn.open_table(BY_ID)?;
            let _ = txn.open_table(EVENT_LOG)?;
            let _ = txn.open_table(META)?;
        }
        txn.commit()?;
        Ok(Self { db })
    }

    /// The current head id for `location`, or `None` if untracked.
    fn head_id(&self, location: &str) -> Result<Option<String>, CatalogError> {
        let txn = self.db.begin_read()?;
        let loc_head = txn.open_table(LOC_HEAD)?;
        Ok(loc_head
            .get(location)?
            .map(|v| v.value().2.to_owned())
            .filter(|s| !s.is_empty()))
    }

    /// Saves a history entry for `location`/`id`, setting `previous_id` to the
    /// location's current head (NULL for the first). Mirrors the oracle's
    /// `save`: **skips the insert when the head already equals `id`** (no-op).
    /// `created_at` is taken from `clock`.
    pub fn save(&self, location: &str, id: &str, clock: &impl Clock) -> Result<(), CatalogError> {
        let previous_id = self.head_id(location)?;
        // Oracle no-op: the location's head is already this id.
        if previous_id.as_deref() == Some(id) {
            return Ok(());
        }
        let created_at = clock.now();
        self.insert_history(location, id, previous_id.as_deref(), &created_at)
    }

    /// Appends an event-log row then calls [`Catalog::save`] (mirrors the
    /// oracle's `log`). Uses a single `created_at` from `clock` for both rows.
    pub fn log(
        &self,
        event: &str,
        id: &str,
        location: &str,
        clock: &impl Clock,
    ) -> Result<(), CatalogError> {
        let created_at = clock.now();
        let seq = self.next_seq()?;
        {
            let txn = self.db.begin_write()?;
            {
                let mut log = txn.open_table(EVENT_LOG)?;
                log.insert((created_at.as_str(), seq), (event, id, location))?;
            }
            txn.commit()?;
        }
        // save() re-reads the head and applies the skip-if-equal no-op, exactly
        // like the oracle (which calls save after the event-log insert).
        let previous_id = self.head_id(location)?;
        if previous_id.as_deref() == Some(id) {
            return Ok(());
        }
        self.insert_history(location, id, previous_id.as_deref(), &created_at)
    }

    /// Reserves and returns the next monotonic sequence number.
    fn next_seq(&self) -> Result<u64, CatalogError> {
        let txn = self.db.begin_write()?;
        let next;
        {
            let mut meta = txn.open_table(META)?;
            let cur = meta.get(SEQ_KEY)?.map_or(0, |v| v.value());
            next = cur;
            meta.insert(SEQ_KEY, cur + 1)?;
        }
        txn.commit()?;
        Ok(next)
    }

    /// Writes one history row across the primary + index tables in a single
    /// transaction.
    fn insert_history(
        &self,
        location: &str,
        id: &str,
        previous_id: Option<&str>,
        created_at: &str,
    ) -> Result<(), CatalogError> {
        let seq = self.next_seq()?;
        let prev = previous_id.unwrap_or(NULL_PREV);
        let txn = self.db.begin_write()?;
        {
            let mut records = txn.open_table(RECORDS)?;
            records.insert((created_at, seq), (location, id, prev))?;

            let mut loc_head = txn.open_table(LOC_HEAD)?;
            loc_head.insert(location, (created_at, seq, id))?;

            let mut by_location = txn.open_table(BY_LOCATION)?;
            by_location.insert((location, created_at, seq), (id, prev))?;

            let mut by_id = txn.open_table(BY_ID)?;
            by_id.insert((id, created_at, seq), (prev, location))?;
        }
        txn.commit()?;
        Ok(())
    }

    /// The latest record per location (oracle `locations`):
    /// `{created_at, id, location}`. `previous_id` is left `None`.
    ///
    /// `loc_head` already holds exactly one row per location (the latest), so
    /// this is a single table iteration — no self-join.
    pub fn locations(&self) -> Result<Vec<Record>, CatalogError> {
        let txn = self.db.begin_read()?;
        let loc_head = txn.open_table(LOC_HEAD)?;
        let mut out = Vec::new();
        for entry in loc_head.iter()? {
            let (k, v) = entry?;
            let (created_at, _seq, id) = v.value();
            out.push(Record {
                created_at: created_at.to_owned(),
                id: id.to_owned(),
                location: k.value().to_owned(),
                previous_id: None,
            });
        }
        Ok(out)
    }

    /// Ancestors of `id` (oracle `ancestors`): the rows whose `id` column equals
    /// `id` and whose `previous_id` is non-null, optionally filtered by
    /// `location`, ordered `created_at DESC`. Each returned [`Record`] reports
    /// the row's **`previous_id`** in its `id` field (matching the oracle's
    /// `'id', previous_id` projection).
    pub fn ancestors(&self, id: &str, location: Option<&str>) -> Result<Vec<Record>, CatalogError> {
        let txn = self.db.begin_read()?;
        let by_id = txn.open_table(BY_ID)?;
        // Range over the `id` prefix: (id, "", 0) ..= (id, "\u{10FFFF}", u64::MAX).
        let lo = (id, "", 0u64);
        let hi = (id, "\u{10FFFF}", u64::MAX);
        let mut out = Vec::new();
        // Reverse for created_at DESC (seq is the secondary, also descending —
        // consistent with the oracle's insertion-order tiebreak under DESC).
        for entry in by_id.range(lo..=hi)?.rev() {
            let (k, v) = entry?;
            let (_id, created_at, _seq) = k.value();
            let (prev, loc) = v.value();
            if prev.is_empty() {
                continue; // previous_id IS NOT NULL
            }
            if let Some(want) = location {
                if loc != want {
                    continue;
                }
            }
            out.push(Record {
                created_at: created_at.to_owned(),
                id: prev.to_owned(), // 'id', previous_id
                location: loc.to_owned(),
                previous_id: None,
            });
        }
        Ok(out)
    }

    /// Revisions at `location` (oracle `revisions`):
    /// `{created_at, id, previous_id}`, ordered `created_at DESC`.
    pub fn revisions(&self, location: &str) -> Result<Vec<Record>, CatalogError> {
        let txn = self.db.begin_read()?;
        let by_location = txn.open_table(BY_LOCATION)?;
        let lo = (location, "", 0u64);
        let hi = (location, "\u{10FFFF}", u64::MAX);
        let mut out = Vec::new();
        for entry in by_location.range(lo..=hi)?.rev() {
            let (k, v) = entry?;
            let (_loc, created_at, _seq) = k.value();
            let (id, prev) = v.value();
            out.push(Record {
                created_at: created_at.to_owned(),
                id: id.to_owned(),
                location: location.to_owned(),
                previous_id: opt(prev),
            });
        }
        Ok(out)
    }

    /// Returns every history record in insertion order (primary table scan).
    /// Plumbing for the later `catalog-rebuild` gate; populates all fields.
    pub fn all_records(&self) -> Result<Vec<Record>, CatalogError> {
        let txn = self.db.begin_read()?;
        let records = txn.open_table(RECORDS)?;
        let mut out = Vec::new();
        for entry in records.iter()? {
            let (key, val) = entry?;
            let (created_at, _seq) = key.value();
            let (location, id, prev): HistVal = {
                let (loc, sid, prev) = val.value();
                (loc.to_owned(), sid.to_owned(), prev.to_owned())
            };
            out.push(Record {
                created_at: created_at.to_owned(),
                id,
                location,
                previous_id: opt(&prev),
            });
        }
        Ok(out)
    }

    /// Rebuilds the catalog for a single `location` from what a **store**
    /// actually preserves, reproducing identical query output for that location.
    ///
    /// This is a convenience, **not** a migration: the catalog is private,
    /// rebuildable state with no on-disk interop. A store yields only the set of
    /// `(id, created_at)` pairs per location (see [`RebuildEntry`]); this method
    /// reconstructs the rest:
    ///
    /// 1. Remove **all** existing records for `location` (across every redb
    ///    table) — stale ids not in the store view are dropped. Other locations
    ///    are untouched.
    /// 2. Insert the entries **sorted by `created_at`** (with a stable `id`
    ///    tiebreak for equal timestamps), reconstructing `previous_id` from the
    ///    running per-location head and applying the same **head==id dedup** as
    ///    [`Catalog::save`]. `created_at` comes verbatim from the store, so this
    ///    path is **clock-free** — it never stamps NOW.
    ///
    /// Because `save`'s insert logic is replayed in chronological order, the
    /// resulting `previous_id` chain and `created_at DESC` ordering are identical
    /// to a catalog built by `save`-ing the same history live. It is **store-
    /// agnostic** (no dependency on `snapdir-stores`): the caller enumerates the
    /// store's `.manifests` and passes the recovered entries in. Rebuilding the
    /// same store view twice is idempotent.
    pub fn rebuild(
        &self,
        location: &str,
        entries: impl IntoIterator<Item = RebuildEntry>,
    ) -> Result<(), CatalogError> {
        // Sort by created_at, then a stable id tiebreak for equal timestamps —
        // this is the chronological replay order `save` would have seen.
        let mut entries: Vec<RebuildEntry> = entries.into_iter().collect();
        entries.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        // 1) Drop every existing record for this location (all tables) so a
        // rebuild replaces a stale history and never leaks ids absent from the
        // store view. Other locations are left intact.
        self.clear_location(location)?;

        // 2) Replay the sorted entries through the same insert logic `save`
        // uses: previous_id = running head, with the head==id no-op dedup.
        // Clock-free: created_at is taken from the store entry, not stamped.
        let mut head: Option<String> = None;
        for entry in entries {
            // Oracle no-op: the location's current head is already this id.
            if head.as_deref() == Some(entry.id.as_str()) {
                continue;
            }
            self.insert_history(location, &entry.id, head.as_deref(), &entry.created_at)?;
            head = Some(entry.id);
        }
        Ok(())
    }

    /// Removes every record for `location` from all redb tables (`records`,
    /// `loc_head`, `by_location`, `by_id`), leaving other locations untouched. The
    /// monotonic `seq` counter is not rewound (seq only disambiguates equal
    /// timestamps and pins insertion order; gaps are harmless).
    fn clear_location(&self, location: &str) -> Result<(), CatalogError> {
        // Collect the location's rows first (read), then delete (write) so we
        // don't mutate a table while iterating it.
        let mut victims: Vec<(String, u64, String)> = Vec::new(); // (created_at, seq, id)
        {
            let txn = self.db.begin_read()?;
            let by_location = txn.open_table(BY_LOCATION)?;
            let lo = (location, "", 0u64);
            let hi = (location, "\u{10FFFF}", u64::MAX);
            for entry in by_location.range(lo..=hi)? {
                let (k, v) = entry?;
                let (_loc, created_at, seq) = k.value();
                let (id, _prev) = v.value();
                victims.push((created_at.to_owned(), seq, id.to_owned()));
            }
        }
        if victims.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut records = txn.open_table(RECORDS)?;
            let mut loc_head = txn.open_table(LOC_HEAD)?;
            let mut by_location = txn.open_table(BY_LOCATION)?;
            let mut by_id = txn.open_table(BY_ID)?;
            for (created_at, seq, id) in &victims {
                records.remove((created_at.as_str(), *seq))?;
                by_location.remove((location, created_at.as_str(), *seq))?;
                by_id.remove((id.as_str(), created_at.as_str(), *seq))?;
            }
            loc_head.remove(location)?;
        }
        txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Tiny temp-dir helper so tests don't pull a `tempfile` dev-dependency
    /// (matching the convention in the other crates).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("snapdir-catalog-test-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn db_path(&self) -> PathBuf {
            self.path.join("catalog.redb")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    // 64-char ids (the oracle CHECKs length == 64; we don't enforce it but use
    // realistic ids).
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// A clock yielding strictly increasing timestamps so ordering is
    /// deterministic.
    fn seq_clock(stamps: &[&str]) -> FixedClock {
        FixedClock::new(stamps.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn empty_catalog_returns_empty() {
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        assert!(cat.locations().unwrap().is_empty());
        assert!(cat.ancestors(A, None).unwrap().is_empty());
        assert!(cat.revisions("/local/foo").unwrap().is_empty());
    }

    #[test]
    fn save_sets_previous_id_to_prior_head_null_for_first() {
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        let clock = seq_clock(&[
            "2026-06-01 00:00:00.001",
            "2026-06-01 00:00:00.002",
            "2026-06-01 00:00:00.003",
        ]);
        cat.save("/local/foo", A, &clock).unwrap();
        cat.save("/local/foo", B, &clock).unwrap();
        cat.save("/local/foo", C, &clock).unwrap();

        // revisions are created_at DESC: C (prev B), B (prev A), A (prev NULL).
        let revs = cat.revisions("/local/foo").unwrap();
        assert_eq!(revs.len(), 3);
        assert_eq!(revs[0].id, C);
        assert_eq!(revs[0].previous_id.as_deref(), Some(B));
        assert_eq!(revs[1].id, B);
        assert_eq!(revs[1].previous_id.as_deref(), Some(A));
        assert_eq!(revs[2].id, A);
        assert_eq!(revs[2].previous_id, None);
    }

    #[test]
    fn save_skips_when_head_equals_id() {
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        let clock = seq_clock(&[
            "2026-06-01 00:00:00.001",
            "2026-06-01 00:00:00.002",
            "2026-06-01 00:00:00.003",
        ]);
        cat.save("/local/foo", A, &clock).unwrap();
        // Re-saving the same head id is a no-op (no new row).
        cat.save("/local/foo", A, &clock).unwrap();
        cat.save("/local/foo", A, &clock).unwrap();
        let revs = cat.revisions("/local/foo").unwrap();
        assert_eq!(revs.len(), 1);
        assert_eq!(revs[0].id, A);
        assert_eq!(revs[0].previous_id, None);

        // But saving a different id after the no-ops still links to A.
        cat.save("/local/foo", B, &clock).unwrap();
        let revs = cat.revisions("/local/foo").unwrap();
        assert_eq!(revs.len(), 2);
        assert_eq!(revs[0].id, B);
        assert_eq!(revs[0].previous_id.as_deref(), Some(A));
    }

    #[test]
    fn locations_returns_latest_per_location() {
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        // Mirror the oracle test fixture ordering.
        let clock = seq_clock(&[
            "2026-06-01 00:00:00.001", // s3://foo  a
            "2026-06-01 00:00:00.002", // s3://bar  a
            "2026-06-01 00:00:00.003", // /local/foo a
            "2026-06-01 00:00:00.004", // /local/foo b
            "2026-06-01 00:00:00.005", // /local/foo c
            "2026-06-01 00:00:00.006", // s3://bar  c
        ]);
        cat.save("s3://foo", A, &clock).unwrap();
        cat.save("s3://bar", A, &clock).unwrap();
        cat.save("/local/foo", A, &clock).unwrap();
        cat.save("/local/foo", B, &clock).unwrap();
        cat.save("/local/foo", C, &clock).unwrap();
        cat.save("s3://bar", C, &clock).unwrap();

        let mut locs = cat.locations().unwrap();
        locs.sort_by(|a, b| a.location.cmp(&b.location));
        assert_eq!(locs.len(), 3);
        // latest id per location: s3://foo -> a, s3://bar -> c, /local/foo -> c
        let by_loc = |l: &str| locs.iter().find(|r| r.location == l).unwrap().id.clone();
        assert_eq!(by_loc("s3://foo"), A);
        assert_eq!(by_loc("s3://bar"), C);
        assert_eq!(by_loc("/local/foo"), C);
        // previous_id is not part of the locations projection.
        assert!(locs.iter().all(|r| r.previous_id.is_none()));
    }

    #[test]
    fn ancestors_walks_previous_id_rows_desc_with_location_filter() {
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        let clock = seq_clock(&[
            "2026-06-01 00:00:00.001", // s3://bar  a (prev NULL)
            "2026-06-01 00:00:00.002", // /local/foo a (prev NULL)
            "2026-06-01 00:00:00.003", // /local/foo b (prev a)
            "2026-06-01 00:00:00.004", // /local/foo c (prev b)
            "2026-06-01 00:00:00.005", // s3://bar  c (prev a)
        ]);
        cat.save("s3://bar", A, &clock).unwrap();
        cat.save("/local/foo", A, &clock).unwrap();
        cat.save("/local/foo", B, &clock).unwrap();
        cat.save("/local/foo", C, &clock).unwrap();
        cat.save("s3://bar", C, &clock).unwrap();

        // ancestors of a root id -> empty (no row has id=A with non-null prev).
        assert!(cat.ancestors(A, None).unwrap().is_empty());

        // ancestors of C: rows where id=C and previous_id non-null:
        //   s3://bar   C (prev A) @ .005
        //   /local/foo C (prev B) @ .004
        // DESC -> s3://bar first, then /local/foo. id field = previous_id.
        let anc = cat.ancestors(C, None).unwrap();
        assert_eq!(anc.len(), 2);
        assert_eq!(anc[0].location, "s3://bar");
        assert_eq!(anc[0].id, A); // previous_id
        assert_eq!(anc[1].location, "/local/foo");
        assert_eq!(anc[1].id, B); // previous_id

        // with the location filter -> only the s3://bar ancestor.
        let anc = cat.ancestors(C, Some("s3://bar")).unwrap();
        assert_eq!(anc.len(), 1);
        assert_eq!(anc[0].location, "s3://bar");
        assert_eq!(anc[0].id, A);
    }

    #[test]
    fn revisions_lists_location_rows_desc_with_previous_id() {
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        let clock = seq_clock(&[
            "2026-06-01 00:00:00.001", // s3://bar a (prev NULL)
            "2026-06-01 00:00:00.002", // s3://bar c (prev a)
        ]);
        cat.save("s3://bar", A, &clock).unwrap();
        cat.save("s3://bar", C, &clock).unwrap();

        let revs = cat.revisions("s3://bar").unwrap();
        assert_eq!(revs.len(), 2);
        // DESC: c (prev a), then a (prev null) — mirrors the oracle revisions test.
        assert_eq!(revs[0].id, C);
        assert_eq!(revs[0].previous_id.as_deref(), Some(A));
        assert_eq!(revs[1].id, A);
        assert_eq!(revs[1].previous_id, None);

        // An untracked location yields nothing.
        assert!(cat.revisions("/not/avail").unwrap().is_empty());
    }

    #[test]
    fn log_writes_event_then_saves_history() {
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        let clock = seq_clock(&["2026-06-01 00:00:00.001", "2026-06-01 00:00:00.002"]);
        cat.log("manifest", A, "s3://foo", &clock).unwrap();
        cat.log("push", B, "s3://foo", &clock).unwrap();
        let revs = cat.revisions("s3://foo").unwrap();
        assert_eq!(revs.len(), 2);
        assert_eq!(revs[0].id, B);
        assert_eq!(revs[0].previous_id.as_deref(), Some(A));
    }

    // ----- json_compat: CLI-compat JSON-line serialization -----------------
    //
    // These tests freeze the three query output shapes against the original
    // sqlite `json_object` output. The literal-string assertions verify
    // compactness (no spaces) and exact key order WITHOUT re-parsing (a re-parse
    // would hide a formatting regression). The `_golden` test below cross-checked
    // byte-for-byte against the original `snapdir-sqlite3-catalog`; since that
    // script was removed it self-skips when the script is absent.

    fn rec(created_at: &str, id: &str, location: &str, previous_id: Option<&str>) -> Record {
        Record {
            created_at: created_at.to_owned(),
            id: id.to_owned(),
            location: location.to_owned(),
            previous_id: previous_id.map(str::to_owned),
        }
    }

    #[test]
    fn json_compat_locations_line_exact_bytes() {
        let r = rec("2026-06-01 00:00:00.001", A, "s3://bucket/some/path", None);
        assert_eq!(
            locations_json_line(&r),
            format!(
                r#"{{"created_at":"2026-06-01 00:00:00.001","id":"{A}","location":"s3://bucket/some/path"}}"#
            )
        );
    }

    #[test]
    fn json_compat_ancestors_line_id_is_previous_id_with_location() {
        // ancestors projects the row's previous_id into Record::id; location is
        // present and DESC ordering is the caller's (Catalog::ancestors). Here we
        // assert the serialized bytes for two rows in DESC order.
        let rows = [
            rec("2026-06-01 00:00:00.005", A, "s3://bar", None), // id = previous_id (A)
            rec("2026-06-01 00:00:00.004", B, "/local/foo", None),
        ];
        let lines: Vec<String> = rows.iter().map(ancestors_json_line).collect();
        assert_eq!(
            lines[0],
            format!(
                r#"{{"created_at":"2026-06-01 00:00:00.005","id":"{A}","location":"s3://bar"}}"#
            )
        );
        assert_eq!(
            lines[1],
            format!(
                r#"{{"created_at":"2026-06-01 00:00:00.004","id":"{B}","location":"/local/foo"}}"#
            )
        );
    }

    #[test]
    fn json_compat_revisions_lines_desc_incl_null_root() {
        // created_at DESC: C (prev B), B (prev A), A (prev NULL -> json null).
        let rows = [
            rec("2026-06-01 00:00:00.003", C, "s3://bar", Some(B)),
            rec("2026-06-01 00:00:00.002", B, "s3://bar", Some(A)),
            rec("2026-06-01 00:00:00.001", A, "s3://bar", None),
        ];
        let lines: Vec<String> = rows.iter().map(revisions_json_line).collect();
        // No `location` key in revisions.
        assert_eq!(
            lines[0],
            format!(r#"{{"created_at":"2026-06-01 00:00:00.003","id":"{C}","previous_id":"{B}"}}"#)
        );
        assert_eq!(
            lines[1],
            format!(r#"{{"created_at":"2026-06-01 00:00:00.002","id":"{B}","previous_id":"{A}"}}"#)
        );
        // The root revision renders previous_id as literal `null` (not "null",
        // not omitted) — byte-identical to sqlite json_object('previous_id',NULL).
        assert_eq!(
            lines[2],
            format!(r#"{{"created_at":"2026-06-01 00:00:00.001","id":"{A}","previous_id":null}}"#)
        );
        assert!(lines[2].contains(r#""previous_id":null"#));
        assert!(!lines[2].contains(r#""previous_id":"null""#));
    }

    #[test]
    fn json_compat_is_compact_no_spaces_and_key_order() {
        // Assert on the literal string: no `": "` or `, ` anywhere, and keys in
        // declaration order. (Done on the raw bytes, not via re-parsing.)
        let loc = locations_json_line(&rec("2026-06-01 00:00:00.001", A, "/p", None));
        let rev = revisions_json_line(&rec("2026-06-01 00:00:00.001", A, "/p", None));
        for line in [&loc, &rev] {
            assert!(
                !line.contains(": "),
                "json not compact (space after colon): {line}"
            );
            assert!(
                !line.contains(", "),
                "json not compact (space after comma): {line}"
            );
            assert!(line.starts_with('{') && line.ends_with('}'));
            assert!(!line.contains('\n'), "no trailing/embedded newline: {line}");
        }
        // Key order: created_at before id before location/previous_id.
        let ca = loc.find("created_at").unwrap();
        let id = loc.find(r#""id""#).unwrap();
        let lock = loc.find("location").unwrap();
        assert!(ca < id && id < lock, "locations key order wrong: {loc}");
        let rca = rev.find("created_at").unwrap();
        let rid = rev.find(r#""id""#).unwrap();
        let rprev = rev.find("previous_id").unwrap();
        assert!(rca < rid && rid < rprev, "revisions key order wrong: {rev}");
    }

    #[test]
    fn json_compat_escapes_match_standard_json() {
        // Ordinary URIs/paths need no escaping; a quote/backslash uses standard
        // JSON escaping (\" and \\), which serde_json produces — the same bytes
        // sqlite json_object emits. We don't hand-roll escaping.
        let r = rec("2026-06-01 00:00:00.001", A, r#"a/b "q" \back"#, None);
        let line = locations_json_line(&r);
        assert!(
            line.contains(r#""location":"a/b \"q\" \\back""#),
            "got: {line}"
        );
    }

    /// Golden cross-check against the original `snapdir-sqlite3-catalog` script
    /// (read-only: only `save`, then the three queries) over a throwaway sqlite
    /// db to produce its JSON, then asserts the Rust serializers produce
    /// byte-identical lines for the same logical rows. The script has been
    /// removed from the branch, so this test self-skips when it is not present.
    ///
    /// Neutralizing `NOW()`: the oracle stamps `created_at` from the wall clock
    /// at insert time, so we cannot predict it. Instead we read the oracle's
    /// emitted `created_at` back out of each oracle line and feed THAT exact
    /// string into the Rust `Record` before serializing — making the comparison a
    /// real byte-for-byte check of key set, key order, compactness, and null
    /// rendering, with the only uncontrollable field (the timestamp) sourced from
    /// the oracle itself. Guarded behind `which sqlite3`; skip (not fail) if
    /// absent. sqlite3 is used ONLY here in the test, never in the shipped crate.
    #[test]
    fn json_compat_matches_live_sqlite3_oracle_golden() {
        use std::process::Command;

        // Locate the original catalog script relative to the workspace root
        // (removed from the branch; the test self-skips below if absent).
        let oracle = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("snapdir-sqlite3-catalog");
        if Command::new("sh")
            .args(["-c", "command -v sqlite3"])
            .output()
            .map_or(true, |o| !o.status.success())
        {
            eprintln!("skipping golden: sqlite3 not on PATH");
            return;
        }
        if !oracle.exists() {
            eprintln!("skipping golden: oracle script not found at {oracle:?}");
            return;
        }

        let dir = TempDir::new();
        let db = dir.path.join("oracle.sqlite3.db");

        // Helper: run the oracle with a fixed db path.
        let run = |args: &[&str]| -> String {
            let out = Command::new("bash")
                .arg(&oracle)
                .args(args)
                .env("SNAPDIR_SQLITE3_BIN", "sqlite3")
                .env("SNAPDIR_SQLITE3_CATALOG_DB_PATH", &db)
                .output()
                .expect("run oracle");
            assert!(
                out.status.success(),
                "oracle {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).expect("utf8 oracle output")
        };

        // Scenario: one location, two revisions (root A, then C with prev A).
        let location = "s3://bucket/some/path";
        run(&[
            "save",
            &format!("--id={A}"),
            &format!("--location={location}"),
        ]);
        run(&[
            "save",
            &format!("--id={C}"),
            &format!("--location={location}"),
        ]);

        // Pull the `created_at` out of an oracle JSON line.
        let created_at_of = |line: &str| -> String {
            let v: serde_json::Value = serde_json::from_str(line).expect("parse oracle line");
            v["created_at"].as_str().expect("created_at str").to_owned()
        };

        // --- revisions (incl. the null root) ---
        let oracle_rev = run(&["revisions", &format!("--location={location}")]);
        let rev_lines: Vec<&str> = oracle_rev.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            rev_lines.len(),
            2,
            "expected 2 revisions, got: {oracle_rev:?}"
        );
        // DESC: C (prev A) first, then A (prev null).
        let r0 = rec(&created_at_of(rev_lines[0]), C, location, Some(A));
        assert_eq!(revisions_json_line(&r0), rev_lines[0]);
        let r1 = rec(&created_at_of(rev_lines[1]), A, location, None);
        assert_eq!(revisions_json_line(&r1), rev_lines[1]);
        // Confirm the oracle really emitted a bare `null` for the root.
        assert!(
            rev_lines[1].contains(r#""previous_id":null"#),
            "oracle root revision should render null: {}",
            rev_lines[1]
        );

        // --- locations (latest id per location -> C) ---
        let oracle_loc = run(&["locations"]);
        let loc_lines: Vec<&str> = oracle_loc.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            loc_lines.len(),
            1,
            "expected 1 location, got: {oracle_loc:?}"
        );
        let l0 = rec(&created_at_of(loc_lines[0]), C, location, None);
        assert_eq!(locations_json_line(&l0), loc_lines[0]);

        // --- ancestors of C (id=C, previous_id non-null) -> one row, id=A ---
        let oracle_anc = run(&["ancestors", &format!("--id={C}")]);
        let anc_lines: Vec<&str> = oracle_anc.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            anc_lines.len(),
            1,
            "expected 1 ancestor, got: {oracle_anc:?}"
        );
        // The oracle projects previous_id (A) into the `id` field.
        let a0 = rec(&created_at_of(anc_lines[0]), A, location, None);
        assert_eq!(ancestors_json_line(&a0), anc_lines[0]);
    }

    // ----- rebuild: regenerate a location's history from a store view --------
    //
    // A store yields only (id, created_at) per location; rebuild must reproduce
    // identical locations/ancestors/revisions output by replaying in created_at
    // order through the same insert logic save uses.

    fn entry(id: &str, created_at: &str) -> RebuildEntry {
        RebuildEntry {
            id: id.to_owned(),
            created_at: created_at.to_owned(),
        }
    }

    /// Serializes a catalog's three queries for a location into a single byte
    /// blob, so two catalogs can be compared byte-for-byte through the
    /// frozen-format serializers (the public contract).
    fn query_bytes(cat: &Catalog, location: &str) -> String {
        let mut out = String::new();
        let mut locs = cat.locations().unwrap();
        locs.sort_by(|a, b| a.location.cmp(&b.location));
        for r in &locs {
            out.push_str(&locations_json_line(r));
            out.push('\n');
        }
        // ancestors of the location's current head (if any).
        if let Some(head) = locs.iter().find(|r| r.location == location) {
            for r in cat.ancestors(&head.id, Some(location)).unwrap() {
                out.push_str(&ancestors_json_line(&r));
                out.push('\n');
            }
        }
        for r in cat.revisions(location).unwrap() {
            out.push_str(&revisions_json_line(&r));
            out.push('\n');
        }
        out
    }

    /// Core assertion: a catalog built by `save` and a catalog `rebuild`-ed from
    /// the store view it would have persisted produce byte-identical query
    /// output — even though the store view is order-independent (shuffled here)
    /// and carries no `previous_id`.
    #[test]
    fn rebuild_round_trip_identical_query_output() {
        let location = "s3://bucket/some/path";

        // Build catalog A live via save: linear history A -> B -> C, with a
        // consecutive duplicate (B saved twice) that save dedups.
        let dir_a = TempDir::new();
        let cat_a = Catalog::open(dir_a.db_path()).unwrap();
        let clock = seq_clock(&[
            "2026-06-01 00:00:00.001", // A (root)
            "2026-06-01 00:00:00.002", // B
            "2026-06-01 00:00:00.003", // B again -> no-op (head==id)
            "2026-06-01 00:00:00.004", // C
        ]);
        cat_a.save(location, A, &clock).unwrap();
        cat_a.save(location, B, &clock).unwrap();
        cat_a.save(location, B, &clock).unwrap(); // dedup no-op
        cat_a.save(location, C, &clock).unwrap();

        // Derive the STORE VIEW = the (id, created_at) set A actually persisted
        // for this location (its revisions), simulating store enumeration. Note
        // the deduped second-B was never written, so the store has 3 manifests.
        let revs_a = cat_a.revisions(location).unwrap();
        assert_eq!(revs_a.len(), 3);
        let mut store_view: Vec<RebuildEntry> =
            revs_a.iter().map(|r| entry(&r.id, &r.created_at)).collect();
        // Stores are order-independent: shuffle to prove rebuild sorts by
        // created_at (deterministic reorder, no rng dep).
        store_view.reverse();
        store_view.swap(0, 1);

        // Rebuild a FRESH catalog B from the shuffled store view.
        let dir_b = TempDir::new();
        let cat_b = Catalog::open(dir_b.db_path()).unwrap();
        cat_b.rebuild(location, store_view).unwrap();

        // Byte-for-byte identical query output through the frozen serializers.
        assert_eq!(query_bytes(&cat_a, location), query_bytes(&cat_b, location));

        // Spot-check the reconstructed chain: C(prev B), B(prev A), A(prev null).
        let revs_b = cat_b.revisions(location).unwrap();
        assert_eq!(revs_b.len(), 3);
        assert_eq!(revs_b[0].id, C);
        assert_eq!(revs_b[0].previous_id.as_deref(), Some(B));
        assert_eq!(revs_b[1].id, B);
        assert_eq!(revs_b[1].previous_id.as_deref(), Some(A));
        assert_eq!(revs_b[2].id, A);
        assert_eq!(revs_b[2].previous_id, None); // root -> null
    }

    #[test]
    fn rebuild_previous_id_reconstruction_matches_save_ordering() {
        let location = "/local/foo";
        // save-built reference.
        let dir_a = TempDir::new();
        let cat_a = Catalog::open(dir_a.db_path()).unwrap();
        let clock = seq_clock(&[
            "2026-06-01 00:00:00.010",
            "2026-06-01 00:00:00.020",
            "2026-06-01 00:00:00.030",
        ]);
        cat_a.save(location, A, &clock).unwrap();
        cat_a.save(location, B, &clock).unwrap();
        cat_a.save(location, C, &clock).unwrap();

        // rebuild from an unsorted store view.
        let dir_b = TempDir::new();
        let cat_b = Catalog::open(dir_b.db_path()).unwrap();
        cat_b
            .rebuild(
                location,
                vec![
                    entry(C, "2026-06-01 00:00:00.030"),
                    entry(A, "2026-06-01 00:00:00.010"),
                    entry(B, "2026-06-01 00:00:00.020"),
                ],
            )
            .unwrap();

        assert_eq!(
            cat_a.revisions(location).unwrap(),
            cat_b.revisions(location).unwrap()
        );
        assert_eq!(
            cat_a.ancestors(C, None).unwrap(),
            cat_b.ancestors(C, None).unwrap()
        );
    }

    #[test]
    fn rebuild_clears_stale_records_for_location() {
        let location = "s3://bar";
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        let clock = seq_clock(&["2026-06-01 00:00:00.001", "2026-06-01 00:00:00.002"]);
        // Pre-existing DIFFERENT history: A -> B.
        cat.save(location, A, &clock).unwrap();
        cat.save(location, B, &clock).unwrap();
        assert_eq!(cat.revisions(location).unwrap().len(), 2);

        // Rebuild from a store view that no longer contains B (only A then C).
        cat.rebuild(
            location,
            vec![
                entry(A, "2026-06-01 00:00:00.001"),
                entry(C, "2026-06-01 00:00:00.005"),
            ],
        )
        .unwrap();

        let revs = cat.revisions(location).unwrap();
        assert_eq!(revs.len(), 2);
        assert_eq!(revs[0].id, C);
        assert_eq!(revs[0].previous_id.as_deref(), Some(A));
        assert_eq!(revs[1].id, A);
        assert_eq!(revs[1].previous_id, None);
        // The stale id B is gone everywhere: it no longer appears as an id in
        // revisions, and ancestors keyed on B is empty.
        assert!(revs.iter().all(|r| r.id != B));
        assert!(cat.ancestors(B, None).unwrap().is_empty());
        // loc_head now points at C.
        let loc = cat
            .locations()
            .unwrap()
            .into_iter()
            .find(|r| r.location == location)
            .unwrap();
        assert_eq!(loc.id, C);
    }

    #[test]
    fn rebuild_leaves_other_locations_untouched() {
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        let clock = seq_clock(&[
            "2026-06-01 00:00:00.001",
            "2026-06-01 00:00:00.002",
            "2026-06-01 00:00:00.003",
        ]);
        cat.save("s3://other", A, &clock).unwrap();
        cat.save("s3://other", B, &clock).unwrap();
        let other_before = cat.revisions("s3://other").unwrap();

        // Rebuild a DIFFERENT location.
        cat.rebuild("s3://target", vec![entry(C, "2026-06-01 00:00:00.050")])
            .unwrap();

        // s3://other is byte-for-byte unchanged.
        assert_eq!(other_before, cat.revisions("s3://other").unwrap());
        // Both locations now appear in locations().
        let mut locs = cat.locations().unwrap();
        locs.sort_by(|a, b| a.location.cmp(&b.location));
        assert_eq!(locs.len(), 2);
    }

    #[test]
    fn rebuild_is_idempotent() {
        let location = "s3://idem";
        let view = vec![
            entry(A, "2026-06-01 00:00:00.001"),
            entry(B, "2026-06-01 00:00:00.002"),
            entry(C, "2026-06-01 00:00:00.003"),
        ];

        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        cat.rebuild(location, view.clone()).unwrap();
        let once = query_bytes(&cat, location);
        // Rebuilding the same store view again yields the same catalog.
        cat.rebuild(location, view).unwrap();
        let twice = query_bytes(&cat, location);
        assert_eq!(once, twice);
        assert_eq!(cat.revisions(location).unwrap().len(), 3);
    }

    #[test]
    fn rebuild_empty_view_clears_location() {
        let location = "s3://empty";
        let dir = TempDir::new();
        let cat = Catalog::open(dir.db_path()).unwrap();
        let clock = seq_clock(&["2026-06-01 00:00:00.001"]);
        cat.save(location, A, &clock).unwrap();
        assert_eq!(cat.revisions(location).unwrap().len(), 1);
        // An empty store view (no manifests) clears the location entirely.
        cat.rebuild(location, Vec::<RebuildEntry>::new()).unwrap();
        assert!(cat.revisions(location).unwrap().is_empty());
        assert!(cat
            .locations()
            .unwrap()
            .into_iter()
            .all(|r| r.location != location));
    }

    #[test]
    fn system_clock_formats_millis() {
        // 2021-01-01 00:00:00.123 UTC = 1609459200 s.
        assert_eq!(format_millis(1_609_459_200, 123), "2021-01-01 00:00:00.123");
        // Epoch.
        assert_eq!(format_millis(0, 0), "1970-01-01 00:00:00.000");
        // A leap-year date with time-of-day.
        assert_eq!(
            format_millis(1_582_934_400 + 3_661, 7),
            "2020-02-29 01:01:01.007"
        );
        // SystemClock produces the right shape/length.
        let s = SystemClock.now();
        assert_eq!(s.len(), "YYYY-MM-DD HH:MM:SS.SSS".len());
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[19..20], ".");
    }
}
