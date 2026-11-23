//! Read-only queries behind the dashboard (`recalla ui`).
//!
//! Every query is parameterized: user input is only ever bound, never
//! formatted into SQL, and `ORDER BY` columns come from closed enums. Lists
//! use keyset pagination with an opaque [`Cursor`]; `limit` is clamped to
//! `1..=200`. Text that came from agent logs is sanitized (control and
//! invisible characters removed) and file paths are reduced to basenames.
//!
//! A *session* is a distinct `(harness, session_id)` pair in `events`.

use std::path::Path;
use std::str::FromStr;

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, Row, params, params_from_iter};

use super::ingest::get_u64;
use super::search::fts_query;
use super::{Store, StoreError, parse_col};
use crate::model::{Kind, Status};
use crate::pack::one_line;
use crate::ui::security::{sanitize, sanitize_multiline};

/// Page size bounds.
pub const MAX_LIMIT: usize = 200;
/// Chars of a memory's text in list rows and briefs.
const LIST_TEXT_CHARS: usize = 400;
/// Chars of an event's text before it is cut (the rest loads lazily).
pub const EVENT_TEXT_CHARS: usize = 2000;
/// Chars of the transcript excerpt shown per provenance row.
const EXCERPT_CHARS: usize = 300;
/// Most injections listed on a memory, and most memories on a session.
const DETAIL_INJECTIONS: usize = 20;
const DETAIL_MEMORIES: usize = 200;
/// Most source file names listed on a session row.
const SESSION_SOURCES: usize = 3;
/// Raw bytes fetched for a memory's list text (it is flattened and cut after).
const RAW_TEXT: i64 = 2000;
const MAX_CURSOR: usize = 128;

/// Opaque position in a list. Obtain it from [`Page::next`]; hand it back
/// unchanged. (For [`Store::ui_session_events`] it is the last event id.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(String);

impl Cursor {
    /// Wraps client-supplied text. Malformed cursors are rejected when used.
    ///
    /// # Errors
    /// `Invalid` if the text is empty, too long or not printable ASCII.
    pub fn parse(raw: &str) -> Result<Self, StoreError> {
        if raw.is_empty() || raw.len() > MAX_CURSOR || !raw.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(StoreError::Invalid("bad cursor".to_owned()));
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn pair<A: FromStr, B: FromStr>(&self) -> Result<(A, B), StoreError> {
        let bad = || StoreError::Invalid("bad cursor".to_owned());
        let (a, b) = self.0.split_once(':').ok_or_else(bad)?;
        Ok((a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?))
    }
}

/// One page of a list. `next` is `None` on the last page.
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub next: Option<Cursor>,
}

fn clamp(limit: usize) -> usize {
    limit.clamp(1, MAX_LIMIT)
}

/// Builds `rows` (at most `limit`) and the next cursor from `limit + 1`
/// fetched rows.
fn page<T>(mut rows: Vec<T>, limit: usize, key: impl Fn(&T) -> String) -> Page<T> {
    let next = (rows.len() > limit).then(|| {
        rows.truncate(limit);
        Cursor(rows.last().map(&key).unwrap_or_default())
    });
    Page { rows, next }
}

/// Bound parameters for one dynamic statement.
#[derive(Default)]
struct Params(Vec<Value>);

impl Params {
    /// Adds `v` and returns its `?N` placeholder.
    fn bind(&mut self, v: impl Into<Value>) -> String {
        self.0.push(v.into());
        format!("?{}", self.0.len())
    }

    fn text(&mut self, s: &str) -> String {
        self.bind(s.to_owned())
    }

    fn bind_all(&mut self, vs: impl Iterator<Item = String>) -> String {
        vs.map(|v| self.bind(v)).collect::<Vec<_>>().join(", ")
    }

    fn limit(&mut self, limit: usize) -> String {
        self.bind(i64::try_from(limit + 1).unwrap_or(i64::MAX))
    }
}

fn base_name(path: &str) -> String {
    sanitize(
        &Path::new(path)
            .file_name()
            .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned()),
    )
}

fn opt_clean(s: Option<String>) -> Option<String> {
    s.map(|s| sanitize(&s))
}

/// `1 - est_tokens / candidate_tokens` as a percentage; `None` without a baseline.
#[must_use]
#[allow(clippy::cast_precision_loss)] // token counts are far below 2^52
pub fn saved_pct(est_tokens: u64, candidate_tokens: Option<u64>) -> Option<f64> {
    candidate_tokens
        .filter(|c| *c > 0)
        .map(|c| (1.0 - est_tokens as f64 / c as f64) * 100.0)
}

// ---------------------------------------------------------------- overview

/// Headline numbers for the dashboard.
#[derive(Debug, Clone)]
pub struct Overview {
    /// `(kind, status, count)`, non-zero only.
    pub memories: Vec<(Kind, Status, u64)>,
    /// Memories with status `needs_reverify`.
    pub stale: u64,
    pub sessions: u64,
    pub events: u64,
    pub sources: u64,
    pub injections: u64,
    /// Sum of `est_tokens` over all injections.
    pub injected_tokens: u64,
    /// Sum of `est_tokens` over injections that recorded a baseline (the
    /// numerator that is comparable with `candidate_tokens`).
    pub baselined_tokens: u64,
    /// Sum of `candidate_tokens` over injections that recorded a baseline.
    pub candidate_tokens: u64,
    pub last_event_ts: Option<i64>,
    pub last_injection_at: Option<i64>,
    pub last_injection_via: Option<String>,
}

