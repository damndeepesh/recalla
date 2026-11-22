//! The four MCP tools. Each returns `Ok(text)` or `Err(message)`; the
//! protocol layer turns the latter into an `isError` result.

use serde_json::{Value, json};

use super::McpCtx;
use crate::model::{Kind, Memory, NewMemory, Origin, Provenance, Scope, Status};
use crate::ops::{self, PackArgs, Why};
use crate::paths;
use crate::redact::redact;
use crate::store::{Insert, Via};

const MAX_TEXT_CHARS: usize = 1000;
const MIN_BUDGET: i64 = 100;
const MAX_BUDGET: i64 = 8000;
const DEFAULT_BUDGET: usize = 1200;
const CONFIRMED_CONFIDENCE: f32 = 0.9;
const REMEMBER_IMPORTANCE: u8 = 2;
const MAX_CANDIDATES: usize = 5;
const EMPTY: &str = "No memories yet for this project.";
const KINDS: [&str; 6] = [
    "preference",
    "decision",
    "gotcha",
    "procedure",
    "fact",
    "task",
];

/// The `tools` array of `tools/list`. A test caps its size at 1k tokens.
pub fn list() -> Value {
    json!([
        {
            "name": "recall",
            "description": "Get this project's memories (preferences, decisions, gotchas). Call at task start.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Topic to favor"},
                    "budget": {"type": "integer", "minimum": MIN_BUDGET, "maximum": MAX_BUDGET, "description": "Max tokens (default 1200)"}
                },
                "additionalProperties": false
            }
        },
        {
            "name": "remember",
            "description": "Save a durable decision, preference or gotcha. Max 1000 chars.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "kind": {"type": "string", "enum": KINDS, "description": "Default fact"}
                },
                "required": ["text"],
                "additionalProperties": false
            }
        },
        {
            "name": "review",
            "description": "Confirm a memory is right, or forget it (reversible).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Memory id prefix"},
                    "action": {"type": "string", "enum": ["confirm", "forget"]}
                },
                "required": ["id", "action"],
                "additionalProperties": false
            }
        },
        {
            "name": "why",
            "description": "Show where a memory came from.",
            "inputSchema": {
                "type": "object",
                "properties": {"id": {"type": "string", "description": "Memory id prefix"}},
                "required": ["id"],
                "additionalProperties": false
            }
        }
    ])
}

