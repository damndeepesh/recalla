//! Ingest checkpoints, redacted events and archive segments (schema v2).
//!
//! Callers hand in text that has ALREADY been redacted and truncated; the
//! store persists it as given.

use rusqlite::types::Type;
use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};

use super::{Store, StoreError};

/// One ingested log file and its resume checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: i64,
    pub harness: String,
    pub path: String,
    /// Hex blake3 of the file's first bytes (see `ingest`). Empty until the
    /// first batch commits.
    pub identity: String,
    /// Hex blake3 of the last complete line before `next_offset` (see
    /// `ingest`). Empty until the first batch commits.
    pub tail_hash: String,
    pub size: u64,
    pub next_offset: u64,
    pub events: u64,
}

/// Checkpoint written together with a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub identity: String,
    pub tail_hash: String,
    pub size: u64,
    pub next_offset: u64,
}

/// A raw-byte range of a source kept in the archive under `hash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRow {
    pub start: u64,
    pub end: u64,
    pub hash: String,
}

/// What a stored event records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredKind {
    UserText,
    AssistantText,
    ToolCall,
    ToolResult,
    Summary,
}

impl StoredKind {
    /// Stable name, as stored in `events.kind`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserText => "user_text",
            Self::AssistantText => "assistant_text",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Summary => "summary",
        }
    }
}

/// An event ready to persist. Content fields (`text`, `tool_name`, `cwd`,
/// `git_branch`) must already be redacted; ids are stored as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub harness: String,
    pub session_id: String,
    pub message_uuid: Option<String>,
    pub parent_uuid: Option<String>,
    /// 1-based line in the source.
    pub line: u64,
    /// Index of this event among those decoded from the same line.
    pub seq: u32,
    pub byte_offset: u64,
    pub ts: Option<i64>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub sidechain: bool,
    pub kind: StoredKind,
    pub tool_name: Option<String>,
    /// Tool-call id, or the `tool_use_id` a result answers.
    pub tool_id: Option<String>,
    pub is_error: bool,
    pub text: String,
}

/// Offsets and counts are `u64` in Rust and `INTEGER` (i64) in SQLite.
fn int(v: u64) -> rusqlite::Result<i64> {
    i64::try_from(v).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn get_u64(r: &Row<'_>, idx: usize) -> rusqlite::Result<u64> {
    let v: i64 = r.get(idx)?;
    u64::try_from(v)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, Type::Integer, Box::new(e)))
}

const SOURCE_COLS: &str = "id, harness, path, identity, tail_hash, size, next_offset, events";

fn source_row(r: &Row<'_>) -> rusqlite::Result<SourceRow> {
    Ok(SourceRow {
        id: r.get(0)?,
        harness: r.get(1)?,
        path: r.get(2)?,
        identity: r.get(3)?,
        tail_hash: r.get(4)?,
        size: get_u64(r, 5)?,
        next_offset: get_u64(r, 6)?,
        events: get_u64(r, 7)?,
    })
}