/// One UTC day of activity for the dashboard's charts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DayActivity {
    /// Start of the UTC day, unix ms.
    pub day: i64,
    /// Distinct sessions with at least one event that day.
    pub sessions: u64,
    /// Memories created that day.
    pub memories: u64,
    /// Context packs served that day.
    pub packs: u64,
    /// Tokens in those packs.
    pub tokens: u64,
}

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;

// ---------------------------------------------------------------- sessions

#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    pub harness: Option<String>,
    /// Exact project (the cwd of the session's first event).
    pub project: Option<String>,
    /// Case-insensitive substring of the session id or project.
    pub q: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub harness: String,
    pub session_id: String,
    /// The cwd of the session's earliest event.
    pub project: Option<String>,
    /// Id of the earliest event.
    pub first_event_id: i64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub event_count: u64,
    pub sidechain_event_count: u64,
    /// Distinct memories whose provenance names this session.
    pub memory_count: u64,
    pub injection_count: u64,
    /// Distinct source file basenames, at most 3.
    pub sources: Vec<String>,
}

/// A memory in a short listing.
#[derive(Debug, Clone)]
pub struct MemoryBrief {
    pub uid: String,
    pub kind: Kind,
    pub text: String,
    pub status: Status,
}

#[derive(Debug, Clone)]
pub struct InjectionBrief {
    pub id: i64,
    pub at: i64,
    pub est_tokens: u64,
    pub via: String,
    pub session_id: Option<String>,
    /// See the harness derivation in `INJECTION_COLS`.
    pub harness: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionDetail {
    pub session: SessionRow,
    /// Memories extracted from this session (at most 200).
    pub memories: Vec<MemoryBrief>,
    /// Injections that session received, newest first.
    pub injections: Vec<InjectionBrief>,
}

#[derive(Debug, Clone)]
pub struct SessionEvent {
    pub id: i64,
    pub ts: Option<i64>,
    pub kind: String,
    pub tool_name: Option<String>,
    pub is_error: bool,
    pub sidechain: bool,
    /// Sanitized, newlines kept, at most 2000 chars.
    pub text: String,
    /// The stored text is longer; fetch it with [`Store::ui_event_text`].
    pub truncated: bool,
}

// ---------------------------------------------------------------- memories

/// Sort order for [`Store::ui_memories`]; always descending, ties by id.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemorySort {
    #[default]
    Updated,
    Confidence,
    Seen,
    Created,
}

