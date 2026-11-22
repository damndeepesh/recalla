//! Anchor queries for staleness checking.

use rusqlite::{TransactionBehavior, params};

use super::{Store, StoreError, require, touch};
use crate::model::Anchor;

/// One anchor with the memory it belongs to and why it is stale, if it is.
pub type AnchorRow = (String, i64, Anchor, Option<String>);

impl Store {
    /// Anchors of live (active or `needs_reverify`) memories scoped to
    /// project or module `key`, as `(memory uid, anchor id, anchor,
    /// stale_reason)`, ordered by memory then anchor.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn anchors_for_project(&self, key: &str) -> Result<Vec<AnchorRow>, StoreError> {
        let rows = self
            .conn
            .prepare_cached(
                "SELECT m.uid, a.id, a.path, a.symbol, a.lang, a.content_hash, a.git_commit,
                        a.stale_reason
                 FROM anchors a JOIN memories m ON m.id = a.memory_id
                 WHERE m.scope_project = ?1 AND m.status IN ('active', 'needs_reverify')
                 ORDER BY m.id, a.id",
            )?
            .query_map([key], |r| {
                let anchor = Anchor {
                    path: r.get(2)?,
                    symbol: r.get(3)?,
                    lang: r.get(4)?,
                    content_hash: r.get(5)?,
                    git_commit: r.get(6)?,
                };
                Ok((r.get(0)?, r.get(1)?, anchor, r.get(7)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Replaces all of a memory's anchors with `anchors` (fresh, not stale),
    /// atomically. Duplicate path+symbol entries collapse to one.
    ///
    /// # Errors
    /// `NotFound` if the memory doesn't exist.
    pub fn replace_anchors(&mut self, uid: &str, anchors: &[Anchor]) -> Result<(), StoreError> {
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = require(&tx, uid)?;
        touch(&tx, id, now)?;
        tx.execute("DELETE FROM anchors WHERE memory_id = ?1", [id])?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO anchors
                     (memory_id, path, symbol, lang, content_hash, git_commit)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for a in anchors {
                insert.execute(params![
                    id,
                    a.path,
                    a.symbol,
                    a.lang,
                    a.content_hash,
                    a.git_commit
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Records why an anchor is stale.
    ///
    /// # Errors
    /// `NotFound` if the anchor id doesn't exist.
    pub fn mark_stale(&mut self, anchor_id: i64, reason: &str) -> Result<(), StoreError> {
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE anchors SET stale_reason = ?2 WHERE id = ?1",
            params![anchor_id, reason],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("anchor {anchor_id}")));
        }
        tx.execute(
            "UPDATE memories SET updated_at = ?2
             WHERE id = (SELECT memory_id FROM anchors WHERE id = ?1)",
            params![anchor_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Clears the stale reason on all of a memory's anchors.
    ///
    /// # Errors
    /// `NotFound` if the memory doesn't exist.
    pub fn clear_stale(&mut self, uid: &str) -> Result<(), StoreError> {
        let now = (self.clock)();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = require(&tx, uid)?;
        touch(&tx, id, now)?;
        tx.execute(
            "UPDATE anchors SET stale_reason = NULL WHERE memory_id = ?1",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, NewMemory, Origin, Provenance, Scope, Status};

    fn add(s: &mut Store, text: &str, scope: Scope) -> String {
        let m = NewMemory {
            kind: Kind::Fact,
            text: text.into(),
            scope,
            confidence: 0.5,
            importance: 1,
        };
        let prov = Provenance {
            line: Some(1),
            ..Provenance::new(Origin::Backfill)
        };
        s.insert(&m, &prov).unwrap().uid().to_owned()
    }

    fn sym(path: &str, symbol: &str, hash: &str) -> Anchor {
        Anchor {
            symbol: Some(symbol.into()),
            content_hash: Some(hash.into()),
            ..Anchor::new(path)
        }
    }

    #[test]
    fn replace_lists_marks_and_clears() {
        let mut s = Store::open_in_memory().unwrap();
        let a = add(&mut s, "a", Scope::Project("p".into()));
        s.replace_anchors(&a, &[sym("x.rs", "f", "h1"), Anchor::new("y.md")])
            .unwrap();
        let rows = s.anchors_for_project("p").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, a);
        assert_eq!(rows[0].2, sym("x.rs", "f", "h1"));
        assert_eq!(rows[0].3, None);

        s.mark_stale(rows[0].1, "symbol changed: f").unwrap();
        let rows = s.anchors_for_project("p").unwrap();
        assert_eq!(rows[0].3.as_deref(), Some("symbol changed: f"));
        assert_eq!(rows[1].3, None);
        s.clear_stale(&a).unwrap();
        assert!(
            s.anchors_for_project("p")
                .unwrap()
                .iter()
                .all(|r| r.3.is_none())
        );

        // Replacing drops old anchors and any stale reason; dups collapse.
        s.mark_stale(rows[0].1, "x").unwrap();
        s.replace_anchors(&a, &[sym("x.rs", "f", "h2"), sym("x.rs", "f", "h2")])
            .unwrap();
        let rows = s.anchors_for_project("p").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2.content_hash.as_deref(), Some("h2"));
        assert_eq!(rows[0].3, None);
        assert_eq!(s.get(&a).unwrap().unwrap().anchors.len(), 1);
    }

    #[test]
    fn filters_by_project_module_and_status() {
        let mut s = Store::open_in_memory().unwrap();
        let proj = add(&mut s, "proj", Scope::Project("p".into()));
        let module = add(
            &mut s,
            "module",
            Scope::Module {
                project: "p".into(),
                path: "src".into(),
            },
        );
        let other = add(&mut s, "other", Scope::Project("q".into()));
        let global = add(&mut s, "global", Scope::Global);
        let old = add(&mut s, "old", Scope::Project("p".into()));
        let reverify = add(&mut s, "rv", Scope::Project("p".into()));
        for uid in [&proj, &module, &other, &global, &old, &reverify] {
            s.replace_anchors(uid, &[Anchor::new("a.rs")]).unwrap();
        }
        s.set_status(&old, Status::Archived).unwrap();
        s.set_status(&reverify, Status::NeedsReverify).unwrap();

        let got: Vec<String> = s
            .anchors_for_project("p")
            .unwrap()
            .into_iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(got, vec![proj, module, reverify]);
        assert!(s.anchors_for_project("none").unwrap().is_empty());
    }

    #[test]
    fn unknown_ids_are_not_found() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(matches!(
            s.replace_anchors("nope", &[]),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            s.clear_stale("nope"),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            s.mark_stale(99, "r"),
            Err(StoreError::NotFound(_))
        ));
    }
}
