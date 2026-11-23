//! A tiny in-memory MCP memory server (`membench fake-mcp`). It stands in for
//! a third-party tool in tests and in the smoke benchmark, so the generic
//! adapter is exercised without installing anything.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::matching::words;

pub fn serve() -> Result<(), String> {
    let mut store: Vec<String> = Vec::new();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = req.get("id").cloned() else {
            continue; // notification
        };
        let result = match req["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-memory", "version": "0"},
            }),
            Some("tools/call") => call(&mut store, &req["params"]),
            _ => json!({}),
        };
        writeln!(
            out,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        )
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn text(t: &str) -> Value {
    json!({"content": [{"type": "text", "text": t}], "isError": false})
}

fn call(store: &mut Vec<String>, p: &Value) -> Value {
    let args = &p["arguments"];
    match p["name"].as_str() {
        Some("store") => {
            store.push(args["text"].as_str().unwrap_or_default().to_owned());
            text("stored")
        }
        Some("search") => {
            let q = words(args["query"].as_str().unwrap_or_default());
            let mut hits: Vec<(usize, &String)> = store
                .iter()
                .map(|s| (words(s).intersection(&q).count(), s))
                .filter(|(n, _)| *n > 0)
                .collect();
            hits.sort_by_key(|h| std::cmp::Reverse(h.0));
            text(
                &hits
                    .iter()
                    .take(10)
                    .map(|(_, s)| format!("- {s}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        _ => json!({"content": [{"type": "text", "text": "unknown tool"}], "isError": true}),
    }
}
