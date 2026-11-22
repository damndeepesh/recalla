//! Log-derived fields never reach the terminal raw: `why` (text, JSON, MCP)
//! and `stats` strip control characters and newlines.

#![allow(clippy::unwrap_used)]

mod common;

use std::io::Cursor;

use common::Env;
use recalla::mcp::{McpCtx, serve};
use recalla::model::{Anchor, Kind, NewMemory, Origin, Provenance, Scope};
use recalla::store::Via;
use serde_json::{Value, json};

const PROJECT: &str = "/work/demo";
const HOSTILE: &str = "\u{1b}[2J\n## X\u{202e}\u{200b}";

fn bad(s: &str) -> bool {
    s.chars()
        .any(|c| (c.is_control() && c != '\n') || c == '\u{202e}' || c == '\u{200b}')
}

fn no_forged_lines(s: &str) {
    assert!(!bad(s), "{s:?}");
    assert!(s.lines().all(|l| !l.starts_with("## X")), "{s:?}");
}

fn seed(env: &Env) -> String {
    let mut s = recalla::ops::open_or_create(&env.data).unwrap();
    let new = NewMemory {
        kind: Kind::Fact,
        text: format!("text {HOSTILE}"),
        scope: Scope::Project(PROJECT.into()),
        confidence: 0.5,
        importance: 1,
    };
    let prov = Provenance {
        harness: Some(HOSTILE.into()),
        session_id: Some(HOSTILE.into()),
        source_path: Some(format!("/x/{HOSTILE}.jsonl")),
        line: Some(1),
        ..Provenance::new(Origin::Backfill)
    };
    let uid = s.insert(&new, &prov).unwrap().uid().to_owned();
    s.add_anchor(
        &uid,
        &Anchor {
            symbol: Some(HOSTILE.into()),
            ..Anchor::new(format!("src/{HOSTILE}.rs"))
        },
    )
    .unwrap();
    let key = format!("/work/{HOSTILE}");
    s.record_injection(Some(&key), 100, 10, std::slice::from_ref(&uid), Via::Cli)
        .unwrap();
    uid[..8].to_owned()
}

#[test]
fn why_text_and_json_are_clean() {
    let env = Env::new();
    let id = seed(&env);
    for args in [vec!["why", &id], vec!["why", &id, "--json"]] {
        let (code, out, _) = env.cli(&args);
        assert_eq!(code, 0);
        no_forged_lines(&out);
        // JSON escapes controls on the wire: they must not even be encoded.
        assert!(!out.contains("u001b") && !out.contains("u202e"), "{out}");
    }
}

#[test]
fn mcp_why_is_clean() {
    let env = Env::new();
    let id = seed(&env);
    let ctx = McpCtx {
        data_dir: env.data.clone(),
        project: Some(PROJECT.into()),
    };
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "why", "arguments": {"id": id}}});
    let mut out = Vec::new();
    serve(Cursor::new(format!("{msg}\n")), &mut out, &ctx).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("anchors"), "{text}");
    no_forged_lines(text);
}

#[test]
fn stats_project_keys_are_clean() {
    let env = Env::new();
    seed(&env);
    for args in [vec!["stats"], vec!["stats", "--json"]] {
        let (code, out, _) = env.cli(&args);
        assert_eq!(code, 0);
        no_forged_lines(&out);
        assert!(out.contains("/work/"));
    }
}
