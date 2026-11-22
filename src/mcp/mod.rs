//! Minimal MCP server over stdio: newline-delimited JSON-RPC 2.0, one
//! synchronous loop (see ADR D-017). stdout carries protocol messages only.
//!
//! Two protocol eras are served at once: the stateless 2026-07-28 revision
//! (every request carries its version in `params._meta`) and the 2025-11-25
//! `initialize` handshake. Notifications (no `id`) never get a response.

mod tools;

use std::io::{self, BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use serde_json::{Map, Value, json};

/// Revisions this server speaks, newest first.
pub const SUPPORTED: [&str; 2] = ["2026-07-28", "2025-11-25"];
const LEGACY: &str = "2025-11-25";
const VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";
const SERVER_INFO_META: &str = "io.modelcontextprotocol/serverInfo";
const INSTRUCTIONS: &str = "Recalla: local memory for this project. Call `recall` at the start of a task; `remember` durable decisions/preferences; `why` to check a memory's source.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const UNSUPPORTED_VERSION: i64 = -32022;

/// What the tools operate on. The store is opened per call, never held.
#[derive(Debug, Clone)]
pub struct McpCtx {
    pub data_dir: PathBuf,
    /// Project key memories are read from and saved to.
    pub project: Option<String>,
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Serves requests from `input` until it ends, writing one compact JSON
/// response per line to `output` (flushed after each).
///
/// # Errors
/// Only I/O errors on `input` or `output`; bad messages get JSON-RPC errors.
pub fn serve(mut input: impl BufRead, mut output: impl Write, ctx: &McpCtx) -> io::Result<()> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if input.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let response = catch_unwind(AssertUnwindSafe(|| process(line, ctx))).unwrap_or_else(|_| {
            Some(error_response(
                &Value::Null,
                &RpcError::new(INTERNAL_ERROR, "Internal error"),
            ))
        });
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
}

fn error_response(id: &Value, e: &RpcError) -> Value {
    let mut error = json!({"code": e.code, "message": e.message});
    if let Some(data) = &e.data {
        error["data"] = data.clone();
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

/// Handles one line; `None` for notifications.
fn process(line: &str, ctx: &McpCtx) -> Option<Value> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Some(error_response(
            &Value::Null,
            &RpcError::new(PARSE_ERROR, "Parse error"),
        ));
    };
    let invalid =
        |id: &Value| error_response(id, &RpcError::new(INVALID_REQUEST, "Invalid Request"));
    let Some(obj) = msg.as_object() else {
        return Some(invalid(&Value::Null));
    };
    let id = match obj.get("id") {
        None => return None, // notification (or stray response): never answered
        Some(id @ (Value::String(_) | Value::Number(_) | Value::Null)) => id,
        Some(_) => return Some(invalid(&Value::Null)),
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Some(invalid(id));
    };
    let params = obj.get("params").unwrap_or(&Value::Null);
    Some(match handle(method, params, ctx) {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(e) => error_response(id, &e),
    })
}

fn handle(method: &str, params: &Value, ctx: &McpCtx) -> Result<Value, RpcError> {
    let modern = match params.get("_meta").and_then(|m| m.get(VERSION_META)) {
        None => false,
        Some(v) if v.as_str().is_some_and(|s| SUPPORTED.contains(&s)) => true,
        Some(v) => {
            return Err(RpcError {
                code: UNSUPPORTED_VERSION,
                message: "Unsupported protocol version".to_owned(),
                data: Some(json!({"supported": SUPPORTED, "requested": v})),
            });
        }
    };
    let mut result = match method {
        "server/discover" => return Ok(discover()),
        "initialize" => initialize(params),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools::list()}),
        "tools/call" => call_tool(params, ctx)?,
        _ => {
            return Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("Method not found: {method}"),
            ));
        }
    };
    if modern && method != "initialize" {
        // Older clients may reject unknown fields, so only 2026 requests get this.
        if let Some(obj) = result.as_object_mut() {
            obj.insert("resultType".to_owned(), json!("complete"));
        }
    }
    Ok(result)
}

fn discover() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": SUPPORTED,
        "capabilities": {"tools": {}},
        "_meta": {SERVER_INFO_META: {"name": "recalla", "version": crate::VERSION}},
        "instructions": INSTRUCTIONS,
        "ttlMs": 3_600_000,
        "cacheScope": "public",
    })
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| SUPPORTED.contains(v))
        .unwrap_or(LEGACY);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "recalla", "version": crate::VERSION},
        "instructions": INSTRUCTIONS,
    })
}

fn call_tool(params: &Value, ctx: &McpCtx) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "`name` is required"))?;
    let empty = Value::Object(Map::new());
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(a) if a.is_object() => a,
        Some(_) => {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "`arguments` must be an object",
            ));
        }
    };
    let outcome = tools::call(name, args, ctx)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("Unknown tool: {name}")))?;
    let (text, is_error) = match outcome {
        Ok(text) => (text, false),
        Err(message) => (message, true),
    };
    Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
}

#[cfg(test)]
mod tests;
