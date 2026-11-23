//! SQLite store: the only module that runs SQL.

mod anchor;
mod extract;
mod ingest;
mod mirror;
mod pack;
mod schema;
mod search;
mod ui;

use std::error::Error;
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::types::Type;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};
use thiserror::Error;

use crate::model::{self, Anchor, Memory, NewMemory, Origin, Provenance, Scope, Status};

pub use extract::EventRow;
pub use ingest::{Checkpoint, SegmentRow, SourceRow, StoredEvent, StoredKind};
pub use mirror::FullMemory;
pub use pack::{InjectionStats, NewInjection, ProjectInjections, Via};
pub use schema::version as schema_version;
pub use search::{Filter, Hit};
pub use ui::{
    AnchorView, Cursor, InjectionBrief, InjectionDetail, InjectionFilter, InjectionRow,
    MemoryBrief, MemoryDetail, MemoryFilter, MemoryRow, MemorySort, Overview, Page, ProvenanceView,
    SessionDetail, SessionEvent, SessionFilter, SessionRow, SourceRef, saved_pct,
};

/// Store failures.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("memory not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
    /// An id prefix matches more than one memory.
    #[error("ambiguous: {0}")]
    Ambiguous(String),
    /// A read-only open of a database whose schema differs from this build's
    /// (which a read-only connection must not migrate).
    #[error("database schema v{found} does not match this build (v{expected})")]
    Schema { found: u32, expected: u32 },
}

impl StoreError {
    /// Another connection holds a lock past the busy timeout.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(e, _))
                if matches!(
                    e.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        )
    }
}

/// Source of unix-millisecond timestamps. Injectable so tests are deterministic.
pub type Clock = Box<dyn Fn() -> i64 + Send>;

