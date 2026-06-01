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
//! Pinned to the frozen oracle script `./snapdir-sqlite3-catalog` (read only).
//! Its data model is one core table
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
