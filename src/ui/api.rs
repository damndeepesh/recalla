//! Read-only JSON API (`GET /api/...`). Every handler opens its own
//! read-only database connection; strings come out of the store already
//! sanitized. Errors map to generic JSON via [`ApiError`].
//!
//! The wire format is documented in ARCHITECTURE.md ("Dashboard API").

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use super::router::{ApiError, ApiResult, Ctx, Params, Route};
use super::security::sanitize;
use super::server::{Method, Request, Response};
use crate::model::{Kind, Status};
use crate::paths;
use crate::store::{
    Cursor, DEFAULT_BUSY, InjectionBrief, InjectionFilter, InjectionRow, MemoryBrief, MemoryFilter,
    MemoryRow, MemorySort, Page, SessionEvent, SessionFilter, SessionRow, SourceRef, Store,
    saved_pct, schema_version,
};

/// Longest `q` accepted.
const MAX_QUERY: usize = 200;
/// Longest filter value, path parameter or list item accepted.
const MAX_FILTER: usize = 500;
/// Days of activity in the overview's chart series.
const ACTIVITY_DAYS: usize = 30;
/// Page size when `limit` is absent.
const DEFAULT_LIMIT: usize = 50;

/// The read routes.
#[must_use]
pub fn routes() -> Vec<Route> {
    let get = |pattern, handler| Route {
        method: Method::Get,
        pattern,
        handler,
    };
    vec![
        get("/api/overview", overview),
        get("/api/projects", projects),
        get("/api/sessions", sessions),
        get("/api/sessions/{harness}/{session_id}", session_detail),
        get(
            "/api/sessions/{harness}/{session_id}/events",
            session_events,
        ),
        get("/api/events/{id}/text", event_text),
        get("/api/memories", memories),
        get("/api/memories/{uid}", memory_detail),
        get("/api/injections", injections),
        get("/api/injections/{id}", injection_detail),
    ]
}

// ---------------------------------------------------------------- helpers

#[allow(clippy::unnecessary_wraps)] // lets handlers end with `ok(..)`
pub(super) fn ok(value: &Value) -> ApiResult {
    Ok(Response::json(200, value))
}

/// A read-only connection; maps a missing file, an old schema and a busy
/// database to the right status.
pub(super) fn read_store(ctx: &Ctx) -> Result<Store, ApiError> {
    Ok(Store::open_read_only(
        paths::db_path(&ctx.data_dir),
        DEFAULT_BUSY,
    )?)
}

fn bad(name: &str) -> ApiError {
    ApiError::BadRequest(format!("invalid {name}"))
}

/// `v` if it is at most `max` chars with no control characters.
pub(super) fn checked<'a>(name: &str, v: &'a str, max: usize) -> Result<&'a str, ApiError> {
    if v.chars().count() > max || v.chars().any(char::is_control) {
        return Err(bad(name));
    }
    Ok(v)
}

/// A filter value from the query string; absent or empty is `None`.
fn filter(req: &Request, name: &str, max: usize) -> Result<Option<String>, ApiError> {
    match req.query_param(name) {
        None | Some("") => Ok(None),
        Some(v) => checked(name, v, max).map(|v| Some(v.to_owned())),
    }
}

fn limit(req: &Request) -> Result<usize, ApiError> {
    req.query_param("limit")
        .filter(|v| !v.is_empty())
        .map_or(Ok(DEFAULT_LIMIT), |v| v.parse().map_err(|_| bad("limit")))
}

fn cursor(req: &Request) -> Result<Option<Cursor>, ApiError> {
    match req.query_param("cursor") {
        None | Some("") => Ok(None),
        Some(v) => Ok(Some(Cursor::parse(v)?)),
    }
}

fn path_param<'a>(p: &'a Params, name: &str) -> Result<&'a str, ApiError> {
    checked(name, p.get(name).ok_or(ApiError::NotFound)?, MAX_FILTER)
}

fn id_param(p: &Params) -> Result<i64, ApiError> {
    path_param(p, "id")?.parse().map_err(|_| bad("id"))
}