fn system_clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Outcome of [`Store::insert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Insert {
    Inserted(String),
    /// The memory already existed; provenance and counters were merged.
    Duplicate(String),
}

impl Insert {
    /// The memory's uid either way.
    #[must_use]
    pub fn uid(&self) -> &str {
        match self {
            Self::Inserted(u) | Self::Duplicate(u) => u,
        }
    }
}

/// Columns read by [`memory_from_row`]; the table must be aliased `m`.
const MEMORY_COLS: &str = "m.uid, m.kind, m.text, m.scope_project, m.scope_path, m.confidence, \
    m.importance, m.status, m.superseded_by, m.seen_count, m.created_at, m.updated_at, \
    m.valid_from, m.valid_to";

fn parse_col<T>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: Error + Send + Sync + 'static,
{
    let raw: String = row.get(idx)?;
    raw.parse()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e)))
}

/// Maps a [`MEMORY_COLS`] row. Provenance and anchors are loaded separately.
fn memory_from_row(row: &Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        uid: row.get(0)?,
        kind: parse_col(row, 1)?,
        text: row.get(2)?,
        scope: Scope::from_parts(row.get(3)?, row.get(4)?),
        confidence: row.get(5)?,
        importance: row.get(6)?,
        status: parse_col(row, 7)?,
        superseded_by: row.get(8)?,
        seen_count: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        valid_from: row.get(12)?,
        valid_to: row.get(13)?,
        provenance: Vec::new(),
        anchors: Vec::new(),
        stale_reason: None,
    })
}

/// Row id for `uid`, or `NotFound`.
fn require(conn: &Connection, uid: &str) -> Result<i64, StoreError> {
    conn.query_row("SELECT id FROM memories WHERE uid = ?1", [uid], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| StoreError::NotFound(uid.to_owned()))
}

/// Bumps a memory's `updated_at` when only its children (anchors) changed, so
/// "last changed" stays truthful. (The mirror relies on triggers, not this.)
fn touch(conn: &Connection, id: i64, now: i64) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE memories SET updated_at = ?2 WHERE id = ?1",
        [id, now],
    )?;
    Ok(())
}

/// The body of [`Store::insert`] inside an open transaction.
fn insert_in(
    tx: &Transaction<'_>,
    now: i64,
    new: &NewMemory,
    prov: &Provenance,
) -> Result<Insert, StoreError> {
    let uid = model::uid(new.kind, &new.scope, &new.text);
    let existing: Option<i64> = tx
        .query_row("SELECT id FROM memories WHERE uid = ?1", [&uid], |r| {
            r.get(0)
        })
        .optional()?;
    let (id, outcome) = if let Some(id) = existing {
        tx.execute(
            "UPDATE memories SET seen_count = seen_count + 1,
                 confidence = max(confidence, ?2), updated_at = ?3 WHERE id = ?1",
            params![id, new.confidence, now],
        )?;
        (id, Insert::Duplicate(uid))
    } else {
        let (project, path) = new.scope.parts();
        tx.execute(
            "INSERT INTO memories (uid, kind, text, scope_key, scope_project, scope_path,
                 confidence, importance, created_at, updated_at, valid_from)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?9)",
            params![
                uid,
                new.kind.as_str(),
                new.text,
                new.scope.to_string(),
                project,
                path,
                new.confidence,
                new.importance,
                now
            ],
        )?;
        (tx.last_insert_rowid(), Insert::Inserted(uid))
    };
    tx.execute(
        "INSERT OR IGNORE INTO provenance
             (memory_id, origin, harness, session_id, message_uuid, source_path, line,
              git_commit, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            prov.origin.as_str(),
            prov.harness,
            prov.session_id,
            prov.message_uuid,
            prov.source_path,
            prov.line,
            prov.git_commit,
            now
        ],
    )?;
    Ok(outcome)
}

/// Default time a statement waits on another writer.
pub const DEFAULT_BUSY: Duration = Duration::from_millis(5000);

/// Handle to a Recalla database.
pub struct Store {
    conn: Connection,
    clock: Clock,
}

impl Store {
    /// Opens (creating if needed) the database at `path` and migrates it.
    ///
    /// # Errors
    /// Fails if the file can't be opened or migrated.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with_busy(path, DEFAULT_BUSY)
    }

    /// [`Store::open`] with a custom SQLite busy timeout: how long any
    /// statement (migration included) waits on another writer before failing.
    ///
    /// # Errors
    /// Fails if the file can't be opened or migrated.
    pub fn open_with_busy(path: impl AsRef<Path>, busy: Duration) -> Result<Self, StoreError> {
        Self::init(Connection::open(path)?, true, busy)
    }

    /// Opens an existing database read-only (SQLite refuses every write) and
    /// never migrates it.
    ///
    /// # Errors
    /// `Sqlite` if the file is missing or can't be opened; `Schema` if its
    /// `user_version` differs from this build's.
    pub fn open_read_only(path: impl AsRef<Path>, busy: Duration) -> Result<Self, StoreError> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(busy)?;
        let store = Self {
            conn,
            clock: Box::new(system_clock),
        };
        let (found, expected) = (store.user_version()?, schema::version());
        if found != expected {
            return Err(StoreError::Schema { found, expected });
        }
        Ok(store)
    }

    /// Opens a private in-memory database (tests, dry runs).
    ///
    /// # Errors
    /// Fails if migration fails.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?, false, DEFAULT_BUSY)
    }

    fn init(mut conn: Connection, wal: bool, busy: Duration) -> Result<Self, StoreError> {
        schema::configure(&conn, wal, busy)?;
        schema::migrate(&mut conn)?;
        Ok(Self {
            conn,
            clock: Box::new(system_clock),
        })
    }

    /// Replaces the clock (for deterministic tests).
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    /// Schema version of the open database.
    ///
    /// # Errors
    /// Fails if the pragma can't be read.
    pub fn user_version(&self) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }
}

impl Store {
    /// Inserts a memory with its first provenance. If a memory with the same
    /// uid exists, merges instead: adds the provenance (identical sources are
    /// ignored), bumps `seen_count`, keeps the higher confidence.
    ///
    /// # Errors
    /// `Invalid` if `new` violates model invariants; `Sqlite` on failure.
    pub fn insert(&mut self, new: &NewMemory, prov: &Provenance) -> Result<Insert, StoreError> {
        new.validate()
            .map_err(|e| StoreError::Invalid(e.to_owned()))?;
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = insert_in(&tx, now, new, prov)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Bulk [`Store::insert`] with anchors, all in one transaction. For
    /// benchmarks and fixtures; the product paths use `insert`.
    ///
    /// # Errors
    /// As `insert`; nothing is stored if any item fails.
    #[doc(hidden)]
    pub fn insert_many(
        &mut self,
        items: &[(NewMemory, Provenance, Vec<Anchor>)],
    ) -> Result<(), StoreError> {
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (new, prov, anchors) in items {
            new.validate()
                .map_err(|e| StoreError::Invalid(e.to_owned()))?;
            let uid = insert_in(&tx, now, new, prov)?.uid().to_owned();
            let id = require(&tx, &uid)?;
            for a in anchors {
                tx.execute(
                    "INSERT OR IGNORE INTO anchors
                         (memory_id, path, symbol, lang, content_hash, git_commit)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, a.path, a.symbol, a.lang, a.content_hash, a.git_commit],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Fetches a memory with its provenance and anchors, in any status.
    ///
    /// # Errors
    /// `Sqlite` on failure. A missing uid is `Ok(None)`.
    pub fn get(&self, uid: &str) -> Result<Option<Memory>, StoreError> {
        let sql = format!("SELECT {MEMORY_COLS} FROM memories m WHERE m.uid = ?1");
        let Some(mut memory) = self
            .conn
            .query_row(&sql, [uid], memory_from_row)
            .optional()?
        else {
            return Ok(None);
        };
        self.load_children(&mut memory)?;
        Ok(Some(memory))
    }

    pub(super) fn load_children(&self, memory: &mut Memory) -> Result<(), StoreError> {
        memory.provenance = self
            .conn
            .prepare_cached(
                "SELECT p.origin, p.harness, p.session_id, p.message_uuid, p.source_path,
                        p.line, p.git_commit
                 FROM provenance p JOIN memories m ON m.id = p.memory_id
                 WHERE m.uid = ?1 ORDER BY p.id",
            )?
            .query_map([&memory.uid], |r| {
                Ok(Provenance {
                    origin: parse_col::<Origin>(r, 0)?,
                    harness: r.get(1)?,
                    session_id: r.get(2)?,
                    message_uuid: r.get(3)?,
                    source_path: r.get(4)?,
                    line: r.get(5)?,
                    git_commit: r.get(6)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        memory.anchors = self
            .conn
            .prepare_cached(
                "SELECT a.path, a.symbol, a.lang, a.content_hash, a.git_commit
                 FROM anchors a JOIN memories m ON m.id = a.memory_id
                 WHERE m.uid = ?1 ORDER BY a.id",
            )?
            .query_map([&memory.uid], |r| {
                Ok(Anchor {
                    path: r.get(0)?,
                    symbol: r.get(1)?,
                    lang: r.get(2)?,
                    content_hash: r.get(3)?,
                    git_commit: r.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    /// Ties a memory to a code location. Re-adding the same path and symbol
    /// is a no-op.
    ///
    /// # Errors
    /// `NotFound` if the memory doesn't exist.
    pub fn add_anchor(&mut self, uid: &str, anchor: &Anchor) -> Result<(), StoreError> {
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = require(&tx, uid)?;
        let added = tx.execute(
            "INSERT OR IGNORE INTO anchors (memory_id, path, symbol, lang, content_hash, git_commit)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                anchor.path,
                anchor.symbol,
                anchor.lang,
                anchor.content_hash,
                anchor.git_commit
            ],
        )?;
        if added > 0 {
            touch(&tx, id, now)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Marks `old` superseded by `new` and records a `supersedes` relation
    /// (`new` -> `old`), atomically. Reversible with [`Store::restore`].
    ///
    /// # Errors
    /// `Invalid` if `old == new`; `NotFound` if either is missing.
    pub fn supersede(&mut self, old: &str, new: &str) -> Result<(), StoreError> {
        if old == new {
            return Err(StoreError::Invalid(
                "a memory cannot supersede itself".to_owned(),
            ));
        }
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require(&tx, new)?;
        require(&tx, old)?;
        tx.execute(
            "UPDATE memories SET status = 'superseded', superseded_by = ?2,
                 valid_to = ?3, updated_at = ?3 WHERE uid = ?1",
            params![old, new, now],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO relations (src, dst, kind, created_at)
             VALUES (?1, ?2, 'supersedes', ?3)",
            params![new, old, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Hides a memory from default search without deleting it.
    ///
    /// # Errors
    /// `NotFound` if the memory doesn't exist.
    pub fn archive(&mut self, uid: &str) -> Result<(), StoreError> {
        self.set_status(uid, Status::Archived)
    }

    /// Makes a memory active again, clearing any supersede link.
    ///
    /// # Errors
    /// `NotFound` if the memory doesn't exist.
    pub fn restore(&mut self, uid: &str) -> Result<(), StoreError> {
        self.set_status(uid, Status::Active)
    }

    /// Sets a lifecycle status. `Active` clears `superseded_by`, `valid_to`
    /// and the `supersedes` relation pointing at the memory. `Superseded`
    /// is only reachable through [`Store::supersede`].
    ///
    /// # Errors
    /// `Invalid` for `Superseded`; `NotFound` if the memory doesn't exist.
    pub fn set_status(&mut self, uid: &str, status: Status) -> Result<(), StoreError> {
        if status == Status::Superseded {
            return Err(StoreError::Invalid(
                "use supersede() to mark a memory superseded".to_owned(),
            ));
        }
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require(&tx, uid)?;
        tx.execute(
            "UPDATE memories SET status = ?2, updated_at = ?3 WHERE uid = ?1",
            params![uid, status.as_str(), now],
        )?;
        if status == Status::Active {
            tx.execute(
                "UPDATE memories SET superseded_by = NULL, valid_to = NULL WHERE uid = ?1",
                [uid],
            )?;
            tx.execute(
                "DELETE FROM relations WHERE dst = ?1 AND kind = 'supersedes'",
                [uid],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Sets a memory's confidence.
    ///
    /// # Errors
    /// `Invalid` outside 0..=1; `NotFound` if the memory doesn't exist.
    pub fn set_confidence(&mut self, uid: &str, confidence: f32) -> Result<(), StoreError> {
        if !(0.0..=1.0).contains(&confidence) {
            return Err(StoreError::Invalid(
                "confidence must be within 0..=1".to_owned(),
            ));
        }
        let now = (self.clock)();
        let changed = self.conn.execute(
            "UPDATE memories SET confidence = ?2, updated_at = ?3 WHERE uid = ?1",
            params![uid, confidence, now],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(uid.to_owned()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::model::Kind;

    #[test]
    fn opens_in_memory_at_current_schema() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.user_version().unwrap(), schema_version());
    }

    fn mem(text: &str) -> NewMemory {
        NewMemory {
            kind: Kind::Fact,
            text: text.into(),
            scope: Scope::Global,
            confidence: 0.5,
            importance: 1,
        }
    }

    #[test]
    fn insert_many_stores_memories_with_anchors_and_merges_duplicates() {
        let mut store = Store::open_in_memory().unwrap();
        let items: Vec<_> = ["a", "b", "a"]
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let line = i64::try_from(i).unwrap();
                (mem(t), prov(line), vec![Anchor::new("src/x.rs")])
            })
            .collect();
        store.insert_many(&items).unwrap();
        let all = store.all_memories(&[Status::Active]).unwrap();
        assert_eq!(all.len(), 2);
        let a = all.iter().find(|m| m.text == "a").unwrap();
        let a = store.get(&a.uid).unwrap().unwrap();
        assert_eq!(
            (a.seen_count, a.provenance.len(), a.anchors.len()),
            (2, 2, 1)
        );
    }

    fn prov(line: i64) -> Provenance {
        Provenance {
            session_id: Some("s1".into()),
            line: Some(line),
            ..Provenance::new(Origin::Backfill)
        }
    }

    fn store_at(clock: &Arc<AtomicI64>) -> Store {
        let mut s = Store::open_in_memory().unwrap();
        let c = Arc::clone(clock);
        s.set_clock(Box::new(move || c.load(Ordering::SeqCst)));
        s
    }

    #[test]
    fn insert_then_get() {
        let clock = Arc::new(AtomicI64::new(1000));
        let mut s = store_at(&clock);
        let Insert::Inserted(uid) = s.insert(&mem("Use SQLite"), &prov(1)).unwrap() else {
            panic!("expected Inserted");
        };
        let m = s.get(&uid).unwrap().unwrap();
        assert_eq!(m.text, "Use SQLite");
        assert_eq!(m.status, Status::Active);
        assert_eq!(m.seen_count, 1);
        assert_eq!(
            (m.created_at, m.updated_at, m.valid_from),
            (1000, 1000, 1000)
        );
        assert_eq!(m.valid_to, None);
        assert_eq!(m.provenance, vec![prov(1)]);
        assert!(s.get("missing").unwrap().is_none());
    }

    #[test]
    fn duplicate_merges_provenance_and_confidence() {
        let clock = Arc::new(AtomicI64::new(1000));
        let mut s = store_at(&clock);
        let uid = s
            .insert(&mem("Use SQLite"), &prov(1))
            .unwrap()
            .uid()
            .to_owned();
        clock.store(2000, Ordering::SeqCst);
        let mut again = mem("  Use   SQLite ");
        again.confidence = 0.9;
        assert_eq!(
            s.insert(&again, &prov(2)).unwrap(),
            Insert::Duplicate(uid.clone())
        );
        let m = s.get(&uid).unwrap().unwrap();
        assert_eq!(m.seen_count, 2);
        assert!((m.confidence - 0.9).abs() < f32::EPSILON);
        assert_eq!(m.updated_at, 2000);
        assert_eq!(m.created_at, 1000);
        assert_eq!(m.provenance.len(), 2);

        // Lower confidence never lowers the stored value.
        s.insert(&mem("Use SQLite"), &prov(2)).unwrap();
        let m = s.get(&uid).unwrap().unwrap();
        assert!((m.confidence - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn identical_provenance_is_idempotent_including_nulls() {
        let mut s = Store::open_in_memory().unwrap();
        let bare = Provenance::new(Origin::Remember);
        let uid = s.insert(&mem("a"), &bare).unwrap().uid().to_owned();
        s.insert(&mem("a"), &bare).unwrap();
        s.insert(&mem("a"), &prov(1)).unwrap();
        s.insert(&mem("a"), &prov(1)).unwrap();
        let m = s.get(&uid).unwrap().unwrap();
        assert_eq!(m.provenance.len(), 2);
        assert_eq!(m.seen_count, 4);
    }

    #[test]
    fn insert_rejects_invalid_input() {
        let mut s = Store::open_in_memory().unwrap();
        let mut bad = mem("x");
        bad.confidence = 2.0;
        assert!(matches!(
            s.insert(&bad, &prov(1)),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn supersede_links_and_restore_unlinks() {
        let clock = Arc::new(AtomicI64::new(1000));
        let mut s = store_at(&clock);
        let old = s.insert(&mem("old"), &prov(1)).unwrap().uid().to_owned();
        let new = s.insert(&mem("new"), &prov(2)).unwrap().uid().to_owned();
        clock.store(3000, Ordering::SeqCst);
        s.supersede(&old, &new).unwrap();
        let m = s.get(&old).unwrap().unwrap();
        assert_eq!(m.status, Status::Superseded);
        assert_eq!(m.superseded_by.as_deref(), Some(new.as_str()));
        assert_eq!(m.valid_to, Some(3000));
        let rels: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM relations WHERE src = ?1 AND dst = ?2 AND kind = 'supersedes'",
                [&new, &old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rels, 1);

        s.restore(&old).unwrap();
        let m = s.get(&old).unwrap().unwrap();
        assert_eq!(m.status, Status::Active);
        assert_eq!((m.superseded_by, m.valid_to), (None, None));
        let rels: i64 = s
            .conn
            .query_row("SELECT count(*) FROM relations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rels, 0);
    }

    #[test]
    fn supersede_validates_inputs() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.insert(&mem("a"), &prov(1)).unwrap().uid().to_owned();
        assert!(matches!(s.supersede(&a, &a), Err(StoreError::Invalid(_))));
        assert!(matches!(
            s.supersede(&a, "nope"),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            s.supersede("nope", &a),
            Err(StoreError::NotFound(_))
        ));
        // A failed supersede changes nothing.
        assert_eq!(s.get(&a).unwrap().unwrap().status, Status::Active);
    }

    #[test]
    fn status_transitions() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.insert(&mem("a"), &prov(1)).unwrap().uid().to_owned();
        s.archive(&a).unwrap();
        assert_eq!(s.get(&a).unwrap().unwrap().status, Status::Archived);
        s.set_status(&a, Status::NeedsReverify).unwrap();
        assert_eq!(s.get(&a).unwrap().unwrap().status, Status::NeedsReverify);
        assert!(matches!(
            s.set_status(&a, Status::Superseded),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(s.archive("nope"), Err(StoreError::NotFound(_))));
        assert!(matches!(s.restore("nope"), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn set_confidence_validates_and_updates() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.insert(&mem("a"), &prov(1)).unwrap().uid().to_owned();
        s.set_confidence(&a, 0.9).unwrap();
        assert!((s.get(&a).unwrap().unwrap().confidence - 0.9).abs() < f32::EPSILON);
        assert!(matches!(
            s.set_confidence(&a, 1.5),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            s.set_confidence("nope", 0.5),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn anchors_are_stored_and_deduped() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.insert(&mem("a"), &prov(1)).unwrap().uid().to_owned();
        let anchor = Anchor {
            symbol: Some("main".into()),
            lang: Some("rust".into()),
            ..Anchor::new("src/main.rs")
        };
        s.add_anchor(&a, &anchor).unwrap();
        s.add_anchor(&a, &anchor).unwrap();
        assert_eq!(s.get(&a).unwrap().unwrap().anchors, vec![anchor.clone()]);
        assert!(matches!(
            s.add_anchor("nope", &anchor),
            Err(StoreError::NotFound(_))
        ));
    }
}
