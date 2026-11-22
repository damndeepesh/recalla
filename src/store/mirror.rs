//! Markdown mirror support (schema v7): full-fidelity memory reads for
//! rendering, the sync dirty set, and the raw restore used by
//! `recalla rebuild --from-mirror`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use super::{MEMORY_COLS, Store, StoreError, memory_from_row, parse_col};
use crate::model::{self, Anchor, Memory, Origin, Provenance};

/// A memory with everything the mirror file carries: its provenance and
/// anchors, plus the columns the model leaves out.
#[derive(Debug, Clone, PartialEq)]
pub struct FullMemory {
    /// With `provenance` and `anchors` loaded; `stale_reason` is unused.
    pub memory: Memory,
    /// `provenance.created_at` for each entry of `memory.provenance`.
    pub provenance_at: Vec<i64>,
    /// `anchors.stale_reason` for each entry of `memory.anchors`.
    pub stale: Vec<Option<String>>,
}

/// A consistent snapshot for one mirror sync: the dirty marks it covers and
/// the memories to render.
#[derive(Debug, Clone, Default)]
pub struct MirrorBatch {
    /// `mirror_dirty.seq` of every mark in the snapshot. Pass them to
    /// [`Store::clear_mirror_marks`] after the files are written.
    pub marks: Vec<i64>,
    /// Dirty memories (every memory when the batch is a full one).
    pub docs: Vec<FullMemory>,
}

/// SQL conditions selecting memories (alias `m`) for a batch.
const ALL: &str = "1";
const DIRTY: &str = "m.id IN (SELECT memory_id FROM mirror_dirty)";

