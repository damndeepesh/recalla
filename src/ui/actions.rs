//! Connector status and the dashboard's actions (`POST /api/...`). Mutations
//! reuse the CLI's own code paths (`ops`, `hooks_cfg`, `mirror`, backfill);
//! Origin and CSRF checks run before dispatch (see `UiHandler`).

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::api::{checked, ok, read_store, tilde};
use super::router::{ApiError, ApiResult, Ctx, Params, Route};
use super::security::{sanitize, sanitize_multiline};
use super::server::{Method, Request, Response};
use super::sync::{self, JobSpec};
use crate::hook;
use crate::hooks_cfg::{self, CfgError};
use crate::mirror;
use crate::model::{Scope, Status};
use crate::ops::{self, PackArgs};
use crate::pack;
use crate::paths;
use crate::store::{
    DEFAULT_BUSY, InjectionFilter, Store, StoreError, Via, saved_pct, schema_version,
};

/// Largest JSON body a POST handler reads.
const MAX_JSON_BODY: usize = 4 * 1024;
const MIN_BUDGET: u64 = 100;
const MAX_BUDGET: u64 = 8000;
const DEFAULT_BUDGET: u64 = 1500;
const HOOK_LOG_LINES: usize = 5;
const HOOK_LOG_CHARS: usize = 300;

/// Connector and action routes.
#[must_use]
pub fn routes() -> Vec<Route> {
    let route = |method, pattern, handler| Route {
        method,
        pattern,
        handler,
    };
    vec![
        route(Method::Get, "/api/connect", connect),
        route(Method::Post, "/api/hooks/install", hooks_install),
        route(Method::Post, "/api/hooks/uninstall", hooks_uninstall),
        route(Method::Post, "/api/memories/{uid}/confirm", memory_confirm),
        route(Method::Post, "/api/memories/{uid}/forget", memory_forget),
        route(Method::Post, "/api/memories/{uid}/restore", memory_restore),
        route(Method::Post, "/api/memories/{uid}/promote", memory_promote),
        route(Method::Post, "/api/memories/{uid}/edit", memory_edit),
        route(Method::Post, "/api/test-recall", test_recall),
        route(Method::Post, "/api/sync", sync_start),
        route(Method::Get, "/api/sync/status", sync_status),
    ]
}

// ---------------------------------------------------------------- connect

fn connect(ctx: &Ctx, _: &Request, _: &Params) -> ApiResult {
    let settings = ctx.settings_path.as_deref();
    let (hooks, settings_error) = hook_states(settings);
    let db = paths::db_path(&ctx.data_dir);
    // The page must work even when the database is missing or old.
    let store = read_store(ctx).ok();
    let schema = match Store::open_read_only(&db, DEFAULT_BUSY) {
        Ok(s) => s.user_version().ok(),
        Err(StoreError::Schema { found, .. }) => Some(found),
        Err(_) => None,
    };
    let last_capture_at = store
        .as_ref()
        .and_then(|s| s.ui_overview().ok())
        .and_then(|o| o.last_event_ts);
    let last_injection = store
        .as_ref()
        .and_then(|s| s.ui_injections(&InjectionFilter::default(), None, 1).ok())
        .and_then(|p| p.rows.into_iter().next())
        .map(|i| json!({ "at": i.at, "via": i.via, "project": i.project }));
    let log_tail: Vec<String> = hook::log_tail(&ctx.data_dir, HOOK_LOG_LINES)
        .iter()
        .map(|l| ops::truncate_chars(l, HOOK_LOG_CHARS))
        .collect();
    ok(&json!({
        "hooks": hooks,
        "settings_path": settings.map(tilde),
        "settings_exists": settings.is_some_and(Path::is_file),
        "settings_error": settings_error,
        "mcp": {
            "claude_code_command": hooks_cfg::mcp_claude_command(&ctx.exe),
            "codex_toml": hooks_cfg::mcp_codex_toml(&ctx.exe),
        },
        "binary_path": sanitize(&ctx.exe.display().to_string()),
        "data_dir": tilde(&ctx.data_dir),
        "db": {
            "exists": db.is_file(),
            "bytes": fs::metadata(&db).map_or(0, |m| m.len()),
            "schema_version": schema,
            "expected_schema_version": schema_version(),
        },
        "mirror_dir": tilde(&paths::mirror_dir(&ctx.data_dir)),
        "last_capture_at": last_capture_at,
        "last_injection": last_injection,
        "hook_log_tail": log_tail,
    }))
}

/// Per-event install state, or all-false plus a short reason when the
/// settings file can't be read.
fn hook_states(settings: Option<&Path>) -> (Vec<Value>, Option<String>) {
    let all = |installed: bool| {
        hooks_cfg::EVENTS
            .iter()
            .map(|(event, _)| json!({ "event": event, "installed": installed }))
            .collect()
    };
    let Some(path) = settings else {
        return (
            all(false),
            Some("no settings path (HOME is unset)".to_owned()),
        );
    };
    match hooks_cfg::installed(path) {
        Ok(states) => (
            states
                .into_iter()
                .map(|(event, installed)| json!({ "event": event, "installed": installed }))
                .collect(),
            None,
        ),
        Err(e) => (all(false), Some(cfg_message(&e))),
    }
}

