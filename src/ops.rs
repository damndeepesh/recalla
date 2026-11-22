//! Operations shared by the CLI and the MCP server: building a pack,
//! explaining a memory, opening the store. Output is plain text; each
//! front end decides how to print it.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use thiserror::Error;

use crate::anchor;
use crate::model::{Scope, Status};
use crate::pack;
use crate::paths;
use crate::store::{DEFAULT_BUSY, Filter, Store, StoreError, Via};

#[derive(Clone)]
pub struct PackArgs {
    pub budget: usize,
    pub project: Option<String>,
    pub query: Option<String>,
    pub no_log: bool,
    pub via: Via,
}

/// Builds the pack and, unless disabled, records it in the ledger. Nothing
/// is recorded when nothing was injected.
///
/// # Errors
/// A message naming the failure (missing DB, SQLite error, bad id).
pub fn pack_for(data_dir: &Path, args: &PackArgs) -> Result<pack::Pack, String> {
    pack_with(&mut open_existing(data_dir)?, args)
}

/// [`pack_for`] over an already-open store.
///
/// # Errors
/// A message naming the failure (SQLite error).
pub fn pack_with(store: &mut Store, args: &PackArgs) -> Result<pack::Pack, String> {
    let project = args.project.as_deref();
    if let Some(key) = project.filter(|k| anchor::project_dir(k).is_some()) {
        anchor::check(store, key, Some(PACK_CHECK_BUDGET)).map_err(|e| e.to_string())?;
    }
    let memories = store
        .memories_for_project(project, &[Status::Active, Status::NeedsReverify])
        .map_err(|e| e.to_string())?;
    let boosts = match args.query.as_deref() {
        Some(q) => {
            let hits = store
                .search(q, &Filter::default(), SEARCH_LIMIT)
                .map_err(|e| e.to_string())?;
            let scored: Vec<_> = hits.into_iter().map(|h| (h.memory.uid, h.score)).collect();
            pack::query_boosts(&scored)
        }
        None => HashMap::new(),
    };
    let label = project.map_or("global", |p| {
        Path::new(p)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(p)
    });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    let built = pack::build(&memories, args.budget, label, &boosts, now);
    if !args.no_log && !built.included_uids.is_empty() {
        store
            .record_injection(
                project,
                args.budget,
                built.est_tokens,
                &built.included_uids,
                args.via,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(built)
}

/// Time the staleness check may take before a pack is built.
pub const PACK_CHECK_BUDGET: Duration = Duration::from_millis(50);
/// Confidence floor a confirmed memory gets.
pub const CONFIRMED_CONFIDENCE: f32 = 0.9;

/// Confirms a memory: re-anchors it against the code as it is now (from its
/// text and existing anchor paths), clears stale reasons, sets it active and
/// raises its confidence to at least [`CONFIRMED_CONFIDENCE`]. Returns how
/// many anchors it has afterwards. Global memories have no code to anchor.
///
/// # Errors
/// A message naming the failure (unknown id, SQLite error).
pub fn confirm(store: &mut Store, uid: &str) -> Result<usize, String> {
    let m = store
        .get(uid)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("No memory matches {uid}"))?;
    let root = match &m.scope {
        Scope::Project(k) | Scope::Module { project: k, .. } => anchor::project_dir(k),
        Scope::Global => None,
    };
    let anchors = if let Some(root) = root {
        // Path-only anchors stay path anchors; symbol anchors came from a
        // symbol match (often a session-window file), so re-resolve those as
        // symbol-only and never degrade them to a whole-file anchor.
        let (sym, plain): (Vec<_>, Vec<_>) = m.anchors.iter().partition(|a| a.symbol.is_some());
        let paths: Vec<String> = plain.iter().map(|a| a.path.clone()).collect();
        let symbol_only: Vec<String> = sym.iter().map(|a| a.path.clone()).collect();
        let fresh = anchor::resolve_with(&m.text, &paths, &symbol_only, root);
        store
            .replace_anchors(uid, &fresh)
            .map_err(|e| e.to_string())?;
        fresh.len()
    } else {
        store.clear_stale(uid).map_err(|e| e.to_string())?;
        m.anchors.len()
    };
    store
        .set_status(uid, Status::Active)
        .map_err(|e| e.to_string())?;
    store
        .set_confidence(uid, m.confidence.max(CONFIRMED_CONFIDENCE))
        .map_err(|e| e.to_string())?;
    Ok(anchors)
}

/// Best-matching memories considered for a `--query` boost.
const SEARCH_LIMIT: usize = 50;

/// Outcome of [`why`].
pub enum Why {
    Found(String),
    /// Candidate list for stderr.
    Ambiguous(String),
    Missing(String),
}

/// Longest source-event excerpt `why` prints.
const EVENT_CHARS: usize = 300;

/// Provenance text for a memory id prefix (`json` selects the format).
///
/// # Errors
/// A message naming the failure (missing DB, SQLite error, bad id).
/// `sources`: include source file names and transcript excerpts. MCP passes
/// false for Global memories, whose sources may be another project's sessions.
pub fn why(store: &Store, id: &str, json: bool, sources: bool) -> Result<Why, String> {
    let mut found = store
        .memories_by_uid_prefix(id)
        .map_err(|e| e.to_string())?;
    let m = match found.len() {
        0 => return Ok(Why::Missing(clean(id))),
        1 => found.remove(0),
        _ => {
            let list = found
                .iter()
                .map(|m| {
                    let (uid, text) = (prefix(&m.uid, 12), truncate_chars(&m.text, 80));
                    format!("  {uid}  {}  {text}", m.kind)
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(Why::Ambiguous(format!(
                "error: {id} matches {} memories; use a longer prefix:\n{list}\n",
                found.len()
            )));
        }
    };
    let mut prov = Vec::new();
    for p in &m.provenance {
        let events = match (&p.source_path, p.line) {
            (Some(path), Some(line)) if sources => {
                store.event_text_at(path, line).map_err(|e| e.to_string())?
            }
            _ => Vec::new(),
        };
        let file = p.source_path.as_deref().filter(|_| sources).map(|s| {
            clean(
                &Path::new(s)
                    .file_name()
                    .map_or_else(|| s.to_owned(), |n| n.to_string_lossy().into_owned()),
            )
        });
        prov.push((p, file, events));
    }
    if json {
        return Ok(Why::Found(format!(
            "{}\n",
            json!({
                "uid": clean(&m.uid), "kind": m.kind.to_string(),
                "text": truncate_chars(&m.text, 2000),
                "scope": clean(&m.scope.to_string()), "confidence": m.confidence,
                "seen_count": m.seen_count, "status": m.status.to_string(),
                "provenance": prov.iter().map(|(p, file, events)| json!({
                    "origin": p.origin.to_string(), "harness": p.harness.as_deref().map(clean),
                    "session": p.session_id.as_deref().map(|s| prefix(s, 8)),
                    "source_file": file, "line": p.line,
                    "events": events.iter().map(|e| truncate_chars(e, EVENT_CHARS)).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "anchors": m.anchors.iter().map(|a| json!({
                    "path": clean(&a.path), "symbol": a.symbol.as_deref().map(clean),
                })).collect::<Vec<_>>(),
            })
        )));
    }
    let mut lines = vec![
        format!("{}  {}  {}", clean(&m.uid), m.kind, m.status),
        format!("  {}", truncate_chars(&m.text, 1000)),
        format!(
            "  scope {}  confidence {:.2}  seen {}",
            clean(&m.scope.to_string().replace('\u{1f}', " > ")),
            m.confidence,
            m.seen_count
        ),
        "provenance".to_owned(),
    ];
    for (p, file, events) in &prov {
        let line = p.line.map_or_else(|| "?".to_owned(), |l| l.to_string());
        lines.push(format!(
            "  {} {} {} line {line} ({})",
            clean(p.harness.as_deref().unwrap_or("-")),
            prefix(p.session_id.as_deref().unwrap_or("-"), 8),
            file.as_deref().unwrap_or("-"),
            p.origin
        ));
        lines.extend(
            events
                .iter()
                .map(|e| format!("    > {}", truncate_chars(e, EVENT_CHARS))),
        );
    }
    if !m.anchors.is_empty() {
        lines.push("anchors".to_owned());
        lines.extend(m.anchors.iter().map(|a| match &a.symbol {
            Some(sym) => format!("  {} ({})", clean(&a.path), clean(sym)),
            None => format!("  {}", clean(&a.path)),
        }));
    }
    Ok(Why::Found(lines.join("\n") + "\n"))
}

/// Opens the existing store; never creates one.
///
/// # Errors
/// A message naming the failure (missing DB, SQLite error, bad id).
pub fn open_existing(data_dir: &Path) -> Result<Store, String> {
    let db = paths::db_path(data_dir);
    if !db.is_file() {
        return Err(format!(
            "no database at {}; run `recalla backfill` first",
            db.display()
        ));
    }
    Store::open(&db).map_err(|e| format!("cannot open store: {e}"))
}

/// Why [`open_or_create`] failed.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error("cannot prepare data dir {}: {source}", path.display())]
    DataDir { path: PathBuf, source: io::Error },
    #[error("cannot open store: {0}")]
    Store(#[from] StoreError),
}

/// Opens the store, creating the data dir (0700) and DB (0600) if missing.
///
/// # Errors
/// [`OpenError`] when the dir or DB can't be prepared or opened.
pub fn open_or_create(data_dir: &Path) -> Result<Store, OpenError> {
    open_or_create_with(data_dir, DEFAULT_BUSY)
}

/// [`open_or_create`] with a custom SQLite busy timeout.
///
/// # Errors
/// [`OpenError`] when the dir or DB can't be prepared or opened.
pub fn open_or_create_with(data_dir: &Path, busy: Duration) -> Result<Store, OpenError> {
    let dir_err = |source| OpenError::DataDir {
        path: data_dir.to_owned(),
        source,
    };
    paths::ensure_dir(data_dir).map_err(dir_err)?;
    let db = paths::db_path(data_dir);
    paths::ensure_private_file(&db).map_err(dir_err)?;
    let store = Store::open_with_busy(&db, busy)?;
    paths::restrict_file(&db).map_err(dir_err)?;
    Ok(store)
}

#[must_use]
pub fn truncate_chars(text: &str, max: usize) -> String {
    crate::pack::one_line(text, max)
}
/// First `n` chars of `s`.
#[must_use]
pub fn prefix(s: &str, n: usize) -> String {
    let head: String = s.chars().take(n).collect();
    clean(&head)
}

/// `s` made safe to print: one line, no control or invisible characters.
/// Everything that comes from logs, the database or a mirror goes through it.
#[must_use]
pub fn clean(s: &str) -> String {
    pack::one_line(s, CLEAN_MAX)
}

/// Longest cleaned field.
const CLEAN_MAX: usize = 500;