impl Store {
    /// Reads, in one snapshot, the dirty marks and the memories to render
    /// (all of them when `all`), oldest first, with provenance, anchors and
    /// stale reasons. Changes made after the snapshot stay dirty.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn mirror_batch(&mut self, all: bool) -> Result<MirrorBatch, StoreError> {
        let tx = self.conn.transaction()?;
        let marks = tx
            .prepare("SELECT seq FROM mirror_dirty")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let docs = load_full(&tx, if all { ALL } else { DIRTY })?;
        Ok(MirrorBatch { marks, docs })
    }

    /// Clears exactly these marks. A memory changed since the snapshot has a
    /// newer mark and stays dirty.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn clear_mirror_marks(&mut self, marks: &[i64]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        {
            let mut del = tx.prepare("DELETE FROM mirror_dirty WHERE seq = ?1")?;
            for m in marks {
                del.execute([m])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// How many memories the mirror still has to look at.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn mirror_dirty_count(&self) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM mirror_dirty", [], |r| r.get(0))?;
        Ok(n.unsigned_abs())
    }

    /// Uids of every memory in any status.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn memory_uids(&self) -> Result<HashSet<String>, StoreError> {
        Ok(self
            .conn
            .prepare("SELECT uid FROM memories")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?)
    }

    /// Writes a consistent copy of the database to `target` with
    /// `VACUUM INTO`. `target` must not exist or be an empty file.
    ///
    /// # Errors
    /// `Sqlite` on failure (including a non-empty `target`).
    pub fn backup_into(&self, target: &Path) -> Result<(), StoreError> {
        let t = target
            .to_str()
            .ok_or_else(|| StoreError::Invalid("backup path is not UTF-8".into()))?;
        self.conn.execute("VACUUM INTO ?1", [t])?;
        Ok(())
    }

    /// `(uid, kind, text)` of every memory whose uid is not in `keep`, for
    /// telling the user what a rebuild would drop.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn memories_not_in(
        &self,
        keep: &HashSet<String>,
    ) -> Result<Vec<(String, String, String)>, StoreError> {
        let rows = self
            .conn
            .prepare("SELECT uid, kind, text FROM memories ORDER BY id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<(String, String, String)>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|(u, _, _)| !keep.contains(u))
            .collect())
    }

    /// Replaces the whole memory layer (memories, provenance, anchors,
    /// relations) with `docs`, preserving every field, in one transaction.
    /// Events, sources, segments and the injection ledger are untouched, and
    /// the dirty triggers mark every restored memory for the next mirror sync.
    /// Runs as one `BEGIN IMMEDIATE` transaction on the live database, so other
    /// processes just see an ordinary commit. Used only by
    /// `recalla rebuild --from-mirror`.
    ///
    /// # Errors
    /// `Invalid` if a memory's uid doesn't match its kind, scope and text;
    /// `Sqlite` for anything else (duplicate uid, dangling `superseded_by`).
    /// Nothing changes on error.
    pub fn replace_memory_layer(&mut self, docs: &[FullMemory]) -> Result<(), StoreError> {
        let mut order: Vec<&FullMemory> = docs.iter().collect();
        order.sort_by(|a, b| {
            (a.memory.created_at, &a.memory.uid).cmp(&(b.memory.created_at, &b.memory.uid))
        });
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.pragma_update(None, "defer_foreign_keys", true)?;
        for table in ["relations", "provenance", "anchors", "memories"] {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
        }
        for doc in &order {
            restore_in(&tx, doc)?;
        }
        for doc in &order {
            let m = &doc.memory;
            if let Some(new) = &m.superseded_by {
                tx.execute(
                    "INSERT OR IGNORE INTO relations (src, dst, kind, created_at)
                     VALUES (?1, ?2, 'supersedes', ?3)",
                    params![new, m.uid, m.valid_to.unwrap_or(m.updated_at)],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// Every memory matching `cond` (a condition on alias `m`) in full, ordered
/// by `updated_at`, then id.
fn load_full(db: &Connection, cond: &str) -> Result<Vec<FullMemory>, StoreError> {
    let sql = format!(
        "SELECT {MEMORY_COLS}, m.id FROM memories m WHERE {cond} ORDER BY m.updated_at, m.id"
    );
    let mut ids = HashMap::new();
    let mut out: Vec<FullMemory> = Vec::new();
    for row in db
        .prepare(&sql)?
        .query_map([], |r| Ok((memory_from_row(r)?, r.get::<_, i64>(14)?)))?
    {
        let (memory, id) = row?;
        ids.insert(id, out.len());
        out.push(FullMemory {
            memory,
            provenance_at: Vec::new(),
            stale: Vec::new(),
        });
    }
    let mut prov = db.prepare(&format!(
        "SELECT p.memory_id, p.origin, p.harness, p.session_id, p.message_uuid,
                p.source_path, p.line, p.git_commit, p.created_at
         FROM provenance p JOIN memories m ON m.id = p.memory_id
         WHERE {cond} ORDER BY p.id"
    ))?;
    for row in prov.query_map([], |r| {
        let p = Provenance {
            origin: parse_col::<Origin>(r, 1)?,
            harness: r.get(2)?,
            session_id: r.get(3)?,
            message_uuid: r.get(4)?,
            source_path: r.get(5)?,
            line: r.get(6)?,
            git_commit: r.get(7)?,
        };
        Ok((r.get::<_, i64>(0)?, p, r.get::<_, i64>(8)?))
    })? {
        let (id, p, at) = row?;
        if let Some(&i) = ids.get(&id) {
            out[i].memory.provenance.push(p);
            out[i].provenance_at.push(at);
        }
    }
    let mut anchors = db.prepare(&format!(
        "SELECT a.memory_id, a.path, a.symbol, a.lang, a.content_hash, a.git_commit,
                a.stale_reason
         FROM anchors a JOIN memories m ON m.id = a.memory_id
         WHERE {cond} ORDER BY a.id"
    ))?;
    for row in anchors.query_map([], |r| {
        let a = Anchor {
            path: r.get(1)?,
            symbol: r.get(2)?,
            lang: r.get(3)?,
            content_hash: r.get(4)?,
            git_commit: r.get(5)?,
        };
        Ok((r.get::<_, i64>(0)?, a, r.get::<_, Option<String>>(6)?))
    })? {
        let (id, a, stale) = row?;
        if let Some(&i) = ids.get(&id) {
            out[i].memory.anchors.push(a);
            out[i].stale.push(stale);
        }
    }
    Ok(out)
}

/// Raw insert of one memory with its children, exactly as given.
fn restore_in(tx: &Transaction<'_>, doc: &FullMemory) -> Result<(), StoreError> {
    let m = &doc.memory;
    if model::uid(m.kind, &m.scope, &m.text) != m.uid {
        return Err(StoreError::Invalid(format!(
            "uid {} does not match the memory's kind, scope and text",
            m.uid
        )));
    }
    let (project, path) = m.scope.parts();
    tx.execute(
        "INSERT INTO memories (uid, kind, text, scope_key, scope_project, scope_path,
             confidence, importance, status, superseded_by, seen_count, created_at,
             updated_at, valid_from, valid_to)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            m.uid,
            m.kind.as_str(),
            m.text,
            m.scope.to_string(),
            project,
            path,
            m.confidence,
            m.importance,
            m.status.as_str(),
            m.superseded_by,
            m.seen_count,
            m.created_at,
            m.updated_at,
            m.valid_from,
            m.valid_to
        ],
    )?;
    let id = tx.last_insert_rowid();
    for (i, p) in m.provenance.iter().enumerate() {
        tx.execute(
            "INSERT INTO provenance (memory_id, origin, harness, session_id, message_uuid,
                 source_path, line, git_commit, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                p.origin.as_str(),
                p.harness,
                p.session_id,
                p.message_uuid,
                p.source_path,
                p.line,
                p.git_commit,
                doc.provenance_at.get(i).copied().unwrap_or(m.created_at)
            ],
        )?;
    }
    for (i, a) in m.anchors.iter().enumerate() {
        tx.execute(
            "INSERT INTO anchors (memory_id, path, symbol, lang, content_hash, git_commit,
                 stale_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                a.path,
                a.symbol,
                a.lang,
                a.content_hash,
                a.git_commit,
                doc.stale.get(i).cloned().flatten()
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, NewMemory, Scope, Status};

    type Step = fn(&mut Store, &str);

    fn add(s: &mut Store, text: &str) -> String {
        let m = NewMemory {
            kind: Kind::Fact,
            text: text.into(),
            scope: Scope::Project("/p".into()),
            confidence: 0.5,
            importance: 1,
        };
        let prov = Provenance {
            line: Some(1),
            ..Provenance::new(Origin::Backfill)
        };
        s.insert(&m, &prov).unwrap().uid().to_owned()
    }

    #[test]
    fn batch_holds_dirty_memories_with_children_until_cleared() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_clock(Box::new(|| 100));
        let a = add(&mut s, "a");
        let b = add(&mut s, "b");
        s.replace_anchors(&b, &[Anchor::new("x.rs")]).unwrap();
        let batch = s.mirror_batch(false).unwrap();
        assert_eq!(batch.docs.len(), 2);
        assert_eq!(batch.docs[0].memory.uid, a);
        let bdoc = &batch.docs[1];
        assert_eq!(bdoc.provenance_at, vec![100]);
        assert_eq!(bdoc.memory.anchors.len(), 1);
        assert_eq!(bdoc.stale, vec![None]);
        s.clear_mirror_marks(&batch.marks).unwrap();
        assert_eq!(s.mirror_dirty_count().unwrap(), 0);
        assert!(s.mirror_batch(false).unwrap().docs.is_empty());
        assert_eq!(s.mirror_batch(true).unwrap().docs.len(), 2);
        assert_eq!(s.memory_uids().unwrap().len(), 2);
    }

    #[test]
    fn a_change_after_the_snapshot_stays_dirty() {
        let mut s = Store::open_in_memory().unwrap();
        let a = add(&mut s, "a");
        let batch = s.mirror_batch(false).unwrap();
        // Still marked from its insert; this re-marks it with a newer seq.
        s.set_confidence(&a, 0.9).unwrap();
        s.clear_mirror_marks(&batch.marks).unwrap();
        let again = s.mirror_batch(false).unwrap();
        assert_eq!(again.docs.len(), 1);
        assert!((again.docs[0].memory.confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn anchor_and_status_changes_bump_updated_at() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_clock(Box::new(|| 100));
        let a = add(&mut s, "a");
        let at = |s: &Store| s.get(&a).unwrap().unwrap().updated_at;
        let steps: [(i64, Step); 6] = [
            (200, |s, a| s.add_anchor(a, &Anchor::new("x.rs")).unwrap()),
            (300, |s, a| {
                s.replace_anchors(a, &[Anchor::new("y.rs")]).unwrap();
            }),
            (400, |s, _| s.mark_stale(1, "gone").unwrap()),
            (500, |s, a| s.clear_stale(a).unwrap()),
            (600, |s, a| s.set_status(a, Status::Archived).unwrap()),
            (700, |s, a| s.set_confidence(a, 0.9).unwrap()),
        ];
        for (clock, step) in steps {
            s.set_clock(Box::new(move || clock));
            step(&mut s, &a);
            assert_eq!(at(&s), clock);
        }
    }

    #[test]
    fn supersede_bumps_the_old_memory() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_clock(Box::new(|| 100));
        let (a, b) = (add(&mut s, "a"), add(&mut s, "b"));
        s.set_clock(Box::new(|| 900));
        s.supersede(&a, &b).unwrap();
        assert_eq!(s.get(&a).unwrap().unwrap().updated_at, 900);
    }

    #[test]
    fn restore_refuses_a_uid_that_does_not_match() {
        let mut s = Store::open_in_memory().unwrap();
        let a = add(&mut s, "a");
        let mut doc = s.mirror_batch(true).unwrap().docs.remove(0);
        doc.memory.text = "tampered".into();
        let err = s.replace_memory_layer(&[doc]).unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)));
        // Nothing changed.
        assert!(s.get(&a).unwrap().is_some());
    }
}
