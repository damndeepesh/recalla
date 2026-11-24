//! Read-only access to other agents' own databases. Recalla's schema never
//! touches these; each is opened `SQLITE_OPEN_READ_ONLY` and only read.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::StoreError;

/// One opencode part with its message's role, in conversation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodePart {
    pub message_id: String,
    pub role: String,
    pub time_ms: i64,
    /// The part's JSON (`type`, `text` / `tool`, `callID`, `state`, ...).
    pub data: String,
}

/// An opencode session and its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeSession {
    pub id: String,
    pub directory: String,
    /// A subagent session (it has a parent).
    pub sidechain: bool,
    pub parts: Vec<OpencodePart>,
}

/// Every session in the opencode database at `db`.
///
/// # Errors
/// The file is missing or is not an opencode database (no `session`,
/// `message` or `part` table).
pub fn read_opencode(db: &Path) -> Result<Vec<OpencodeSession>, StoreError> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    let mut sessions: Vec<OpencodeSession> = conn
        .prepare(
            "SELECT id, directory, parent_id IS NOT NULL FROM session ORDER BY time_created, id",
        )?
        .query_map([], |r| {
            Ok(OpencodeSession {
                id: r.get(0)?,
                directory: r.get(1)?,
                sidechain: r.get(2)?,
                parts: Vec::new(),
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut parts = conn.prepare(
        "SELECT p.message_id, COALESCE(json_extract(m.data, '$.role'), ''), p.time_created, p.data
         FROM part p JOIN message m ON m.id = p.message_id
         WHERE p.session_id = ?1 ORDER BY p.time_created, p.id",
    )?;
    for s in &mut sessions {
        s.parts = parts
            .query_map([&s.id], |r| {
                Ok(OpencodePart {
                    message_id: r.get(0)?,
                    role: r.get(1)?,
                    time_ms: r.get(2)?,
                    data: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
    }
    Ok(sessions)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tiny opencode-shaped database for tests (synthetic content only).
    pub(crate) fn fixture_db(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL, parent_id TEXT, time_created INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
             INSERT INTO session VALUES ('ses_1', '/work/app', NULL, 1), ('ses_2', '/work/app', 'ses_1', 2);
             INSERT INTO message VALUES ('msg_1', 'ses_1', 10, '{\"role\":\"user\"}'), ('msg_2', 'ses_1', 20, '{\"role\":\"assistant\"}');
             INSERT INTO part VALUES
              ('prt_1', 'msg_1', 'ses_1', 10, '{\"type\":\"text\",\"text\":\"use pnpm, never npm\"}'),
              ('prt_2', 'msg_2', 'ses_1', 21, '{\"type\":\"reasoning\",\"text\":\"hmm\"}'),
              ('prt_3', 'msg_2', 'ses_1', 22, '{\"type\":\"tool\",\"tool\":\"bash\",\"callID\":\"c1\",\"state\":{\"status\":\"completed\",\"input\":{\"command\":\"pnpm test\"},\"output\":\"ok\"}}'),
              ('prt_4', 'msg_2', 'ses_1', 23, '{\"type\":\"tool\",\"tool\":\"bash\",\"callID\":\"c2\",\"state\":{\"status\":\"error\",\"input\":{\"command\":\"npm test\"},\"error\":\"npm: not allowed\"}}'),
              ('prt_5', 'msg_2', 'ses_1', 24, '{\"type\":\"tool\",\"tool\":\"edit\",\"callID\":\"c3\",\"state\":{\"status\":\"completed\",\"input\":{\"filePath\":\"/work/app/a.ts\",\"oldString\":\"x\",\"newString\":\"y\"},\"output\":\"done\"}}'),
              ('prt_6', 'msg_2', 'ses_1', 25, '{\"type\":\"text\",\"text\":\"Done. We use pnpm here.\"}');",
        )
        .unwrap();
    }

    #[test]
    fn reads_sessions_with_parts_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        fixture_db(&db);
        let s = read_opencode(&db).unwrap();
        assert_eq!(s.len(), 2);
        assert!(!s[0].sidechain && s[1].sidechain);
        assert_eq!(s[0].parts.len(), 6);
        assert_eq!(s[0].parts[0].role, "user");
        assert!(s[0].parts.windows(2).all(|w| w[0].time_ms <= w[1].time_ms));
    }

    #[test]
    fn a_non_opencode_database_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("x.db");
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t(a)")
            .unwrap();
        assert!(read_opencode(&db).is_err());
        assert!(read_opencode(&dir.path().join("missing.db")).is_err());
    }
}
