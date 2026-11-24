//! Memory relations (`supersedes`, `conflicts`) and the rule-memory scan the
//! contradiction detector needs.

use rusqlite::params;

use super::{MEMORY_COLS, Store, StoreError, memory_from_row, require};
use crate::model::Memory;

/// Relation kind: `src` (newer) contradicts `dst`, which was flagged.
pub const CONFLICTS: &str = "conflicts";

impl Store {
    /// Every live (`active` / `needs_reverify`) preference and decision, in
    /// insertion order. Provenance and anchors are not loaded.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn live_rules(&self) -> Result<Vec<Memory>, StoreError> {
        let sql = format!(
            "SELECT {MEMORY_COLS} FROM memories m
             WHERE m.status IN ('active', 'needs_reverify')
               AND m.kind IN ('preference', 'decision')
             ORDER BY m.id"
        );
        Ok(self
            .conn
            .prepare(&sql)?
            .query_map([], memory_from_row)?
            .collect::<Result<_, _>>()?)
    }

    /// Records `src -[kind]-> dst`. Idempotent.
    ///
    /// # Errors
    /// `NotFound` if either memory is missing.
    pub fn add_relation(&mut self, src: &str, dst: &str, kind: &str) -> Result<(), StoreError> {
        let now = (self.clock)();
        let tx = self.conn.transaction()?;
        require(&tx, src)?;
        require(&tx, dst)?;
        tx.execute(
            "INSERT OR IGNORE INTO relations (src, dst, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![src, dst, kind, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Relations touching `uid` as `(src, dst, kind)`, oldest first.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn relations_of(&self, uid: &str) -> Result<Vec<(String, String, String)>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT src, dst, kind FROM relations WHERE src = ?1 OR dst = ?1
                 ORDER BY created_at, src, dst, kind",
            )?
            .query_map([uid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?)
    }
}