impl Store {
    /// The source recorded for `path`, if any.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn source(&self, path: &str) -> Result<Option<SourceRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {SOURCE_COLS} FROM sources WHERE path = ?1"),
                [path],
                source_row,
            )
            .optional()?)
    }

    /// Every source, in path order.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn sources(&self) -> Result<Vec<SourceRow>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(&format!("SELECT {SOURCE_COLS} FROM sources ORDER BY path"))?
            .query_map([], source_row)?
            .collect::<Result<_, _>>()?)
    }

    /// Returns the id of the source for `path`, creating it (empty identity,
    /// offset 0) if missing. An existing source is left untouched.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn upsert_source(&mut self, harness: &str, path: &str) -> Result<i64, StoreError> {
        let now = (self.clock)();
        Ok(self.conn.query_row(
            "INSERT INTO sources (harness, path, identity, size, next_offset, updated_at)
             VALUES (?1, ?2, '', 0, 0, ?3)
             ON CONFLICT (path) DO UPDATE SET path = excluded.path
             RETURNING id",
            params![harness, path, now],
            |r| r.get(0),
        )?)
    }

    /// Inserts `events`, records `segment` and advances the checkpoint, all in
    /// one IMMEDIATE transaction. With `reset`, everything previously ingested
    /// from the source (events and segment rows) is forgotten first, in the
    /// same transaction, so a failed restart leaves the old data intact.
    /// Archive files are NOT removed: they are content-addressed and may be
    /// shared. Events already stored (same source, line
    /// and seq) are ignored, so replaying a batch is a no-op. Returns how many
    /// events were added.
    ///
    /// # Errors
    /// `NotFound` if the source doesn't exist; `Sqlite` on failure (nothing
    /// is written).
    pub fn ingest_batch(
        &mut self,
        source_id: i64,
        events: &[StoredEvent],
        checkpoint: &Checkpoint,
        segment: Option<&SegmentRow>,
        reset: bool,
    ) -> Result<u64, StoreError> {
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if reset {
            tx.execute("DELETE FROM events WHERE source_id = ?1", [source_id])?;
            tx.execute("DELETE FROM segments WHERE source_id = ?1", [source_id])?;
            tx.execute("UPDATE sources SET events = 0 WHERE id = ?1", [source_id])?;
        }
        let mut added = 0u64;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO events (source_id, harness, session_id, message_uuid,
                     parent_uuid, line, seq, byte_offset, ts, cwd, git_branch, sidechain, kind,
                     tool_name, tool_id, is_error, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17)",
            )?;
            for e in events {
                added += insert.execute(params![
                    source_id,
                    e.harness,
                    e.session_id,
                    e.message_uuid,
                    e.parent_uuid,
                    int(e.line)?,
                    e.seq,
                    int(e.byte_offset)?,
                    e.ts,
                    e.cwd,
                    e.git_branch,
                    e.sidechain,
                    e.kind.as_str(),
                    e.tool_name,
                    e.tool_id,
                    e.is_error,
                    e.text,
                ])? as u64;
            }
        }
        if let Some(s) = segment {
            tx.execute(
                "INSERT OR IGNORE INTO segments (source_id, start_offset, end_offset, hash)
                 VALUES (?1, ?2, ?3, ?4)",
                params![source_id, int(s.start)?, int(s.end)?, s.hash],
            )?;
        }
        let n = tx.execute(
            "UPDATE sources SET identity = ?2, tail_hash = ?7, size = ?3, next_offset = ?4,
                 events = events + ?5, updated_at = ?6 WHERE id = ?1",
            params![
                source_id,
                checkpoint.identity,
                int(checkpoint.size)?,
                int(checkpoint.next_offset)?,
                int(added)?,
                now,
                checkpoint.tail_hash
            ],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("source {source_id}")));
        }
        tx.commit()?;
        Ok(added)
    }

    /// Archive segments of a source, in start order.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn segments(&self, source_id: i64) -> Result<Vec<SegmentRow>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT start_offset, end_offset, hash FROM segments
                 WHERE source_id = ?1 ORDER BY start_offset",
            )?
            .query_map([source_id], |r| {
                Ok(SegmentRow {
                    start: get_u64(r, 0)?,
                    end: get_u64(r, 1)?,
                    hash: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    /// Total stored events.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn event_count(&self) -> Result<u64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM events", [], |r| get_u64(r, 0))?)
    }

    /// Every TEXT value of every ordinary table (FTS virtual and shadow
    /// tables excluded). For leak audits in tests and diagnostics; never
    /// log the result.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn all_text_values(&self) -> Result<Vec<String>, StoreError> {
        let tables: Vec<String> = self
            .conn
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                   AND sql NOT LIKE 'CREATE VIRTUAL%' AND name NOT LIKE '%_fts_%'",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::new();
        for table in tables {
            let cols: Vec<String> = self
                .conn
                .prepare("SELECT name FROM pragma_table_info(?1)")?
                .query_map([&table], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for col in cols {
                // Identifiers come from sqlite_schema, quoted defensively.
                let sql = format!(
                    "SELECT \"{c}\" FROM \"{t}\" WHERE typeof(\"{c}\") = 'text'",
                    c = col.replace('"', "\"\""),
                    t = table.replace('"', "\"\"")
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let values = stmt.query_map([], |r| r.get::<_, String>(0))?;
                for v in values {
                    out.push(v?);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(line: u64, seq: u32, text: &str) -> StoredEvent {
        StoredEvent {
            harness: "claude-code".into(),
            session_id: "s1".into(),
            message_uuid: Some(format!("m{line}")),
            parent_uuid: None,
            line,
            seq,
            byte_offset: line * 10,
            ts: Some(1),
            cwd: Some("/tmp/demo".into()),
            git_branch: Some("main".into()),
            sidechain: false,
            kind: StoredKind::UserText,
            tool_name: None,
            tool_id: None,
            is_error: false,
            text: text.into(),
        }
    }

    fn cp(next: u64) -> Checkpoint {
        Checkpoint {
            identity: "id".into(),
            tail_hash: "tail".into(),
            size: next,
            next_offset: next,
        }
    }

    #[test]
    fn source_lifecycle_and_idempotent_batches() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(s.source("/a").unwrap().is_none());
        let id = s.upsert_source("claude-code", "/a").unwrap();
        assert_eq!(s.upsert_source("claude-code", "/a").unwrap(), id);
        let fresh = s.source("/a").unwrap().unwrap();
        assert_eq!(s.sources().unwrap(), vec![fresh.clone()]);
        assert_eq!((fresh.identity.as_str(), fresh.next_offset), ("", 0));

        let batch = [event(1, 0, "a"), event(1, 1, "b"), event(2, 0, "c")];
        let seg = SegmentRow {
            start: 0,
            end: 30,
            hash: "h1".into(),
        };
        assert_eq!(
            s.ingest_batch(id, &batch, &cp(30), Some(&seg), false)
                .unwrap(),
            3
        );
        assert_eq!(
            s.ingest_batch(id, &batch, &cp(30), Some(&seg), false)
                .unwrap(),
            0
        );
        let row = s.source("/a").unwrap().unwrap();
        assert_eq!((row.next_offset, row.events, row.size), (30, 3, 30));
        assert_eq!(
            (row.identity.as_str(), row.tail_hash.as_str()),
            ("id", "tail")
        );
        assert_eq!(s.event_count().unwrap(), 3);
        assert_eq!(s.segments(id).unwrap(), vec![seg]);

        // A reset batch replaces everything in one transaction.
        let replacement = SegmentRow {
            start: 0,
            end: 5,
            hash: "h2".into(),
        };
        let added = s
            .ingest_batch(id, &[event(1, 0, "z")], &cp(5), Some(&replacement), true)
            .unwrap();
        assert_eq!(added, 1);
        let row = s.source("/a").unwrap().unwrap();
        assert_eq!((row.next_offset, row.events), (5, 1));
        assert_eq!(s.event_count().unwrap(), 1);
        assert_eq!(s.segments(id).unwrap(), vec![replacement]);
        s.ingest_batch(id, &[], &cp(5), None, true).unwrap();
        assert_eq!(s.event_count().unwrap(), 0);
        assert_eq!(s.source("/a").unwrap().unwrap().events, 0);
        // FTS rows follow the deletes.
        let fts: i64 = s
            .conn
            .query_row("SELECT count(*) FROM events_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts, 0);
    }

    #[test]
    fn unknown_source_is_not_found_and_writes_nothing() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(matches!(
            s.ingest_batch(99, &[], &cp(1), None, true),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn all_text_values_covers_event_columns() {
        let mut s = Store::open_in_memory().unwrap();
        let id = s.upsert_source("claude-code", "/a").unwrap();
        let mut e = event(1, 0, "body");
        e.tool_name = Some("ToolX".into());
        s.ingest_batch(id, &[e], &cp(10), None, false).unwrap();
        let all = s.all_text_values().unwrap();
        for want in ["body", "ToolX", "/tmp/demo", "main", "/a", "s1"] {
            assert!(all.iter().any(|v| v == want), "{want}");
        }
    }
}
