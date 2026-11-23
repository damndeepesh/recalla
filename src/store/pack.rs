//! Pack support (schema v4): candidate selection for a project, the
//! injection ledger, and the lookups behind `recalla why`.

use std::collections::HashMap;

use rusqlite::params;
use serde_json::Value;

use super::ingest::get_u64;
use super::{MEMORY_COLS, Store, StoreError, memory_from_row};
use crate::model::{Kind, Memory, Status};

/// Shortest uid prefix `memories_by_uid_prefix` accepts.
pub const MIN_PREFIX: usize = 4;
/// How many most-injected memories `injection_stats` reports.
const TOP_INJECTED: usize = 10;

/// Largest `pack_text` stored per injection.
pub const MAX_PACK_TEXT: usize = 64 * 1024;
const TRUNCATED: &str = "\n[truncated]";

fn cap_pack_text(text: &str) -> String {
    if text.len() <= MAX_PACK_TEXT {
        return text.to_owned();
    }
    let mut end = MAX_PACK_TEXT - TRUNCATED.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", &text[..end])
}

/// One ledger row to record (see [`Store::record_injection`]).
#[derive(Debug, Clone)]
pub struct NewInjection<'a> {
    pub project: Option<&'a str>,
    pub budget: usize,
    pub est_tokens: usize,
    pub memory_uids: &'a [String],
    pub via: Via,
    /// Harness session that received the pack (hook only).
    pub session_id: Option<&'a str>,
    /// The exact pack markdown that was output.
    pub pack_text: Option<&'a str>,
    /// Memories considered for the pack.
    pub candidate_count: Option<usize>,
    /// Estimated tokens had every candidate been injected.
    pub candidate_tokens: Option<usize>,
}

impl<'a> NewInjection<'a> {
    /// A row with only the original fields; the dashboard fields are unset.
    #[must_use]
    pub const fn new(
        project: Option<&'a str>,
        budget: usize,
        est_tokens: usize,
        memory_uids: &'a [String],
        via: Via,
    ) -> Self {
        Self {
            project,
            budget,
            est_tokens,
            memory_uids,
            via,
            session_id: None,
            pack_text: None,
            candidate_count: None,
            candidate_tokens: None,
        }
    }
}

/// What triggered an injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Cli,
    Hook,
    Mcp,
}

impl Via {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Hook => "hook",
            Self::Mcp => "mcp",
        }
    }
}

/// Injections for one project (`None` is the unscoped bucket).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInjections {
    pub project: Option<String>,
    pub injections: u64,
    pub est_tokens: u64,
}

/// Aggregate ledger numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InjectionStats {
    pub injections: u64,
    pub est_tokens: u64,
    /// Most injections first, then by project name.
    pub per_project: Vec<ProjectInjections>,
    /// `(uid, times injected)`, at most 10, most injected first, then by uid.
    pub top_memories: Vec<(String, u64)>,
}