/// A comma-separated list of enum names; unknown values are a 400.
fn enum_list<T: std::str::FromStr>(req: &Request, name: &str) -> Result<Vec<T>, ApiError> {
    let Some(raw) = req.query_param(name).filter(|v| !v.is_empty()) else {
        return Ok(Vec::new());
    };
    checked(name, raw, MAX_FILTER)?
        .split(',')
        .map(|item| item.parse().map_err(|_| bad(name)))
        .collect()
}

fn sort(req: &Request) -> Result<MemorySort, ApiError> {
    match req.query_param("sort").unwrap_or("") {
        "" | "updated" => Ok(MemorySort::Updated),
        "confidence" => Ok(MemorySort::Confidence),
        "seen" => Ok(MemorySort::Seen),
        "created" => Ok(MemorySort::Created),
        _ => Err(bad("sort")),
    }
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// `path` with the home directory shown as `~`, sanitized.
pub(super) fn tilde(path: &Path) -> String {
    tilde_in(path, std::env::var_os("HOME").as_deref().map(Path::new))
}

fn tilde_in(path: &Path, home: Option<&Path>) -> String {
    let shown = home
        .filter(|h| !h.as_os_str().is_empty())
        .and_then(|h| path.strip_prefix(h).ok())
        .map_or_else(
            || path.display().to_string(),
            |rest| {
                if rest.as_os_str().is_empty() {
                    "~".to_owned()
                } else {
                    format!("~/{}", rest.display())
                }
            },
        );
    sanitize(&shown)
}

/// Confidence as a short decimal (0.9, not 0.8999999761581421).
fn round2(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn page_json<T>(page: &Page<T>, item: impl Fn(&T) -> Value) -> Value {
    json!({
        "items": page.rows.iter().map(item).collect::<Vec<_>>(),
        "next": page.next.as_ref().map(Cursor::as_str),
    })
}

// ---------------------------------------------------------------- JSON shapes

fn source_json(s: &SourceRef) -> Value {
    json!({
        "harness": s.harness, "session_id": s.session_id, "file": s.file, "line": s.line,
    })
}

fn session_json(s: &SessionRow) -> Value {
    json!({
        "harness": s.harness,
        "session_id": s.session_id,
        "project": s.project,
        "first_ts": s.first_ts,
        "last_ts": s.last_ts,
        "event_count": s.event_count,
        "sidechain_event_count": s.sidechain_event_count,
        "memory_count": s.memory_count,
        "injection_count": s.injection_count,
        "sources": s.sources,
    })
}

fn memory_brief_json(m: &MemoryBrief) -> Value {
    json!({ "uid": m.uid, "kind": m.kind.as_str(), "text": m.text, "status": m.status.as_str() })
}

fn injection_brief_json(i: &InjectionBrief) -> Value {
    json!({
        "id": i.id, "at": i.at, "est_tokens": i.est_tokens, "via": i.via,
        "session_id": i.session_id, "harness": i.harness,
    })
}

fn event_json(e: &SessionEvent) -> Value {
    json!({
        "id": e.id, "ts": e.ts, "kind": e.kind, "tool_name": e.tool_name,
        "is_error": e.is_error, "sidechain": e.sidechain, "text": e.text,
        "truncated": e.truncated,
    })
}

/// A memory list row; also the response of the memory actions.
pub(super) fn memory_row_json(m: &MemoryRow) -> Value {
    json!({
        "uid": m.uid,
        "kind": m.kind.as_str(),
        "text": m.text,
        "scope_project": m.scope_project,
        "scope_path": m.scope_path,
        "status": m.status.as_str(),
        "confidence": round2(m.confidence),
        "importance": m.importance,
        "seen_count": m.seen_count,
        "created_at": m.created_at,
        "updated_at": m.updated_at,
        "anchor_count": m.anchor_count,
        "stale_reason": m.stale_reason,
        "first_source": m.first_source.as_ref().map(source_json),
    })
}

fn injection_row_json(i: &InjectionRow) -> Value {
    json!({
        "id": i.id,
        "at": i.at,
        "project": i.project,
        "via": i.via,
        "session_id": i.session_id,
        "harness": i.harness,
        "budget": i.budget,
        "est_tokens": i.est_tokens,
        "candidate_count": i.candidate_count,
        "candidate_tokens": i.candidate_tokens,
        "saved_pct": i.saved_pct.map(round1),
        "memory_count": i.memory_count,
    })
}

/// `row` plus extra fields, as one flat object.
fn extend(mut row: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(more)) = (row.as_object_mut(), extra) {
        base.extend(more);
    }
    row
}

// ---------------------------------------------------------------- handlers

fn projects(ctx: &Ctx, _: &Request, _: &Params) -> ApiResult {
    let items: Vec<Value> = read_store(ctx)?
        .ui_projects()?
        .iter()
        .map(|p| json!({ "project": p.project, "memory_count": p.memory_count }))
        .collect();
    ok(&json!({ "items": items }))
}

fn overview(ctx: &Ctx, _: &Request, _: &Params) -> ApiResult {
    let store = read_store(ctx)?;
    let o = store.ui_overview()?;
    let activity: Vec<Value> = store
        .ui_activity(now_ms(), ACTIVITY_DAYS)?
        .iter()
        .map(|d| json!({ "day": d.day, "sessions": d.sessions, "memories": d.memories, "packs": d.packs, "tokens": d.tokens }))
        .collect();
    let (mut by_kind, mut by_status) = (Map::new(), Map::new());
    let mut total = 0u64;
    for (kind, status, n) in &o.memories {
        total += n;
        for (map, key) in [
            (&mut by_kind, kind.as_str()),
            (&mut by_status, status.as_str()),
        ] {
            let sum = map.get(key).and_then(Value::as_u64).unwrap_or(0) + n;
            map.insert(key.to_owned(), json!(sum));
        }
    }
    let file_len = |p: &Path| fs::metadata(p).map_or(0, |m| m.len());
    let archive_bytes: u64 = fs::read_dir(paths::archive_dir(&ctx.data_dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "zst"))
        .map(|p| file_len(&p))
        .sum();
    ok(&json!({
        "memories": {
            "total": total, "stale": o.stale, "by_kind": by_kind, "by_status": by_status,
        },
        "sessions": o.sessions,
        "events": o.events,
        "sources": o.sources,
        "injections": {
            "count": o.injections,
            "est_tokens": o.injected_tokens,
            "baselined_est_tokens": o.baselined_tokens,
            "candidate_tokens": o.candidate_tokens,
            "saved_pct": saved_pct(o.baselined_tokens, Some(o.candidate_tokens)).map(round1),
            "last_at": o.last_injection_at,
            "last_via": o.last_injection_via,
        },
        "last_event_ts": o.last_event_ts,
        "activity": activity,
        "db_bytes": file_len(&paths::db_path(&ctx.data_dir)),
        "archive_bytes": archive_bytes,
        "data_dir": tilde(&ctx.data_dir),
        "mirror_dir": tilde(&paths::mirror_dir(&ctx.data_dir)),
        "version": crate::VERSION,
        "schema_version": schema_version(),
        "generated_at": now_ms(),
    }))
}

fn sessions(ctx: &Ctx, req: &Request, _: &Params) -> ApiResult {
    let f = SessionFilter {
        harness: filter(req, "harness", MAX_FILTER)?,
        project: filter(req, "project", MAX_FILTER)?,
        q: filter(req, "q", MAX_QUERY)?,
    };
    let (cursor, limit) = (cursor(req)?, limit(req)?);
    let page = read_store(ctx)?.ui_sessions(&f, cursor.as_ref(), limit)?;
    ok(&page_json(&page, session_json))
}

fn session_detail(ctx: &Ctx, _: &Request, p: &Params) -> ApiResult {
    let (harness, id) = (path_param(p, "harness")?, path_param(p, "session_id")?);
    let d = read_store(ctx)?
        .ui_session_detail(harness, id)?
        .ok_or(ApiError::NotFound)?;
    ok(&json!({
        "session": session_json(&d.session),
        "memories": d.memories.iter().map(memory_brief_json).collect::<Vec<_>>(),
        "injections": d.injections.iter().map(injection_brief_json).collect::<Vec<_>>(),
    }))
}

fn session_events(ctx: &Ctx, req: &Request, p: &Params) -> ApiResult {
    let (harness, id) = (path_param(p, "harness")?, path_param(p, "session_id")?);
    let after = match req.query_param("after").filter(|v| !v.is_empty()) {
        None => None,
        Some(v) => Some(
            v.parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or_else(|| bad("after"))?,
        ),
    };
    let store = read_store(ctx)?;
    // An unknown session is a 404, not an empty page.
    store
        .ui_session_detail(harness, id)?
        .ok_or(ApiError::NotFound)?;
    let page = store.ui_session_events(harness, id, after, limit(req)?)?;
    ok(&page_json(&page, event_json))
}

fn event_text(ctx: &Ctx, _: &Request, p: &Params) -> ApiResult {
    let id = id_param(p)?;
    let text = read_store(ctx)?
        .ui_event_text(id)?
        .ok_or(ApiError::NotFound)?;
    ok(&json!({ "id": id, "text": text }))
}

fn memories(ctx: &Ctx, req: &Request, _: &Params) -> ApiResult {
    let f = MemoryFilter {
        kinds: enum_list::<Kind>(req, "kind")?,
        statuses: enum_list::<Status>(req, "status")?,
        project: filter(req, "project", MAX_FILTER)?,
        harness: filter(req, "harness", MAX_FILTER)?,
        q: filter(req, "q", MAX_QUERY)?,
        sort: sort(req)?,
    };
    let (cursor, limit) = (cursor(req)?, limit(req)?);
    let page = read_store(ctx)?.ui_memories(&f, cursor.as_ref(), limit)?;
    ok(&page_json(&page, memory_row_json))
}

fn memory_detail(ctx: &Ctx, _: &Request, p: &Params) -> ApiResult {
    let d = read_store(ctx)?
        .ui_memory_detail(path_param(p, "uid")?)?
        .ok_or(ApiError::NotFound)?;
    ok(&extend(
        memory_row_json(&d.row),
        json!({
            "text_full": d.text_full,
            "valid_from": d.valid_from,
            "valid_to": d.valid_to,
            "superseded_by": d.superseded_by,
            "provenance": d.provenance.iter().map(|v| json!({
                "origin": v.origin,
                "harness": v.source.harness,
                "session_id": v.source.session_id,
                "file": v.source.file,
                "line": v.source.line,
                "git_commit": v.git_commit,
                "excerpt": v.excerpt,
            })).collect::<Vec<_>>(),
            "anchors": d.anchors.iter().map(|a| json!({
                "path": a.path, "symbol": a.symbol, "stale_reason": a.stale_reason,
            })).collect::<Vec<_>>(),
            "injections": d.injections.iter().map(injection_brief_json).collect::<Vec<_>>(),
        }),
    ))
}

fn injections(ctx: &Ctx, req: &Request, _: &Params) -> ApiResult {
    let f = InjectionFilter {
        project: filter(req, "project", MAX_FILTER)?,
        via: filter(req, "via", MAX_FILTER)?,
        session_id: filter(req, "session", MAX_FILTER)?,
    };
    let (cursor, limit) = (cursor(req)?, limit(req)?);
    let page = read_store(ctx)?.ui_injections(&f, cursor.as_ref(), limit)?;
    ok(&page_json(&page, injection_row_json))
}

fn injection_detail(ctx: &Ctx, _: &Request, p: &Params) -> ApiResult {
    let d = read_store(ctx)?
        .ui_injection_detail(id_param(p)?)?
        .ok_or(ApiError::NotFound)?;
    ok(&extend(
        injection_row_json(&d.row),
        json!({
            "pack_text": d.pack_text,
            "memories": d.memories.iter().map(memory_row_json).collect::<Vec<_>>(),
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_shown_as_tilde() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(
            tilde_in(Path::new("/home/u/.claude/x"), home),
            "~/.claude/x"
        );
        assert_eq!(tilde_in(Path::new("/home/u"), home), "~");
        assert_eq!(tilde_in(Path::new("/home/user2/x"), home), "/home/user2/x");
        assert_eq!(tilde_in(Path::new("/etc/x"), None), "/etc/x");
        assert_eq!(tilde_in(Path::new("/etc/x"), Some(Path::new(""))), "/etc/x");
        assert_eq!(tilde_in(Path::new("/a\u{1b}b"), None), "/ab");
    }

    #[test]
    fn confidence_and_percentages_are_short_decimals() {
        assert_eq!(round2(0.9), 0.9);
        assert_eq!(round2(0.123_456), 0.12);
        assert_eq!(round1(66.666_7), 66.7);
    }
}
