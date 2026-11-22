//! Extractor support (schema v3): ordered event reads, the extraction
//! checkpoint, and archiving of backfill-only memories for rebuilds.

use rusqlite::types::Type;
use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};

use super::ingest::get_u64;
use super::{Store, StoreError, StoredKind};
use crate::model::{Memory, Status};

/// A stored event joined with its source path, as the extractor reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub id: i64,
    pub source_path: String,
    pub harness: String,
    pub session_id: String,
    pub message_uuid: Option<String>,
    /// 1-based line in the source.
    pub line: u64,
    pub cwd: Option<String>,
    pub kind: StoredKind,
    pub tool_name: Option<String>,
    pub tool_id: Option<String>,
    pub is_error: bool,
    pub text: String,
    pub ts: Option<i64>,
    /// Subagent record: its "user" turns are the orchestrator's, not the human's.
    pub sidechain: bool,
}

fn event_row(r: &Row<'_>) -> rusqlite::Result<EventRow> {
    let kind: String = r.get(7)?;
    Ok(EventRow {
        id: r.get(0)?,
        source_path: r.get(1)?,
        harness: r.get(2)?,
        session_id: r.get(3)?,
        message_uuid: r.get(4)?,
        line: get_u64(r, 5)?,
        cwd: r.get(6)?,
        kind: StoredKind::parse(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(7, Type::Text, format!("kind {kind}").into())
        })?,
        tool_name: r.get(8)?,
        tool_id: r.get(9)?,
        is_error: r.get(10)?,
        text: r.get(11)?,
        ts: r.get(12)?,
        sidechain: r.get(13)?,
    })
}