fn cfg_message(e: &CfgError) -> String {
    sanitize(&e.to_string())
}

// ---------------------------------------------------------------- hooks

fn hooks_install(ctx: &Ctx, _: &Request, _: &Params) -> ApiResult {
    if ctx.exe.as_os_str().is_empty() {
        return Err(ApiError::internal("cannot locate the recalla binary"));
    }
    edit_hooks(ctx, |path| hooks_cfg::install(path, &ctx.exe, false))
}

fn hooks_uninstall(ctx: &Ctx, _: &Request, _: &Params) -> ApiResult {
    edit_hooks(ctx, |path| hooks_cfg::uninstall(path, false))
}

fn edit_hooks(
    ctx: &Ctx,
    f: impl FnOnce(&Path) -> Result<hooks_cfg::Outcome, CfgError>,
) -> ApiResult {
    let path = ctx
        .settings_path
        .as_deref()
        .ok_or_else(|| ApiError::BadRequest("no settings path (HOME is unset)".to_owned()))?;
    let outcome = f(path).map_err(|e| match e {
        CfgError::Malformed { .. } | CfgError::Shape { .. } => ApiError::Conflict(cfg_message(&e)),
        CfgError::Read { .. } | CfgError::Write { .. } => ApiError::internal(&e.to_string()),
    })?;
    let backup = outcome
        .backup
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| sanitize(&n.to_string_lossy()));
    let (hooks, _) = hook_states(Some(path));
    ok(&json!({
        "changed": outcome.changed,
        "changes": outcome.changes.iter().map(|c| sanitize(c)).collect::<Vec<_>>(),
        "backup": backup,
        "hooks": hooks,
    }))
}

// ---------------------------------------------------------------- memories

#[derive(Clone, Copy)]
enum Act {
    Confirm,
    Forget,
    Restore,
}

fn memory_confirm(ctx: &Ctx, _: &Request, p: &Params) -> ApiResult {
    memory_action(ctx, p, Act::Confirm)
}

fn memory_forget(ctx: &Ctx, _: &Request, p: &Params) -> ApiResult {
    memory_action(ctx, p, Act::Forget)
}

fn memory_restore(ctx: &Ctx, _: &Request, p: &Params) -> ApiResult {
    memory_action(ctx, p, Act::Restore)
}

/// Body `{"scope": "global"}` (default) or `{"scope": "project", "project": KEY}`.
/// Answers with the new memory; the original is superseded by it.
fn memory_promote(ctx: &Ctx, req: &Request, p: &Params) -> ApiResult {
    let id = checked("uid", p.get("uid").ok_or(ApiError::NotFound)?, 64)?;
    let body = json_body(req)?;
    let target = match string_field(&body, "scope", 16)?.as_deref() {
        None | Some("global") => Scope::Global,
        Some("project") => {
            Scope::Project(string_field(&body, "project", 500)?.ok_or_else(|| {
                ApiError::BadRequest("project scope needs a project key".to_owned())
            })?)
        }
        Some(_) => return Err(ApiError::BadRequest("invalid scope".to_owned())),
    };
    let mut store = write_store(ctx)?;
    let uid = match store.memories_by_uid_prefix(id)?.as_slice() {
        [] => return Err(ApiError::NotFound),
        [one] => one.uid.clone(),
        _ => {
            return Err(ApiError::Conflict(
                "id prefix matches several memories; use a longer one".to_owned(),
            ));
        }
    };
    let new_uid = ops::promote(&mut store, &uid, &target).map_err(ApiError::BadRequest)?;
    if let Some(warning) = mirror::sync_best_effort(&mut store, &ctx.data_dir) {
        eprintln!("recalla ui: {warning}");
    }
    let detail = store
        .ui_memory_detail(&new_uid)?
        .ok_or(ApiError::NotFound)?;
    ok(&super::api::memory_row_json(&detail.row))
}

/// Body `{"text": "..."}`. The text is redacted and the original superseded
/// by a confirmed memory carrying it. Answers with the new memory.
fn memory_edit(ctx: &Ctx, req: &Request, p: &Params) -> ApiResult {
    let id = checked("uid", p.get("uid").ok_or(ApiError::NotFound)?, 64)?;
    let body = json_body(req)?;
    let text = string_field(&body, "text", ops::MAX_EDIT_CHARS * 4)?
        .ok_or_else(|| ApiError::BadRequest("text is required".to_owned()))?;
    let mut store = write_store(ctx)?;
    let uid = match store.memories_by_uid_prefix(id)?.as_slice() {
        [] => return Err(ApiError::NotFound),
        [one] => one.uid.clone(),
        _ => {
            return Err(ApiError::Conflict(
                "id prefix matches several memories; use a longer one".to_owned(),
            ));
        }
    };
    let new_uid = ops::edit(&mut store, &uid, &text).map_err(ApiError::BadRequest)?;
    if let Some(warning) = mirror::sync_best_effort(&mut store, &ctx.data_dir) {
        eprintln!("recalla ui: {warning}");
    }
    let detail = store
        .ui_memory_detail(&new_uid)?
        .ok_or(ApiError::NotFound)?;
    ok(&super::api::memory_row_json(&detail.row))
}