/// Runs tool `name`; `None` when no such tool exists.
pub fn call(name: &str, args: &Value, ctx: &McpCtx) -> Option<Result<String, String>> {
    Some(match name {
        "recall" => recall(args, ctx),
        "remember" => remember(args, ctx),
        "review" => review(args, ctx),
        "why" => why(args, ctx),
        _ => return None,
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` is required and must be a string"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn recall(args: &Value, ctx: &McpCtx) -> Result<String, String> {
    let query = opt_str(args, "query")?
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_owned);
    let budget = match args.get("budget") {
        None | Some(Value::Null) => DEFAULT_BUDGET,
        Some(v) => v
            .as_i64()
            .filter(|b| (MIN_BUDGET..=MAX_BUDGET).contains(b))
            .and_then(|b| usize::try_from(b).ok())
            .ok_or_else(|| format!("`budget` must be an integer in {MIN_BUDGET}..={MAX_BUDGET}"))?,
    };
    if !paths::db_path(&ctx.data_dir).is_file() {
        return Ok(EMPTY.to_owned());
    }
    let built = ops::pack_for(
        &ctx.data_dir,
        &PackArgs {
            budget,
            project: ctx.project.clone(),
            query,
            no_log: false,
            via: Via::Mcp,
        },
    )?;
    Ok(if built.markdown.is_empty() {
        EMPTY.to_owned()
    } else {
        built.markdown
    })
}

fn remember(args: &Value, ctx: &McpCtx) -> Result<String, String> {
    let text = str_arg(args, "text")?.trim();
    if text.is_empty() {
        return Err("`text` must not be empty".to_owned());
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!(
            "`text` is over {MAX_TEXT_CHARS} characters; save one short, atomic memory"
        ));
    }
    let kind = match opt_str(args, "kind")? {
        None => Kind::Fact,
        Some(k) if KINDS.contains(&k) => k.parse().map_err(|e| format!("`kind`: {e}"))?,
        Some(k) => {
            return Err(format!(
                "unknown kind `{k}`; use one of {}",
                KINDS.join(", ")
            ));
        }
    };
    if let Some(extra) = args
        .as_object()
        .and_then(|o| o.keys().find(|k| !matches!(k.as_str(), "text" | "kind")))
    {
        return Err(format!(
            "unknown argument `{extra}`; remember takes text and kind"
        ));
    }
    // Always this project: an agent (possibly prompt-injected) must not plant
    // memories that get injected into every other project. Global memories
    // come only from the human's own words via extraction.
    let scope = match &ctx.project {
        Some(p) if !p.is_empty() => Scope::Project(p.clone()),
        _ => Scope::Global,
    };
    // Redact before anything is persisted.
    let clean = redact(text).text.into_owned();
    let new = NewMemory {
        kind,
        text: clean,
        scope,
        confidence: CONFIRMED_CONFIDENCE,
        importance: REMEMBER_IMPORTANCE,
    };
    let prov = Provenance {
        harness: Some("mcp".to_owned()),
        ..Provenance::new(Origin::Remember)
    };
    let mut store = ops::open_or_create(&ctx.data_dir).map_err(|e| e.to_string())?;
    let outcome = store.insert(&new, &prov).map_err(|e| e.to_string())?;
    let uid8 = ops::prefix(outcome.uid(), 8);
    Ok(match outcome {
        Insert::Inserted(_) => format!("Saved {uid8}"),
        Insert::Duplicate(_) => format!("Already known {uid8}"),
    })
}

/// Whether an MCP client working in `ctx.project` may see or change `m`.
/// The server is bound to one project, so a (possibly prompt-injected) agent
/// in repo A can't read or archive repo B's memories by id prefix.
fn visible(m: &Memory, ctx: &McpCtx) -> bool {
    match (&m.scope, ctx.project.as_deref()) {
        (Scope::Global, _) => true,
        (Scope::Project(p) | Scope::Module { project: p, .. }, Some(cur)) => p == cur,
        (_, None) => false,
    }
}

/// Memories matching a uid prefix that this project's client may access.
fn resolve(store: &crate::store::Store, id: &str, ctx: &McpCtx) -> Result<Vec<Memory>, String> {
    let found = store
        .memories_by_uid_prefix(id)
        .map_err(|e| e.to_string())?;
    Ok(found.into_iter().filter(|m| visible(m, ctx)).collect())
}

fn review(args: &Value, ctx: &McpCtx) -> Result<String, String> {
    let id = str_arg(args, "id")?;
    let action = str_arg(args, "action")?;
    if !matches!(action, "confirm" | "forget") {
        return Err("`action` must be `confirm` or `forget`".to_owned());
    }
    let mut store = ops::open_existing(&ctx.data_dir)?;
    // Global memories are read-only over MCP: they reach every project, so
    // only the human (CLI) may confirm or forget them.
    let mut found: Vec<Memory> = resolve(&store, id, ctx)?
        .into_iter()
        .filter(|m| !matches!(m.scope, Scope::Global))
        .collect();
    let m: Memory = match found.len() {
        0 => return Err(format!("No memory matches {id}")),
        1 => found.remove(0),
        n => {
            let list = found
                .iter()
                .take(MAX_CANDIDATES)
                .map(|m| {
                    format!(
                        "{} {} {}",
                        ops::prefix(&m.uid, 8),
                        m.kind,
                        ops::truncate_chars(&m.text, 60)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "{id} matches {n} memories; use a longer id: {list}"
            ));
        }
    };
    let uid8 = ops::prefix(&m.uid, 8);
    if action == "forget" {
        store.archive(&m.uid).map_err(|e| e.to_string())?;
        return Ok(format!(
            "Forgot {uid8} (archived, reversible: restore with review confirm)"
        ));
    }
    store
        .set_status(&m.uid, Status::Active)
        .map_err(|e| e.to_string())?;
    store
        .set_confidence(&m.uid, m.confidence.max(CONFIRMED_CONFIDENCE))
        .map_err(|e| e.to_string())?;
    Ok(format!("Confirmed {uid8}"))
}

fn why(args: &Value, ctx: &McpCtx) -> Result<String, String> {
    let id = str_arg(args, "id")?;
    let store = ops::open_existing(&ctx.data_dir)?;
    let found = resolve(&store, id, ctx)?;
    let [m] = found.as_slice() else {
        return Err(if found.is_empty() {
            format!("No memory matches {id}")
        } else {
            format!("{id} matches {} memories; use a longer id", found.len())
        });
    };
    let sources = !matches!(m.scope, Scope::Global);
    match ops::why(&store, &m.uid, false, sources)? {
        Why::Found(text) => Ok(text),
        Why::Ambiguous(text) => Err(text.trim_end().to_owned()),
        Why::Missing(_) => Err(format!("No memory matches {id}")),
    }
}