impl Store {
    /// Memories a pack for `project` may draw from: every `Global` memory,
    /// plus that project's `Project` and `Module` memories when `project` is
    /// given. Ordered by id. Provenance and anchors are not loaded.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn memories_for_project(
        &self,
        project: Option<&str>,
        statuses: &[Status],
    ) -> Result<Vec<Memory>, StoreError> {
        // Statuses are a closed enum, so inlining their names is injection-safe.
        let statuses = statuses
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {MEMORY_COLS} FROM memories m
             WHERE m.status IN ({statuses})
               AND (m.scope_project IS NULL OR m.scope_project = ?1)
             ORDER BY m.id"
        );
        let mut memories: Vec<Memory> = self
            .conn
            .prepare(&sql)?
            .query_map([project], memory_from_row)?
            .collect::<Result<_, _>>()?;
        if memories.iter().any(|m| m.status == Status::NeedsReverify) {
            let reasons = self.stale_reasons(project)?;
            for m in &mut memories {
                m.stale_reason = reasons.get(&m.uid).cloned();
            }
        }
        Ok(memories)
    }

    /// First stale reason (lowest anchor id) per memory uid, for stale
    /// anchors of live memories visible to `project`.
    fn stale_reasons(&self, project: Option<&str>) -> Result<HashMap<String, String>, StoreError> {
        let mut out = HashMap::new();
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.uid, a.stale_reason FROM anchors a JOIN memories m ON m.id = a.memory_id
             WHERE a.stale_reason IS NOT NULL AND m.status = 'needs_reverify'
               AND (m.scope_project IS NULL OR m.scope_project = ?1)
             ORDER BY a.id DESC",
        )?;
        for row in stmt.query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (uid, reason): (String, String) = row?;
            out.insert(uid, reason); // DESC order: the lowest id wins last
        }
        Ok(out)
    }

    /// Appends a ledger row stamped with the store clock. `pack_text` is
    /// capped at [`MAX_PACK_TEXT`] bytes (cut at a char boundary, with a marker).
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn record_injection(&mut self, inj: &NewInjection<'_>) -> Result<(), StoreError> {
        let uids = Value::from(inj.memory_uids).to_string();
        let to_i64 = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        let text = inj.pack_text.map(cap_pack_text);
        self.conn.execute(
            "INSERT INTO injections (at, project, budget, est_tokens, memory_uids, via,
                 session_id, pack_text, candidate_count, candidate_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                (self.clock)(),
                inj.project,
                to_i64(inj.budget),
                to_i64(inj.est_tokens),
                uids,
                inj.via.as_str(),
                inj.session_id,
                text,
                inj.candidate_count.map(to_i64),
                inj.candidate_tokens.map(to_i64)
            ],
        )?;
        Ok(())
    }

    /// Ledger totals, per-project breakdown and the most-injected memories.
    ///
    /// # Errors
    /// `Sqlite` on failure, `Invalid` if a stored uid list isn't a JSON array
    /// of strings (we write it ourselves, so that means corruption).
    pub fn injection_stats(&self) -> Result<InjectionStats, StoreError> {
        let mut stats = InjectionStats::default();
        let mut per_project: HashMap<Option<String>, (u64, u64)> = HashMap::new();
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT project, est_tokens, memory_uids FROM injections")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                get_u64(r, 1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (project, tokens, raw) = row?;
            let uids: Vec<String> = serde_json::from_str(&raw)
                .map_err(|e| StoreError::Invalid(format!("injections.memory_uids: {e}")))?;
            stats.injections += 1;
            stats.est_tokens += tokens;
            let entry = per_project.entry(project).or_default();
            entry.0 += 1;
            entry.1 += tokens;
            for uid in uids {
                *counts.entry(uid).or_default() += 1;
            }
        }
        stats.per_project = per_project
            .into_iter()
            .map(|(project, (injections, est_tokens))| ProjectInjections {
                project,
                injections,
                est_tokens,
            })
            .collect();
        stats.per_project.sort_by(|a, b| {
            b.injections
                .cmp(&a.injections)
                .then(a.project.cmp(&b.project))
        });
        let mut top: Vec<_> = counts.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        top.truncate(TOP_INJECTED);
        stats.top_memories = top;
        Ok(stats)
    }

    /// Memories (any status, with provenance and anchors) whose uid starts
    /// with `prefix`, ordered by uid.
    ///
    /// # Errors
    /// `Invalid` unless `prefix` is at least 4 hex characters; `Sqlite` on
    /// failure.
    pub fn memories_by_uid_prefix(&self, prefix: &str) -> Result<Vec<Memory>, StoreError> {
        if prefix.len() < MIN_PREFIX || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(StoreError::Invalid(format!(
                "id prefix must be at least {MIN_PREFIX} hex characters"
            )));
        }
        let sql = format!(
            "SELECT {MEMORY_COLS} FROM memories m
             WHERE m.uid LIKE ?1 || '%' ORDER BY m.uid"
        );
        let mut found = self
            .conn
            .prepare(&sql)?
            .query_map([prefix.to_ascii_lowercase()], memory_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        for memory in &mut found {
            self.load_children(memory)?;
        }
        Ok(found)
    }

    /// Stored (redacted) text of every event row at `line` of `source_path`,
    /// in `seq` order. Empty when the source or line is unknown.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn event_text_at(&self, source_path: &str, line: i64) -> Result<Vec<String>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT e.text FROM events e JOIN sources s ON s.id = e.source_id
                 WHERE s.path = ?1 AND e.line = ?2 ORDER BY e.seq",
            )?
            .query_map(params![source_path, line], |r| r.get(0))?
            .collect::<Result<_, _>>()?)
    }

    /// Memory counts by kind and status (non-zero only), sorted by kind then
    /// status.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn memory_counts(&self) -> Result<Vec<(Kind, Status, u64)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, status, count(*) FROM memories GROUP BY kind, status
             ORDER BY kind, status",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                super::parse_col(r, 0)?,
                super::parse_col(r, 1)?,
                get_u64(r, 2)?,
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewMemory, Origin, Provenance, Scope};
    use crate::store::{Checkpoint, StoredEvent, StoredKind};

    fn add(s: &mut Store, kind: Kind, scope: Scope, text: &str) -> String {
        let new = NewMemory {
            kind,
            text: text.into(),
            scope,
            confidence: 0.8,
            importance: 1,
        };
        s.insert(&new, &Provenance::new(Origin::Remember))
            .unwrap()
            .uid()
            .to_owned()
    }

    fn project(p: &str) -> Scope {
        Scope::Project(p.into())
    }

    #[test]
    fn project_union_includes_global_project_and_module_only() {
        let mut s = Store::open_in_memory().unwrap();
        add(&mut s, Kind::Fact, Scope::Global, "global one");
        add(&mut s, Kind::Fact, project("/a"), "in a");
        let module = Scope::Module {
            project: "/a".into(),
            path: "src".into(),
        };
        add(&mut s, Kind::Fact, module, "module of a");
        add(&mut s, Kind::Fact, project("/b"), "in b");
        let archived = add(&mut s, Kind::Fact, project("/a"), "old a");
        s.archive(&archived).unwrap();

        let texts = |key: Option<&str>| -> Vec<String> {
            s.memories_for_project(key, &[Status::Active, Status::NeedsReverify])
                .unwrap()
                .into_iter()
                .map(|m| m.text)
                .collect()
        };
        assert_eq!(texts(Some("/a")), ["global one", "in a", "module of a"]);
        assert_eq!(texts(None), ["global one"]);
        assert!(s.memories_for_project(Some("/a"), &[]).unwrap().is_empty());
        let all = s
            .memories_for_project(Some("/a"), &[Status::Archived])
            .unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn ledger_records_and_aggregates() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(s.injection_stats().unwrap(), InjectionStats::default());
        let u = |x: &str| x.to_owned();
        s.record_injection(&NewInjection::new(
            Some("/a"),
            1500,
            100,
            &[u("aa"), u("bb")],
            Via::Cli,
        ))
        .unwrap();
        s.record_injection(&NewInjection::new(
            Some("/a"),
            1500,
            50,
            &[u("aa")],
            Via::Hook,
        ))
        .unwrap();
        s.record_injection(&NewInjection::new(None, 800, 10, &[], Via::Mcp))
            .unwrap();
        s.record_injection(&NewInjection::new(Some("/b"), 800, 5, &[u("cc")], Via::Cli))
            .unwrap();

        let st = s.injection_stats().unwrap();
        assert_eq!((st.injections, st.est_tokens), (4, 165));
        let rows: Vec<_> = st
            .per_project
            .iter()
            .map(|p| (p.project.as_deref(), p.injections, p.est_tokens))
            .collect();
        assert_eq!(
            rows,
            [(Some("/a"), 2, 150), (None, 1, 10), (Some("/b"), 1, 5)]
        );
        assert_eq!(st.top_memories, [(u("aa"), 2), (u("bb"), 1), (u("cc"), 1)]);
    }

    #[test]
    fn dashboard_fields_are_stored_and_pack_text_is_capped() {
        let mut s = Store::open_in_memory().unwrap();
        let big = "é".repeat(MAX_PACK_TEXT);
        s.record_injection(&NewInjection {
            session_id: Some("sess"),
            pack_text: Some(&big),
            candidate_count: Some(3),
            candidate_tokens: Some(400),
            ..NewInjection::new(None, 1, 1, &[], Via::Hook)
        })
        .unwrap();
        let (sid, text, n, t): (String, String, i64, i64) = s
            .conn
            .query_row(
                "SELECT session_id, pack_text, candidate_count, candidate_tokens FROM injections",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((sid.as_str(), n, t), ("sess", 3, 400));
        assert!(
            text.len() <= MAX_PACK_TEXT && text.ends_with(TRUNCATED),
            "{}",
            text.len()
        );
        assert_eq!(cap_pack_text("short"), "short");
    }

    #[test]
    fn top_memories_capped_at_ten() {
        let mut s = Store::open_in_memory().unwrap();
        let uids: Vec<String> = (0..15).map(|i| format!("u{i:02}")).collect();
        s.record_injection(&NewInjection::new(None, 1, 1, &uids, Via::Cli))
            .unwrap();
        assert_eq!(s.injection_stats().unwrap().top_memories.len(), 10);
    }

    #[test]
    fn corrupt_ledger_row_is_an_error_not_silent() {
        let s = Store::open_in_memory().unwrap();
        s.conn
            .execute(
                "INSERT INTO injections (at, budget, est_tokens, memory_uids, via)
                 VALUES (0, 1, 1, 'not json', 'cli')",
                [],
            )
            .unwrap();
        assert!(matches!(s.injection_stats(), Err(StoreError::Invalid(_))));
    }

    #[test]
    fn uid_prefix_lookup() {
        let mut s = Store::open_in_memory().unwrap();
        let uid = add(&mut s, Kind::Gotcha, Scope::Global, "careful");
        let found = s.memories_by_uid_prefix(&uid[..6]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].uid, uid);
        assert_eq!(found[0].provenance.len(), 1);
        let upper = s.memories_by_uid_prefix(&uid[..6].to_uppercase()).unwrap();
        assert_eq!(upper.len(), 1);
        assert!(s.memories_by_uid_prefix("ffff0000ffff").unwrap().is_empty());
        for bad in ["", "abc", "zzzz", "ab%d", "abc_"] {
            assert!(
                matches!(s.memories_by_uid_prefix(bad), Err(StoreError::Invalid(_))),
                "{bad}"
            );
        }
    }

    fn event(line: u64, seq: u32, text: &str) -> StoredEvent {
        StoredEvent {
            harness: "claude-code".into(),
            session_id: "s1".into(),
            message_uuid: None,
            parent_uuid: None,
            line,
            seq,
            byte_offset: line * 10 + u64::from(seq),
            ts: None,
            cwd: None,
            git_branch: None,
            sidechain: false,
            kind: StoredKind::UserText,
            tool_name: None,
            tool_id: None,
            is_error: false,
            text: text.into(),
        }
    }

    #[test]
    fn event_text_at_returns_all_seq_rows_in_order() {
        let mut s = Store::open_in_memory().unwrap();
        let id = s.upsert_source("claude-code", "/s/one.jsonl").unwrap();
        let cp = Checkpoint {
            identity: "i".into(),
            tail_hash: "t".into(),
            size: 99,
            next_offset: 99,
        };
        let batch = [
            event(3, 1, "second"),
            event(3, 0, "first"),
            event(4, 0, "other line"),
        ];
        s.ingest_batch(id, &batch, &cp, None, false).unwrap();
        assert_eq!(
            s.event_text_at("/s/one.jsonl", 3).unwrap(),
            ["first", "second"]
        );
        assert!(s.event_text_at("/s/one.jsonl", 9).unwrap().is_empty());
        assert!(s.event_text_at("/nope", 3).unwrap().is_empty());
    }

    #[test]
    fn memory_counts_group_by_kind_and_status() {
        let mut s = Store::open_in_memory().unwrap();
        add(&mut s, Kind::Fact, Scope::Global, "a");
        add(&mut s, Kind::Fact, Scope::Global, "b");
        let c = add(&mut s, Kind::Gotcha, Scope::Global, "c");
        s.set_status(&c, Status::NeedsReverify).unwrap();
        assert_eq!(
            s.memory_counts().unwrap(),
            [
                (Kind::Fact, Status::Active, 2),
                (Kind::Gotcha, Status::NeedsReverify, 1)
            ]
        );
    }
}
