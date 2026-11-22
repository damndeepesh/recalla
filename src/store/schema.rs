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
    // v2: ingest checkpoints, redacted events, archive segments.
    "
    CREATE TABLE sources (
        id           INTEGER PRIMARY KEY,
        harness      TEXT NOT NULL,
        path         TEXT NOT NULL UNIQUE,
        identity     TEXT NOT NULL,
        tail_hash    TEXT NOT NULL DEFAULT '',
        size         INTEGER NOT NULL,
        next_offset  INTEGER NOT NULL,
        events       INTEGER NOT NULL DEFAULT 0,
        updated_at   INTEGER NOT NULL
    );

    CREATE TABLE events (
        id            INTEGER PRIMARY KEY,
        source_id     INTEGER NOT NULL REFERENCES sources(id),
        harness       TEXT NOT NULL,
        session_id    TEXT NOT NULL,
        message_uuid  TEXT,
        parent_uuid   TEXT,
        line          INTEGER NOT NULL,
        seq           INTEGER NOT NULL,
        byte_offset   INTEGER NOT NULL,
        ts            INTEGER,
        cwd           TEXT,
        git_branch    TEXT,
        sidechain     INTEGER NOT NULL,
        kind          TEXT NOT NULL
                      CHECK (kind IN ('user_text','assistant_text','tool_call','tool_result','summary')),
        tool_name     TEXT,
        tool_id       TEXT,
        is_error      INTEGER NOT NULL DEFAULT 0,
        text          TEXT NOT NULL,
        UNIQUE (source_id, line, seq)
    );
    CREATE INDEX events_session ON events (session_id);

    CREATE VIRTUAL TABLE events_fts USING fts5(
        text, content='events', content_rowid='id'
    );
    CREATE TRIGGER events_ai AFTER INSERT ON events BEGIN
        INSERT INTO events_fts (rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER events_ad AFTER DELETE ON events BEGIN
        INSERT INTO events_fts (events_fts, rowid, text)
        VALUES ('delete', old.id, old.text);
    END;

    CREATE TABLE segments (
        source_id     INTEGER NOT NULL REFERENCES sources(id),
        start_offset  INTEGER NOT NULL,
        end_offset    INTEGER NOT NULL CHECK (end_offset > start_offset),
        hash          TEXT NOT NULL,
        PRIMARY KEY (source_id, start_offset)
    );

    DROP INDEX provenance_source;
    CREATE UNIQUE INDEX provenance_source ON provenance (
        memory_id,
        COALESCE(harness, ''),
        COALESCE(session_id, ''),
        COALESCE(message_uuid, ''),
        COALESCE(source_path, ''),
        COALESCE(line, -1)
    );
    ",
    // v3: extractor checkpoint (single row).
    "
    CREATE TABLE extract_state (
        id                INTEGER PRIMARY KEY CHECK (id = 1),
        last_event_id     INTEGER NOT NULL DEFAULT 0,
        extractor_version INTEGER NOT NULL DEFAULT 0
    );
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
    migrate_to(conn, version())
}

/// Applies migrations up to `target` (tests build older schemas with it).
fn migrate_to(conn: &mut Connection, target: u32) -> Result<(), StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current > target {
        return Err(StoreError::Invalid(format!(
            "database schema v{current} is newer than this build (v{})",
            version()
        )));
    }
    for sql in &MIGRATIONS[current as usize..target as usize] {
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

    #[test]
    fn v1_data_survives_v2_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_to(&mut conn, 1).unwrap();
        assert_eq!(user_version(&conn), 1);
        conn.execute_batch(
            "INSERT INTO memories (uid, kind, text, scope_key, confidence, importance,
                 created_at, updated_at, valid_from)
             VALUES ('u1', 'fact', 'kept across v2', 'global', 0.5, 1, 1, 1, 1);
             INSERT INTO provenance (memory_id, origin, harness, session_id, source_path,
                 line, created_at)
             VALUES (1, 'backfill', 'claude-code', 's1', '/a.jsonl', 3, 1);",
        )
        .unwrap();

        migrate_to(&mut conn, 2).unwrap();
        assert_eq!(user_version(&conn), 2);
        let text: String = conn
            .query_row("SELECT text FROM memories WHERE uid = 'u1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(text, "kept across v2");
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'kept'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);

        // Same session and line from another file (a subagent log) is now a
        // distinct provenance; the exact same source is still deduped.
        let add = |path: &str| {
            conn.execute(
                "INSERT OR IGNORE INTO provenance (memory_id, origin, harness, session_id,
                     source_path, line, created_at)
                 VALUES (1, 'backfill', 'claude-code', 's1', ?1, 3, 2)",
                [path],
            )
            .unwrap()
        };
        assert_eq!(add("/a.jsonl"), 0);
        assert_eq!(add("/sub/agent.jsonl"), 1);
    }

    #[test]
    fn events_fts_follows_insert_and_delete() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO sources (harness, path, identity, size, next_offset, updated_at)
             VALUES ('h', '/p', 'i', 1, 1, 0);
             INSERT INTO events (source_id, harness, session_id, line, seq, byte_offset,
                 sidechain, kind, text)
             VALUES (1, 'h', 's', 1, 0, 0, 0, 'user_text', 'zebra crossing');",
        )
        .unwrap();
        let count = |c: &Connection| -> i64 {
            c.query_row(
                "SELECT count(*) FROM events_fts WHERE events_fts MATCH 'zebra'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count(&conn), 1);
        conn.execute("DELETE FROM events", []).unwrap();
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn v2_data_survives_v3_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_to(&mut conn, 2).unwrap();
        conn.execute_batch(
            "INSERT INTO sources (harness, path, identity, size, next_offset, updated_at)
             VALUES ('h', '/p', 'i', 1, 1, 0);
             INSERT INTO events (source_id, harness, session_id, line, seq, byte_offset,
                 sidechain, kind, text)
             VALUES (1, 'h', 's', 1, 0, 0, 0, 'user_text', 'kept across v3');",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn), 3);
        let text: String = conn
            .query_row("SELECT text FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(text, "kept across v3");
        let n: i64 = conn
            .query_row("SELECT count(*) FROM extract_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        assert!(
            conn.execute("INSERT INTO extract_state (id) VALUES (2)", [])
                .is_err()
        );
    }
}