impl MemorySort {
    const fn column(self) -> &'static str {
        match self {
            Self::Updated => "m.updated_at",
            Self::Confidence => "m.confidence",
            Self::Seen => "m.seen_count",
            Self::Created => "m.created_at",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryFilter {
    /// Empty allows all kinds.
    pub kinds: Vec<Kind>,
    /// Empty means active and `needs_reverify`.
    pub statuses: Vec<Status>,
    /// Exact `scope_project`.
    pub project: Option<String>,
    /// Memories with provenance from this harness.
    pub harness: Option<String>,
    /// Full-text query over memory text (blank is ignored).
    pub q: Option<String>,
    pub sort: MemorySort,
}

#[derive(Debug, Clone)]
pub struct SourceRef {
    pub harness: Option<String>,
    pub session_id: Option<String>,
    /// Source file basename.
    pub file: Option<String>,
    pub line: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct MemoryRow {
    pub uid: String,
    pub kind: Kind,
    /// One line, at most 400 chars.
    pub text: String,
    pub scope_project: Option<String>,
    pub scope_path: Option<String>,
    pub status: Status,
    pub confidence: f32,
    pub importance: i64,
    pub seen_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub anchor_count: u64,
    /// First stale reason among the memory's anchors.
    pub stale_reason: Option<String>,
    pub first_source: Option<SourceRef>,
}

#[derive(Debug, Clone)]
pub struct ProvenanceView {
    pub origin: String,
    pub source: SourceRef,
    pub git_commit: Option<String>,
    /// Start of the source event's text (at most 300 chars), if still stored.
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnchorView {
    pub path: String,
    pub symbol: Option<String>,
    pub stale_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MemoryDetail {
    pub row: MemoryRow,
    /// Full text, newlines kept.
    pub text_full: String,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub superseded_by: Option<String>,
    pub provenance: Vec<ProvenanceView>,
    pub anchors: Vec<AnchorView>,
    /// Newest injections that included this memory (at most 20).
    pub injections: Vec<InjectionBrief>,
}

// -------------------------------------------------------------- injections

#[derive(Debug, Clone, Default)]
pub struct InjectionFilter {
    pub project: Option<String>,
    /// `cli`, `hook` or `mcp`.
    pub via: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InjectionRow {
    pub id: i64,
    pub at: i64,
    pub project: Option<String>,
    pub via: String,
    pub session_id: Option<String>,
    /// See the harness derivation in `INJECTION_COLS`.
    pub harness: Option<String>,
    pub budget: u64,
    pub est_tokens: u64,
    pub candidate_count: Option<u64>,
    pub candidate_tokens: Option<u64>,
    /// See [`saved_pct`].
    pub saved_pct: Option<f64>,
    pub memory_count: u64,
}

/// A project key with its live (active or needs-reverify) memory count.
#[derive(Debug, Clone)]
pub struct ProjectCount {
    pub project: String,
    pub memory_count: u64,
}

#[derive(Debug, Clone)]
pub struct InjectionDetail {
    pub row: InjectionRow,
    /// Exact pack text, newlines kept (`None` for rows from before v8).
    pub pack_text: Option<String>,
    /// Injected memories that still exist, in pack order, as full list rows
    /// (scope, confidence, seen count, freshness, first source) so the
    /// dashboard can explain why each was included.
    pub memories: Vec<MemoryRow>,
}

/// Injection columns. The last one is the harness of the injection's session,
/// derived from its events: set only when the session id appears under
/// exactly one harness (injections record none themselves), otherwise NULL.
const INJECTION_COLS: &str = "i.id, i.at, i.project, i.via, i.session_id, i.budget, i.est_tokens,
    i.candidate_count, i.candidate_tokens, json_array_length(i.memory_uids),
    (SELECT CASE WHEN min(harness) = max(harness) THEN min(harness) END
     FROM events WHERE session_id = i.session_id)";

fn injection_row(r: &Row<'_>) -> rusqlite::Result<InjectionRow> {
    let est_tokens = get_u64(r, 6)?;
    let candidate_tokens = r
        .get::<_, Option<i64>>(8)?
        .map(|v| u64::try_from(v).unwrap_or(0));
    Ok(InjectionRow {
        id: r.get(0)?,
        at: r.get(1)?,
        project: opt_clean(r.get(2)?),
        via: sanitize(&r.get::<_, String>(3)?),
        session_id: opt_clean(r.get(4)?),
        budget: get_u64(r, 5)?,
        est_tokens,
        candidate_count: r
            .get::<_, Option<i64>>(7)?
            .map(|v| u64::try_from(v).unwrap_or(0)),
        candidate_tokens,
        saved_pct: saved_pct(est_tokens, candidate_tokens),
        memory_count: get_u64(r, 9)?,
        harness: opt_clean(r.get(10)?),
    })
}

fn injection_brief(r: &Row<'_>) -> rusqlite::Result<InjectionBrief> {
    Ok(InjectionBrief {
        id: r.get(0)?,
        at: r.get(1)?,
        est_tokens: get_u64(r, 2)?,
        via: sanitize(&r.get::<_, String>(3)?),
        session_id: opt_clean(r.get(4)?),
        harness: opt_clean(r.get(5)?),
    })
}

fn memory_brief(r: &Row<'_>) -> rusqlite::Result<MemoryBrief> {
    Ok(MemoryBrief {
        uid: r.get(0)?,
        kind: parse_col(r, 1)?,
        text: one_line(&r.get::<_, String>(2)?, LIST_TEXT_CHARS),
        status: parse_col(r, 3)?,
    })
}

const MEMORY_ROW_COLS: &str = "m.id, m.uid, m.kind, substr(m.text, 1, ?RAW), m.scope_project,
    m.scope_path, m.status, m.confidence, m.importance, m.seen_count, m.created_at, m.updated_at,
    (SELECT count(*) FROM anchors a WHERE a.memory_id = m.id),
    (SELECT a.stale_reason FROM anchors a
      WHERE a.memory_id = m.id AND a.stale_reason IS NOT NULL ORDER BY a.id LIMIT 1)";

/// Base row plus its memory id (for the follow-up provenance lookup).
fn memory_row(r: &Row<'_>) -> rusqlite::Result<(i64, MemoryRow)> {
    Ok((
        r.get(0)?,
        MemoryRow {
            uid: r.get(1)?,
            kind: parse_col(r, 2)?,
            text: one_line(&r.get::<_, String>(3)?, LIST_TEXT_CHARS),
            scope_project: opt_clean(r.get(4)?),
            scope_path: opt_clean(r.get(5)?),
            status: parse_col(r, 6)?,
            confidence: r.get(7)?,
            importance: r.get(8)?,
            seen_count: r.get(9)?,
            created_at: r.get(10)?,
            updated_at: r.get(11)?,
            anchor_count: get_u64(r, 12)?,
            stale_reason: opt_clean(r.get(13)?),
            first_source: None,
        },
    ))
}

fn memory_row_sql() -> String {
    MEMORY_ROW_COLS.replace("?RAW", &RAW_TEXT.to_string())
}

fn source_ref(r: &Row<'_>) -> rusqlite::Result<SourceRef> {
    Ok(SourceRef {
        harness: opt_clean(r.get(0)?),
        session_id: opt_clean(r.get(1)?),
        file: r.get::<_, Option<String>>(2)?.map(|p| base_name(&p)),
        line: r.get(3)?,
    })
}

impl Store {
    /// Counts and recency for the overview page.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn ui_overview(&self) -> Result<Overview, StoreError> {
        let memories = self.memory_counts()?;
        let stale = memories
            .iter()
            .filter(|(_, s, _)| *s == Status::NeedsReverify)
            .map(|(_, _, n)| n)
            .sum();
        let count = |sql: &str| -> Result<u64, StoreError> {
            Ok(self.conn.query_row(sql, [], |r| get_u64(r, 0))?)
        };
        let (injections, injected_tokens, baselined_tokens, candidate_tokens) =
            self.conn.query_row(
                "SELECT count(*), coalesce(sum(est_tokens), 0),
                    coalesce(sum(CASE WHEN candidate_tokens IS NOT NULL THEN est_tokens END), 0),
                    coalesce(sum(candidate_tokens), 0)
             FROM injections",
                [],
                |r| {
                    Ok((
                        get_u64(r, 0)?,
                        get_u64(r, 1)?,
                        get_u64(r, 2)?,
                        get_u64(r, 3)?,
                    ))
                },
            )?;
        let last_event_ts = self
            .conn
            .query_row("SELECT max(ts) FROM events", [], |r| r.get(0))?;
        let last: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT at, via FROM injections ORDER BY at DESC, id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(Overview {
            memories,
            stale,
            sessions: count(
                "SELECT count(*) FROM (SELECT DISTINCT harness, session_id FROM events)",
            )?,
            events: count("SELECT count(*) FROM events")?,
            sources: count("SELECT count(*) FROM sources")?,
            injections,
            injected_tokens,
            baselined_tokens,
            candidate_tokens,
            last_event_ts,
            last_injection_at: last.as_ref().map(|l| l.0),
            last_injection_via: last.map(|l| l.1),
        })
    }

    /// The last `days` UTC days of activity ending with the day of `now_ms`,
    /// oldest first, zero-filled.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn ui_activity(&self, now_ms: i64, days: usize) -> Result<Vec<DayActivity>, StoreError> {
        let days_i = i64::try_from(days.clamp(1, 366)).unwrap_or(30);
        let last = now_ms.div_euclid(DAY_MS) * DAY_MS;
        let first = last - (days_i - 1) * DAY_MS;
        let mut out: Vec<DayActivity> = (0..days_i)
            .map(|i| DayActivity {
                day: first + i * DAY_MS,
                ..DayActivity::default()
            })
            .collect();
        let mut fill = |sql: &str, set: fn(&mut DayActivity, u64, u64)| -> Result<(), StoreError> {
            let mut stmt = self.conn.prepare_cached(sql)?;
            let rows = stmt.query_map(params![first, last + DAY_MS, DAY_MS], |r| {
                Ok((r.get::<_, i64>(0)?, get_u64(r, 1)?, get_u64(r, 2)?))
            })?;
            for row in rows {
                let (day, a, b) = row?;
                if let Some(slot) = usize::try_from((day - first) / DAY_MS)
                    .ok()
                    .and_then(|i| out.get_mut(i))
                {
                    set(slot, a, b);
                }
            }
            Ok(())
        };
        fill(
            "SELECT (ts / ?3) * ?3 AS d, count(DISTINCT harness || char(31) || session_id), 0
             FROM events WHERE ts >= ?1 AND ts < ?2 GROUP BY d",
            |s, a, _| s.sessions = a,
        )?;
        fill(
            "SELECT (created_at / ?3) * ?3 AS d, count(*), 0
             FROM memories WHERE created_at >= ?1 AND created_at < ?2 GROUP BY d",
            |s, a, _| s.memories = a,
        )?;
        fill(
            "SELECT (at / ?3) * ?3 AS d, count(*), coalesce(sum(est_tokens), 0)
             FROM injections WHERE at >= ?1 AND at < ?2 GROUP BY d",
            |s, a, b| {
                s.packs = a;
                s.tokens = b;
            },
        )?;
        Ok(out)
    }

    /// Sessions, most recently active first (ties by first event id).
    ///
    /// # Errors
    /// `Invalid` for a malformed cursor; `Sqlite` on failure.
    pub fn ui_sessions(
        &self,
        filter: &SessionFilter,
        cursor: Option<&Cursor>,
        limit: usize,
    ) -> Result<Page<SessionRow>, StoreError> {
        let limit = clamp(limit);
        let mut rows = self.session_rows(filter, None, cursor, limit)?;
        for row in &mut rows {
            self.fill_session(row)?;
        }
        Ok(page(rows, limit, |r| {
            format!("{}:{}", r.last_ts, r.first_event_id)
        }))
    }

    fn session_rows(
        &self,
        filter: &SessionFilter,
        exact: Option<(&str, &str)>,
        cursor: Option<&Cursor>,
        limit: usize,
    ) -> Result<Vec<SessionRow>, StoreError> {
        let mut p = Params::default();
        let mut inner = Vec::new();
        if let Some((h, s)) = exact {
            inner.push(format!(
                "harness = {} AND session_id = {}",
                p.text(h),
                p.text(s)
            ));
        }
        if let Some(h) = &filter.harness {
            inner.push(format!("harness = {}", p.text(h.as_str())));
        }
        let mut outer = Vec::new();
        if let Some(project) = &filter.project {
            outer.push(format!("project = {}", p.text(project.as_str())));
        }
        if let Some(q) = filter.q.as_deref().filter(|q| !q.trim().is_empty()) {
            let q = p.text(q.trim());
            outer.push(format!(
                "(instr(lower(session_id), lower({q})) > 0
                  OR instr(lower(coalesce(project, '')), lower({q})) > 0)"
            ));
        }
        if let Some(c) = cursor {
            let (ts, id): (i64, i64) = c.pair()?;
            let (ts, id) = (p.bind(ts), p.bind(id));
            outer.push(format!(
                "(last_ts < {ts} OR (last_ts = {ts} AND first_id > {id}))"
            ));
        }
        let where_inner = if inner.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", inner.join(" AND "))
        };
        let where_outer = if outer.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", outer.join(" AND "))
        };
        let limit = p.limit(limit);
        let sql = format!(
            "SELECT harness, session_id, first_id, first_ts, last_ts, n, side, project FROM (
                 SELECT harness, session_id, first_id, first_ts, last_ts, n, side,
                        (SELECT cwd FROM events WHERE id = first_id) AS project
                 FROM (SELECT harness, session_id, min(id) AS first_id,
                              coalesce(min(ts), 0) AS first_ts, coalesce(max(ts), 0) AS last_ts,
                              count(*) AS n, sum(sidechain) AS side
                       FROM events {where_inner} GROUP BY harness, session_id)
             ) {where_outer} ORDER BY last_ts DESC, first_id ASC LIMIT {limit}"
        );
        let rows = self
            .conn
            .prepare(&sql)?
            .query_map(params_from_iter(p.0), |r| {
                Ok(SessionRow {
                    harness: r.get(0)?,
                    session_id: r.get(1)?,
                    first_event_id: r.get(2)?,
                    first_ts: r.get(3)?,
                    last_ts: r.get(4)?,
                    event_count: get_u64(r, 5)?,
                    sidechain_event_count: get_u64(r, 6)?,
                    project: r.get(7)?,
                    memory_count: 0,
                    injection_count: 0,
                    sources: Vec::new(),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Memory, injection and source-file counts for one session row, then
    /// sanitizes its strings (lookups need the raw ids, so this goes last).
    fn fill_session(&self, row: &mut SessionRow) -> Result<(), StoreError> {
        let (h, s) = (&row.harness, &row.session_id);
        row.memory_count = self.conn.query_row(
            "SELECT count(DISTINCT memory_id) FROM provenance
             WHERE harness = ?1 AND session_id = ?2",
            [h, s],
            |r| get_u64(r, 0),
        )?;
        row.injection_count = self.conn.query_row(
            "SELECT count(*) FROM injections WHERE session_id = ?1",
            [s],
            |r| get_u64(r, 0),
        )?;
        row.sources = self
            .conn
            .prepare_cached(
                "SELECT DISTINCT src.path FROM events e JOIN sources src ON src.id = e.source_id
                 WHERE e.harness = ?1 AND e.session_id = ?2 LIMIT ?3",
            )?
            .query_map(
                params![h, s, i64::try_from(SESSION_SOURCES).unwrap_or(3)],
                |r| r.get::<_, String>(0),
            )?
            .map(|p| p.map(|p| base_name(&p)))
            .collect::<Result<_, _>>()?;
        row.harness = sanitize(&row.harness);
        row.session_id = sanitize(&row.session_id);
        row.project = row.project.take().map(|p| sanitize(&p));
        Ok(())
    }

    /// One session with its memories and injections.
    ///
    /// # Errors
    /// `Sqlite` on failure. An unknown session is `Ok(None)`.
    pub fn ui_session_detail(
        &self,
        harness: &str,
        session_id: &str,
    ) -> Result<Option<SessionDetail>, StoreError> {
        let none = SessionFilter::default();
        let Some(mut session) = self
            .session_rows(&none, Some((harness, session_id)), None, 1)?
            .pop()
        else {
            return Ok(None);
        };
        self.fill_session(&mut session)?;
        let memories = self
            .conn
            .prepare_cached(
                "SELECT m.uid, m.kind, substr(m.text, 1, ?3), m.status FROM memories m
                 WHERE m.id IN (SELECT memory_id FROM provenance
                                WHERE harness = ?1 AND session_id = ?2)
                 ORDER BY m.id LIMIT ?4",
            )?
            .query_map(
                params![
                    harness,
                    session_id,
                    RAW_TEXT,
                    i64::try_from(DETAIL_MEMORIES).unwrap_or(200)
                ],
                memory_brief,
            )?
            .collect::<Result<_, _>>()?;
        let injections = self
            .conn
            .prepare_cached(
                "SELECT i.id, i.at, i.est_tokens, i.via, i.session_id,
                        (SELECT CASE WHEN min(harness) = max(harness) THEN min(harness) END
                         FROM events WHERE session_id = i.session_id)
                 FROM injections i WHERE i.session_id = ?1 ORDER BY i.id DESC LIMIT ?2",
            )?
            .query_map(
                params![session_id, i64::try_from(MAX_LIMIT).unwrap_or(200)],
                injection_brief,
            )?
            .collect::<Result<_, _>>()?;
        Ok(Some(SessionDetail {
            session,
            memories,
            injections,
        }))
    }

    /// A session's events in id order, starting after `after_id`. `Page::next`
    /// holds the last returned id.
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn ui_session_events(
        &self,
        harness: &str,
        session_id: &str,
        after_id: Option<i64>,
        limit: usize,
    ) -> Result<Page<SessionEvent>, StoreError> {
        let limit = clamp(limit);
        let rows = self
            .conn
            .prepare_cached(
                "SELECT id, ts, kind, tool_name, is_error, sidechain, substr(text, 1, ?5)
                 FROM events WHERE harness = ?1 AND session_id = ?2 AND id > ?3
                 ORDER BY id LIMIT ?4",
            )?
            .query_map(
                params![
                    harness,
                    session_id,
                    after_id.unwrap_or(0),
                    i64::try_from(limit + 1).unwrap_or(i64::MAX),
                    i64::try_from(EVENT_TEXT_CHARS + 1).unwrap_or(2001)
                ],
                |r| {
                    let raw: String = r.get(6)?;
                    let truncated = raw.chars().count() > EVENT_TEXT_CHARS;
                    let kept: String = raw.chars().take(EVENT_TEXT_CHARS).collect();
                    Ok(SessionEvent {
                        id: r.get(0)?,
                        ts: r.get(1)?,
                        kind: sanitize(&r.get::<_, String>(2)?),
                        tool_name: opt_clean(r.get(3)?),
                        is_error: r.get(4)?,
                        sidechain: r.get(5)?,
                        text: sanitize_multiline(&kept),
                        truncated,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(page(rows, limit, |e| e.id.to_string()))
    }

    /// Full stored text of one event, newlines kept.
    ///
    /// # Errors
    /// `Sqlite` on failure. An unknown id is `Ok(None)`.
    pub fn ui_event_text(&self, event_id: i64) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT text FROM events WHERE id = ?1", [event_id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map(|t| sanitize_multiline(&t)))
    }

    /// Memories filtered, searched and sorted (see [`MemoryFilter`]).
    ///
    /// # Errors
    /// `Invalid` for a malformed cursor; `Sqlite` on failure.
    pub fn ui_memories(
        &self,
        filter: &MemoryFilter,
        cursor: Option<&Cursor>,
        limit: usize,
    ) -> Result<Page<MemoryRow>, StoreError> {
        let limit = clamp(limit);
        let mut p = Params::default();
        let statuses = if filter.statuses.is_empty() {
            vec![Status::Active, Status::NeedsReverify]
        } else {
            filter.statuses.clone()
        };
        let mut w = vec![format!(
            "m.status IN ({})",
            p.bind_all(statuses.iter().map(ToString::to_string))
        )];
        if !filter.kinds.is_empty() {
            w.push(format!(
                "m.kind IN ({})",
                p.bind_all(filter.kinds.iter().map(ToString::to_string))
            ));
        }
        if let Some(project) = &filter.project {
            w.push(format!("m.scope_project = {}", p.text(project.as_str())));
        }
        if let Some(h) = &filter.harness {
            w.push(format!(
                "EXISTS (SELECT 1 FROM provenance pv WHERE pv.memory_id = m.id AND pv.harness = {})",
                p.text(h.as_str())
            ));
        }
        if let Some(fts) = filter.q.as_deref().and_then(fts_query) {
            w.push(format!(
                "m.id IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH {})",
                p.bind(fts)
            ));
        }
        let col = filter.sort.column();
        if let Some(c) = cursor {
            let bad = || StoreError::Invalid("bad cursor".to_owned());
            let (raw, id): (String, i64) = c.pair()?;
            let key = match filter.sort {
                MemorySort::Confidence => Value::Real(raw.parse().map_err(|_| bad())?),
                _ => Value::Integer(raw.parse().map_err(|_| bad())?),
            };
            let (key, id) = (p.bind(key), p.bind(id));
            w.push(format!(
                "({col} < {key} OR ({col} = {key} AND m.id > {id}))"
            ));
        }
        let limit_ph = p.limit(limit);
        let sql = format!(
            "SELECT {} FROM memories m WHERE {} ORDER BY {col} DESC, m.id ASC LIMIT {limit_ph}",
            memory_row_sql(),
            w.join(" AND ")
        );
        let mut rows = self
            .conn
            .prepare(&sql)?
            .query_map(params_from_iter(p.0), memory_row)?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, row) in &mut rows {
            row.first_source = self.first_source(*id)?;
        }
        let sort = filter.sort;
        let Page { rows, next } = page(rows, limit, |(id, r)| {
            let key = match sort {
                MemorySort::Updated => r.updated_at.to_string(),
                MemorySort::Created => r.created_at.to_string(),
                MemorySort::Seen => r.seen_count.to_string(),
                // Exact: the stored REAL came from an f32.
                MemorySort::Confidence => f64::from(r.confidence).to_string(),
            };
            format!("{key}:{id}")
        });
        Ok(Page {
            rows: rows.into_iter().map(|(_, r)| r).collect(),
            next,
        })
    }

    fn first_source(&self, memory_id: i64) -> Result<Option<SourceRef>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT harness, session_id, source_path, line FROM provenance
                 WHERE memory_id = ?1 ORDER BY id LIMIT 1",
            )?
            .query_row([memory_id], source_ref)
            .optional()?)
    }

    /// A memory by full uid or unique prefix (4+ hex chars), with all
    /// provenance, anchors and the injections that included it.
    ///
    /// # Errors
    /// `Ambiguous` if the prefix matches several memories; `Sqlite` on failure.
    /// No match (or a malformed id) is `Ok(None)`.
    pub fn ui_memory_detail(&self, id: &str) -> Result<Option<MemoryDetail>, StoreError> {
        let sql = format!(
            "SELECT {} FROM memories m WHERE m.uid = ?1 OR (length(?1) >= 4 AND m.uid LIKE ?1 || '%')
             ORDER BY (m.uid = ?1) DESC, m.uid LIMIT 2",
            memory_row_sql()
        );
        if !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let mut found = self
            .conn
            .prepare(&sql)?
            .query_map([id.to_ascii_lowercase()], memory_row)?
            .collect::<Result<Vec<_>, _>>()?;
        if found.len() > 1 && found[0].1.uid != id.to_ascii_lowercase() {
            return Err(StoreError::Ambiguous(format!(
                "id prefix {} matches several memories",
                sanitize(id)
            )));
        }
        let Some((mem_id, mut row)) = (!found.is_empty()).then(|| found.remove(0)) else {
            return Ok(None);
        };
        row.first_source = self.first_source(mem_id)?;
        let (text_full, valid_from, valid_to, superseded_by): (
            String,
            i64,
            Option<i64>,
            Option<String>,
        ) = self.conn.query_row(
            "SELECT text, valid_from, valid_to, superseded_by FROM memories WHERE id = ?1",
            [mem_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let mut provenance = Vec::new();
        let rows = self
            .conn
            .prepare_cached(
                "SELECT harness, session_id, source_path, line, origin, git_commit,
                        source_path
                 FROM provenance WHERE memory_id = ?1 ORDER BY id",
            )?
            .query_map([mem_id], |r| {
                Ok((
                    source_ref(r)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (source, origin, git_commit, path) in rows {
            let excerpt = match (path, source.line) {
                (Some(path), Some(line)) => {
                    let text = self.event_text_at(&path, line)?.join("\n");
                    (!text.is_empty()).then(|| one_line(&text, EXCERPT_CHARS))
                }
                _ => None,
            };
            provenance.push(ProvenanceView {
                origin: sanitize(&origin),
                source,
                git_commit: opt_clean(git_commit),
                excerpt,
            });
        }
        let anchors = self
            .conn
            .prepare_cached(
                "SELECT path, symbol, stale_reason FROM anchors WHERE memory_id = ?1 ORDER BY id",
            )?
            .query_map([mem_id], |r| {
                Ok(AnchorView {
                    path: sanitize(&r.get::<_, String>(0)?),
                    symbol: opt_clean(r.get(1)?),
                    stale_reason: opt_clean(r.get(2)?),
                })
            })?
            .collect::<Result<_, _>>()?;
        let injections = self.memory_injections(&row.uid)?;
        Ok(Some(MemoryDetail {
            row,
            text_full: sanitize_multiline(&text_full),
            valid_from,
            valid_to,
            superseded_by: opt_clean(superseded_by),
            provenance,
            anchors,
            injections,
        }))
    }

    /// Injections, newest first.
    ///
    /// # Errors
    /// `Invalid` for a malformed cursor; `Sqlite` on failure.
    pub fn ui_injections(
        &self,
        filter: &InjectionFilter,
        cursor: Option<&Cursor>,
        limit: usize,
    ) -> Result<Page<InjectionRow>, StoreError> {
        let limit = clamp(limit);
        let mut p = Params::default();
        let mut w = vec!["1 = 1".to_owned()];
        if let Some(v) = &filter.project {
            w.push(format!("i.project = {}", p.text(v.as_str())));
        }
        if let Some(v) = &filter.via {
            w.push(format!("i.via = {}", p.text(v.as_str())));
        }
        if let Some(v) = &filter.session_id {
            w.push(format!("i.session_id = {}", p.text(v.as_str())));
        }
        if let Some(c) = cursor {
            let id: i64 = c
                .as_str()
                .parse()
                .map_err(|_| StoreError::Invalid("bad cursor".to_owned()))?;
            w.push(format!("i.id < {}", p.bind(id)));
        }
        let limit_ph = p.limit(limit);
        let sql = format!(
            "SELECT {INJECTION_COLS} FROM injections i WHERE {} ORDER BY i.id DESC LIMIT {limit_ph}",
            w.join(" AND ")
        );
        let rows = self
            .conn
            .prepare(&sql)?
            .query_map(params_from_iter(p.0), injection_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(page(rows, limit, |r| r.id.to_string()))
    }

    /// Newest injections that included memory `uid`.
    fn memory_injections(&self, uid: &str) -> Result<Vec<InjectionBrief>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT i.id, i.at, i.est_tokens, i.via, i.session_id,
                        (SELECT CASE WHEN min(harness) = max(harness) THEN min(harness) END
                         FROM events WHERE session_id = i.session_id)
                 FROM injections i
                 WHERE EXISTS (SELECT 1 FROM json_each(i.memory_uids) j WHERE j.value = ?1)
                 ORDER BY i.id DESC LIMIT ?2",
            )?
            .query_map(
                params![uid, i64::try_from(DETAIL_INJECTIONS).unwrap_or(20)],
                injection_brief,
            )?
            .collect::<Result<_, _>>()?)
    }

    /// Distinct projects that own live memories, most memories first (at most 200).
    ///
    /// # Errors
    /// `Sqlite` on failure.
    pub fn ui_projects(&self) -> Result<Vec<ProjectCount>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT scope_project, count(*) AS n FROM memories
                 WHERE scope_project IS NOT NULL AND status IN ('active', 'needs_reverify')
                 GROUP BY scope_project ORDER BY n DESC, scope_project LIMIT 200",
            )?
            .query_map([], |r| {
                Ok(ProjectCount {
                    project: sanitize(&r.get::<_, String>(0)?),
                    memory_count: get_u64(r, 1)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    /// One injection with its exact pack text and the memories it named.
    ///
    /// # Errors
    /// `Sqlite` on failure. An unknown id is `Ok(None)`.
    pub fn ui_injection_detail(&self, id: i64) -> Result<Option<InjectionDetail>, StoreError> {
        let sql = format!("SELECT {INJECTION_COLS}, i.pack_text FROM injections i WHERE i.id = ?1");
        let Some((row, pack_text)) = self
            .conn
            .query_row(&sql, [id], |r| {
                Ok((injection_row(r)?, r.get::<_, Option<String>>(11)?))
            })
            .optional()?
        else {
            return Ok(None);
        };
        let sql = format!(
            "SELECT {} FROM injections i, json_each(i.memory_uids) j
             JOIN memories m ON m.uid = j.value WHERE i.id = ?1 ORDER BY j.key",
            memory_row_sql()
        );
        let rows = self
            .conn
            .prepare_cached(&sql)?
            .query_map([id], memory_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut memories = Vec::with_capacity(rows.len());
        for (memory_id, mut row) in rows {
            row.first_source = self.first_source(memory_id)?;
            memories.push(row);
        }
        Ok(Some(InjectionDetail {
            row,
            pack_text: pack_text.map(|t| sanitize_multiline(&t)),
            memories,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_pct_needs_a_baseline() {
        assert_eq!(saved_pct(100, Some(400)), Some(75.0));
        assert_eq!(saved_pct(100, Some(0)), None);
        assert_eq!(saved_pct(100, None), None);
        assert_eq!(saved_pct(0, Some(10)), Some(100.0));
    }

    #[test]
    fn limits_clamp_and_cursors_validate() {
        assert_eq!((clamp(0), clamp(5), clamp(10_000)), (1, 5, MAX_LIMIT));
        assert!(Cursor::parse("12:34").is_ok());
        assert!(Cursor::parse(&"9".repeat(MAX_CURSOR + 1)).is_err());
        assert!(Cursor::parse("é").is_err());
        assert!(Cursor::parse("a:b").unwrap().pair::<i64, i64>().is_err());
        assert_eq!(
            Cursor::parse("1:2").unwrap().pair::<i64, i64>().unwrap(),
            (1, 2)
        );
    }

    #[test]
    fn activity_is_zero_filled_and_bucketed_by_utc_day() {
        let s = Store::open_in_memory().unwrap();
        let now = 20 * DAY_MS + 5_000;
        let a = s.ui_activity(now, 7).unwrap();
        assert_eq!(a.len(), 7);
        assert_eq!(a[6].day, 20 * DAY_MS);
        assert_eq!(a[0].day, 14 * DAY_MS);
        assert!(a.iter().all(|d| d.sessions == 0 && d.packs == 0));
        s.conn
            .execute(
                "INSERT INTO injections (at, budget, est_tokens, memory_uids, via)
                 VALUES (?1, 100, 40, '[]', 'cli'), (?1 + 10, 100, 60, '[]', 'hook'), (?2, 100, 1, '[]', 'cli')",
                params![19 * DAY_MS + 7, 3 * DAY_MS],
            )
            .unwrap();
        let a = s.ui_activity(now, 7).unwrap();
        assert_eq!((a[5].packs, a[5].tokens), (2, 100));
        assert_eq!(
            a.iter().map(|d| d.packs).sum::<u64>(),
            2,
            "older rows are outside the window"
        );
    }

    #[test]
    fn empty_database_yields_empty_pages() {
        let s = Store::open_in_memory().unwrap();
        assert!(
            s.ui_sessions(&SessionFilter::default(), None, 10)
                .unwrap()
                .rows
                .is_empty()
        );
        assert!(
            s.ui_memories(&MemoryFilter::default(), None, 10)
                .unwrap()
                .next
                .is_none()
        );
        let o = s.ui_overview().unwrap();
        assert_eq!((o.sessions, o.injections, o.last_event_ts), (0, 0, None));
    }
}
