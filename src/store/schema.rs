//! Schema, pragmas and migrations. Hand-rolled: an ordered list of SQL
//! scripts, with `PRAGMA user_version` recording how many have been applied.

use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

use super::StoreError;

/// Ordered migrations. Append only: never edit a shipped entry.
const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE memories (
        id             INTEGER PRIMARY KEY,
        uid            TEXT NOT NULL UNIQUE,
        kind           TEXT NOT NULL,
        text           TEXT NOT NULL,
        scope_key      TEXT NOT NULL,
        scope_project  TEXT,
        scope_path     TEXT,
        confidence     REAL NOT NULL CHECK (confidence BETWEEN 0 AND 1),
        importance     INTEGER NOT NULL CHECK (importance BETWEEN 0 AND 3),
        status         TEXT NOT NULL DEFAULT 'active'
                       CHECK (status IN ('active','needs_reverify','superseded','archived')),
        superseded_by  TEXT REFERENCES memories(uid),
        seen_count     INTEGER NOT NULL DEFAULT 1,
        created_at     INTEGER NOT NULL,
        updated_at     INTEGER NOT NULL,
        valid_from     INTEGER NOT NULL,
        valid_to       INTEGER
    );
    CREATE INDEX memories_scope_status ON memories (scope_key, status);

    CREATE TABLE provenance (
        id            INTEGER PRIMARY KEY,
        memory_id     INTEGER NOT NULL REFERENCES memories(id),
        origin        TEXT NOT NULL,
        harness       TEXT,
        session_id    TEXT,
        message_uuid  TEXT,
        source_path   TEXT,
        line          INTEGER,
        git_commit    TEXT,
        created_at    INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX provenance_source ON provenance (
        memory_id,
        COALESCE(harness, ''),
        COALESCE(session_id, ''),
        COALESCE(message_uuid, ''),
        COALESCE(line, -1)
    );

    CREATE TABLE anchors (
        id            INTEGER PRIMARY KEY,
        memory_id     INTEGER NOT NULL REFERENCES memories(id),
        path          TEXT NOT NULL,
        symbol        TEXT,
        lang          TEXT,
        content_hash  TEXT,
        git_commit    TEXT
    );
    CREATE UNIQUE INDEX anchors_target ON anchors (memory_id, path, COALESCE(symbol, ''));

    CREATE TABLE relations (
        src         TEXT NOT NULL REFERENCES memories(uid),
        dst         TEXT NOT NULL REFERENCES memories(uid),
        kind        TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        PRIMARY KEY (src, dst, kind)
    );

    CREATE VIRTUAL TABLE memories_fts USING fts5(
        text, content='memories', content_rowid='id'
    );
    CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memories_fts (rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memories_fts (memories_fts, rowid, text)
        VALUES ('delete', old.id, old.text);
    END;
    CREATE TRIGGER memories_au AFTER UPDATE OF text ON memories BEGIN
        INSERT INTO memories_fts (memories_fts, rowid, text)
        VALUES ('delete', old.id, old.text);
        INSERT INTO memories_fts (rowid, text) VALUES (new.id, new.text);
    END;
    ",
];

/// Schema version this build produces.
#[must_use]
pub fn version() -> u32 {
    u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX)
}

/// Per-connection settings. WAL is skipped for in-memory databases.
pub(super) fn configure(conn: &Connection, wal: bool) -> Result<(), StoreError> {
    conn.busy_timeout(Duration::from_millis(5000))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    if wal {
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    }
    Ok(())
}

/// Applies pending migrations in one write transaction. Safe to call
/// repeatedly and from concurrent processes.
pub(super) fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let target = version();
    if current > target {
        return Err(StoreError::Invalid(format!(
            "database schema v{current} is newer than this build (v{target})"
        )));
    }
    for sql in &MIGRATIONS[current as usize..] {
        tx.execute_batch(sql)?;
    }
    if current < target {
        tx.pragma_update(None, "user_version", target)?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_version(conn: &Connection) -> u32 {
        conn.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn migrate_twice_is_a_noop() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn), version());
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn), version());
    }

    #[test]
    fn newer_database_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", version() + 1)
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(StoreError::Invalid(_))));
    }

    #[test]
    fn fts5_is_available() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM memories_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