impl Store {
    /// Up to `limit` events with id greater than `last_id`, in id order.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn events_after(&self, last_id: i64, limit: u32) -> Result<Vec<EventRow>, StoreError> {
        Ok(self
            .conn
            .prepare(
                "SELECT e.id, s.path, e.harness, e.session_id, e.message_uuid, e.line, e.cwd,
                        e.kind, e.tool_name, e.tool_id, e.is_error, e.text, e.ts, e.sidechain
                 FROM events e JOIN sources s ON s.id = e.source_id
                 WHERE e.id > ?1 ORDER BY e.id LIMIT ?2",
            )?
            .query_map(params![last_id, limit], event_row)?
            .collect::<Result<_, _>>()?)
    }

    /// Up to `limit` events of `session_id` with id below `before_id`, oldest
    /// first (the session window for a mid-session resume).
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn events_before_in_session(
        &self,
        session_id: &str,
        before_id: i64,
        limit: u32,
    ) -> Result<Vec<EventRow>, StoreError> {
        let mut rows: Vec<EventRow> = self
            .conn
            .prepare(
                "SELECT e.id, s.path, e.harness, e.session_id, e.message_uuid, e.line, e.cwd,
                        e.kind, e.tool_name, e.tool_id, e.is_error, e.text, e.ts, e.sidechain
                 FROM events e JOIN sources s ON s.id = e.source_id
                 WHERE e.session_id = ?1 AND e.id < ?2 ORDER BY e.id DESC LIMIT ?3",
            )?
            .query_map(params![session_id, before_id, limit], event_row)?
            .collect::<Result<_, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Every memory whose status is in `statuses`, oldest first, with
    /// provenance and anchors (inspection and eval).
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn all_memories(&self, statuses: &[Status]) -> Result<Vec<Memory>, StoreError> {
        let mut uids = Vec::new();
        let mut stmt = self
            .conn
            .prepare("SELECT uid, status FROM memories ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (uid, status) = row?;
            if statuses.iter().any(|s| s.as_str() == status) {
                uids.push(uid);
            }
        }
        uids.iter()
            .filter_map(|u| self.get(u).transpose())
            .collect()
    }

    /// `(last_event_id, extractor_version)`; `(0, 0)` before the first run.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn extract_checkpoint(&self) -> Result<(i64, u32), StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT last_event_id, extractor_version FROM extract_state WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0)))
    }

    /// Records extraction progress.
    ///
    /// # Errors
    /// `Invalid` on a negative id; `Sqlite` on failure.
    pub fn set_extract_checkpoint(&mut self, last_id: i64, version: u32) -> Result<(), StoreError> {
        if last_id < 0 {
            return Err(StoreError::Invalid("last_event_id must be >= 0".into()));
        }
        self.conn.execute(
            "INSERT INTO extract_state (id, last_event_id, extractor_version)
             VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET
                 last_event_id = excluded.last_event_id,
                 extractor_version = excluded.extractor_version",
            params![last_id, version],
        )?;
        Ok(())
    }

    /// Archives live memories whose provenance rows are ALL `backfill`
    /// (reversible: nothing is deleted). Memories with any other origin, or
    /// none, are untouched. Returns how many were archived.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn archive_backfill_only_memories(&mut self) -> Result<u64, StoreError> {
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE memories SET status = 'archived', updated_at = ?1
             WHERE status IN ('active', 'needs_reverify')
               AND EXISTS (SELECT 1 FROM provenance p WHERE p.memory_id = memories.id)
               AND NOT EXISTS (SELECT 1 FROM provenance p
                               WHERE p.memory_id = memories.id AND p.origin <> 'backfill')",
            [now],
        )?;
        tx.commit()?;
        u64::try_from(n).map_err(|_| StoreError::Invalid("row count".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, NewMemory, Origin, Provenance, Scope, Status};
    use crate::store::{Checkpoint, StoredEvent};

    fn event(line: u64, kind: StoredKind, text: &str) -> StoredEvent {
        StoredEvent {
            harness: "claude-code".into(),
            session_id: "s1".into(),
            message_uuid: Some(format!("m{line}")),
            parent_uuid: None,
            line,
            seq: 0,
            byte_offset: line * 10,
            ts: Some(5),
            cwd: Some("/tmp/demo".into()),
            git_branch: None,
            sidechain: false,
            kind,
            tool_name: Some("Write".into()),
            tool_id: Some("t1".into()),
            is_error: true,
            text: text.into(),
        }
    }

    fn seeded() -> Store {
        let mut s = Store::open_in_memory().unwrap();
        let id = s.upsert_source("claude-code", "/a.jsonl").unwrap();
        let cp = Checkpoint {
            identity: "i".into(),
            tail_hash: "t".into(),
            size: 40,
            next_offset: 40,
        };
        let batch = [
            event(1, StoredKind::UserText, "one"),
            event(2, StoredKind::ToolCall, "two"),
            event(3, StoredKind::Summary, "three"),
        ];
        s.ingest_batch(id, &batch, &cp, None, false).unwrap();
        s
    }

    #[test]
    fn events_after_is_ordered_limited_and_complete() {
        let s = seeded();
        let all = s.events_after(0, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.windows(2).all(|w| w[0].id < w[1].id));
        let e = &all[1];
        assert_eq!(
            (e.source_path.as_str(), e.harness.as_str(), e.kind),
            ("/a.jsonl", "claude-code", StoredKind::ToolCall)
        );
        assert_eq!(
            (e.session_id.as_str(), e.message_uuid.as_deref(), e.line),
            ("s1", Some("m2"), 2)
        );
        assert_eq!(
            (
                e.cwd.as_deref(),
                e.tool_name.as_deref(),
                e.tool_id.as_deref()
            ),
            (Some("/tmp/demo"), Some("Write"), Some("t1"))
        );
        assert_eq!((e.is_error, e.text.as_str(), e.ts), (true, "two", Some(5)));

        assert_eq!(s.events_after(0, 2).unwrap().len(), 2);
        let rest = s.events_after(all[0].id, 10).unwrap();
        assert_eq!(rest.iter().map(|e| e.line).collect::<Vec<_>>(), [2, 3]);
        let none = s.events_after(all[2].id, 10).unwrap();
        assert_eq!(none.len(), 0);
    }

    #[test]
    fn empty_text_events_round_trip() {
        let mut s = Store::open_in_memory().unwrap();
        let id = s.upsert_source("claude-code", "/e.jsonl").unwrap();
        let cp = Checkpoint {
            identity: "i".into(),
            tail_hash: "t".into(),
            size: 10,
            next_offset: 10,
        };
        let batch = [event(1, StoredKind::ToolResult, "")];
        s.ingest_batch(id, &batch, &cp, None, false).unwrap();
        let got = s.events_after(0, 10).unwrap();
        assert_eq!((got.len(), got[0].text.as_str()), (1, ""));
    }

    #[test]
    fn sidechain_flag_is_selected_by_both_readers() {
        let mut s = Store::open_in_memory().unwrap();
        let id = s.upsert_source("claude-code", "/sub.jsonl").unwrap();
        let cp = Checkpoint {
            identity: "i".into(),
            tail_hash: "t".into(),
            size: 40,
            next_offset: 40,
        };
        let mut side = event(2, StoredKind::UserText, "side");
        side.sidechain = true;
        let batch = [event(1, StoredKind::UserText, "main"), side];
        s.ingest_batch(id, &batch, &cp, None, false).unwrap();
        let all = s.events_after(0, 10).unwrap();
        assert_eq!(
            all.iter().map(|e| e.sidechain).collect::<Vec<_>>(),
            [false, true]
        );
        let prev = s.events_before_in_session("s1", all[1].id + 1, 5).unwrap();
        assert_eq!(
            prev.iter().map(|e| e.sidechain).collect::<Vec<_>>(),
            [false, true]
        );
    }

    #[test]
    fn events_before_in_session_is_bounded_and_oldest_first() {
        let s = seeded();
        let all = s.events_after(0, 10).unwrap();
        let got = s.events_before_in_session("s1", all[2].id, 5).unwrap();
        assert_eq!(got.iter().map(|e| e.line).collect::<Vec<_>>(), [1, 2]);
        let got = s.events_before_in_session("s1", all[2].id, 1).unwrap();
        assert_eq!(got.iter().map(|e| e.line).collect::<Vec<_>>(), [2]);
        assert!(
            s.events_before_in_session("other", 99, 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn all_memories_filters_by_status() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.insert(&mem("alpha"), &prov(Origin::Backfill, 1)).unwrap();
        s.insert(&mem("beta"), &prov(Origin::Backfill, 2)).unwrap();
        s.archive(a.uid()).unwrap();
        let active = s.all_memories(&[Status::Active]).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].text, "beta");
        assert_eq!(s.all_memories(&[Status::Archived]).unwrap().len(), 1);
        assert!(s.all_memories(&[]).unwrap().is_empty());
    }

    #[test]
    fn checkpoint_defaults_then_upserts() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(s.extract_checkpoint().unwrap(), (0, 0));
        s.set_extract_checkpoint(7, 1).unwrap();
        assert_eq!(s.extract_checkpoint().unwrap(), (7, 1));
        s.set_extract_checkpoint(9, 2).unwrap();
        assert_eq!(s.extract_checkpoint().unwrap(), (9, 2));
        assert!(matches!(
            s.set_extract_checkpoint(-1, 1),
            Err(StoreError::Invalid(_))
        ));
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

    fn prov(origin: Origin, line: i64) -> Provenance {
        let mut p = Provenance::new(origin);
        p.session_id = Some("s1".into());
        p.line = Some(line);
        p
    }

    #[test]
    fn archives_only_backfill_only_memories() {
        let mut s = Store::open_in_memory().unwrap();
        let only = s
            .insert(&mem("only backfill"), &prov(Origin::Backfill, 1))
            .unwrap();
        let mixed = s.insert(&mem("mixed"), &prov(Origin::Backfill, 1)).unwrap();
        s.insert(&mem("mixed"), &prov(Origin::Remember, 2)).unwrap();
        let manual = s
            .insert(&mem("manual"), &prov(Origin::Remember, 1))
            .unwrap();

        assert_eq!(s.archive_backfill_only_memories().unwrap(), 1);
        let status = |s: &Store, uid: &str| s.get(uid).unwrap().unwrap().status;
        assert_eq!(status(&s, only.uid()), Status::Archived);
        assert_eq!(status(&s, mixed.uid()), Status::Active);
        assert_eq!(status(&s, manual.uid()), Status::Active);
        // Reversible and idempotent: row still exists, second run is a no-op.
        assert_eq!(s.archive_backfill_only_memories().unwrap(), 0);
    }
}