/// A writable connection to the existing database.
fn write_store(ctx: &Ctx) -> Result<Store, ApiError> {
    let db = paths::db_path(&ctx.data_dir);
    if !db.is_file() {
        return Err(ApiError::Unavailable("database not found"));
    }
    Ok(Store::open_with_busy(db, DEFAULT_BUSY)?)
}

/// Human path: Global memories are allowed here (the MCP tools refuse them).
fn memory_action(ctx: &Ctx, p: &Params, act: Act) -> ApiResult {
    let id = checked("uid", p.get("uid").ok_or(ApiError::NotFound)?, 64)?;
    let mut store = write_store(ctx)?;
    let uid = match store.memories_by_uid_prefix(id)?.as_slice() {
        [] => return Err(ApiError::NotFound),
        [one] => one.uid.clone(),
        _ => {
            return Err(ApiError::Conflict(
                "id prefix matches several memories; use a longer one".to_owned(),
            ));
        }
    };
    match act {
        Act::Confirm => {
            ops::confirm(&mut store, &uid).map_err(|e| ApiError::internal(&e))?;
        }
        Act::Forget => store.archive(&uid)?,
        Act::Restore => store.set_status(&uid, Status::Active)?,
    }
    if let Some(warning) = mirror::sync_best_effort(&mut store, &ctx.data_dir) {
        eprintln!("recalla ui: {warning}");
    }
    let detail = store.ui_memory_detail(&uid)?.ok_or(ApiError::NotFound)?;
    ok(&super::api::memory_row_json(&detail.row))
}

// ---------------------------------------------------------------- test recall

/// The JSON object in a request body (empty counts as `{}`).
fn json_body(req: &Request) -> Result<serde_json::Map<String, Value>, ApiError> {
    if req.body.len() > MAX_JSON_BODY {
        return Err(ApiError::BadRequest("body too large".to_owned()));
    }
    if req.body.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_slice(&req.body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(ApiError::BadRequest(
            "body must be a JSON object".to_owned(),
        )),
    }
}

fn string_field(
    body: &serde_json::Map<String, Value>,
    name: &str,
    max: usize,
) -> Result<Option<String>, ApiError> {
    match body.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => checked(name, s, max).map(|s| Some(s.to_owned())),
        Some(_) => Err(ApiError::BadRequest(format!("invalid {name}"))),
    }
}

/// Builds a pack as `recalla pack` does, but read-only: no injection is
/// logged and the anchor staleness check (which writes) is skipped.
fn test_recall(ctx: &Ctx, req: &Request, _: &Params) -> ApiResult {
    let body = json_body(req)?;
    let budget = match body.get("budget") {
        None | Some(Value::Null) => DEFAULT_BUDGET,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| ApiError::BadRequest("invalid budget".to_owned()))?,
    }
    .clamp(MIN_BUDGET, MAX_BUDGET);
    let args = PackArgs {
        budget: usize::try_from(budget).unwrap_or(1500),
        project: string_field(&body, "project", 500)?,
        query: string_field(&body, "query", 200)?,
        no_log: true,
        via: Via::Cli,
        session_id: None,
    };
    let store = read_store(ctx)?;
    let (pack, base) = ops::pack_preview(&store, &args).map_err(|e| ApiError::internal(&e))?;
    let mut memories = Vec::new();
    for uid in &pack.included_uids {
        if let Some(m) = store.get(uid)? {
            memories.push(json!({
                "uid": m.uid,
                "kind": m.kind.as_str(),
                "text": ops::truncate_chars(&m.text, 400),
            }));
        }
    }
    let to_u64 = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    ok(&json!({
        "markdown": sanitize_multiline(&pack.markdown),
        "est_tokens": pack.est_tokens,
        "candidate_count": base.candidate_count,
        "candidate_tokens": base.candidate_tokens,
        "saved_pct": saved_pct(to_u64(pack.est_tokens), Some(to_u64(base.candidate_tokens)))
            .map(|v| (v * 10.0).round() / 10.0),
        "memories": memories,
        "explain": pack::explain_json(&pack.decisions),
    }))
}

// ---------------------------------------------------------------- sync

#[allow(clippy::unnecessary_wraps)] // route signature
fn sync_start(ctx: &Ctx, _: &Request, _: &Params) -> ApiResult {
    match ctx.sync.try_start() {
        Ok(job) => {
            sync::spawn(
                &ctx.sync,
                JobSpec {
                    data_dir: ctx.data_dir.clone(),
                    claude_root: ctx.claude_root.clone(),
                    codex_root: ctx.codex_root.clone(),
                    opencode_root: ctx.opencode_root.clone(),
                },
            );
            Ok(Response::json(202, &json!({ "job": job })))
        }
        Err(job) => Ok(Response::json(
            409,
            &json!({ "error": "sync already running", "job": job }),
        )),
    }
}

fn sync_status(ctx: &Ctx, _: &Request, _: &Params) -> ApiResult {
    ok(&ctx.sync.status_json())
}
