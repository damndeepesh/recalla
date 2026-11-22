use std::io::Cursor;

use serde_json::{Value, json};

use super::*;
use crate::pack::est_tokens;

fn ctx(dir: &tempfile::TempDir) -> McpCtx {
    McpCtx {
        data_dir: dir.path().join("data"),
        project: Some("/work/demo".to_owned()),
    }
}

/// Runs `lines` through the server; returns the raw output lines.
fn session(ctx: &McpCtx, lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    serve(Cursor::new(lines.join("\n")), &mut out, ctx).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn run(ctx: &McpCtx, msgs: &[Value]) -> Vec<Value> {
    let lines: Vec<String> = msgs.iter().map(Value::to_string).collect();
    session(ctx, &lines)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn req(id: i64, method: &str, params: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn modern(extra: Value) -> Value {
    let mut p = extra;
    p["_meta"] = json!({VERSION_META: "2026-07-28"});
    p
}

fn text(resp: &Value) -> &str {
    resp["result"]["content"][0]["text"].as_str().unwrap()
}

fn call(id: i64, name: &str, args: &Value) -> Value {
    req(id, "tools/call", &json!({"name": name, "arguments": args}))
}

#[test]
fn legacy_handshake_then_tools() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&dir);
    let out = run(
        &c,
        &[
            req(
                1,
                "initialize",
                &json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
            ),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            req(2, "tools/list", &json!({})),
            call(3, "recall", &json!({})),
        ],
    );
    assert_eq!(out.len(), 3, "the notification must not be answered");
    let init = &out[0]["result"];
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["serverInfo"]["name"], "recalla");
    assert_eq!(init["capabilities"], json!({"tools": {}}));
    assert!(init["instructions"].as_str().unwrap().contains("`recall`"));
    let tools = out[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    assert!(out[1]["result"].get("resultType").is_none());
    assert_eq!(text(&out[2]), "No memories yet for this project.");
    assert_eq!(out[2]["result"]["isError"], false);
    assert!(out[2]["result"].get("resultType").is_none());
    assert_eq!(out[2]["id"], 3);
}

#[test]
fn initialize_falls_back_to_legacy_version() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        &ctx(&dir),
        &[req(
            1,
            "initialize",
            &json!({"protocolVersion": "1999-01-01"}),
        )],
    );
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn modern_discover_and_call() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        &ctx(&dir),
        &[
            req(1, "server/discover", &json!({})),
            req(
                2,
                "tools/call",
                &modern(json!({"name": "recall", "arguments": {}})),
            ),
            req(3, "tools/list", &modern(json!({}))),
        ],
    );
    let d = &out[0]["result"];
    assert_eq!(d["resultType"], "complete");
    assert_eq!(d["supportedVersions"], json!(SUPPORTED));
    assert_eq!(d["_meta"][SERVER_INFO_META]["name"], "recalla");
    assert_eq!(d["ttlMs"], 3_600_000);
    assert_eq!(d["cacheScope"], "public");
    assert_eq!(out[1]["result"]["resultType"], "complete");
    assert_eq!(out[1]["result"]["isError"], false);
    assert_eq!(out[2]["result"]["resultType"], "complete");
}

#[test]
fn unsupported_version_is_32022() {
    let dir = tempfile::tempdir().unwrap();
    let bad = json!({"_meta": {VERSION_META: "2030-01-01"}});
    let out = run(&ctx(&dir), &[req(7, "tools/list", &bad)]);
    assert_eq!(out[0]["id"], 7);
    assert_eq!(out[0]["error"]["code"], -32022);
    assert_eq!(out[0]["error"]["data"]["requested"], "2030-01-01");
    assert_eq!(out[0]["error"]["data"]["supported"], json!(SUPPORTED));
}

#[test]
fn malformed_input_errors() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = [
        "{not json",
        "[1,2]",
        r#"{"jsonrpc":"2.0","id":4}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":42}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"nope"}"#,
        r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"nope"}}"#,
        r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"arguments":{}}}"#,
    ]
    .map(str::to_owned)
    .to_vec();
    let out = session(&ctx(&dir), &lines);
    assert_eq!(
        out[0],
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#
    );
    let v: Vec<Value> = out
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let codes: Vec<i64> = v
        .iter()
        .map(|r| r["error"]["code"].as_i64().unwrap())
        .collect();
    assert_eq!(
        codes,
        [-32700, -32600, -32600, -32600, -32601, -32602, -32602]
    );
    assert_eq!(v[2]["id"], 4);
}

#[test]
fn notifications_and_blank_lines_produce_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = [
        "",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","method":"nope"}"#,
        "   ",
    ]
    .map(str::to_owned)
    .to_vec();
    assert!(session(&ctx(&dir), &lines).is_empty());
}

#[test]
fn many_requests_one_line_each() {
    let dir = tempfile::tempdir().unwrap();
    let msgs: Vec<Value> = (1..=5).map(|i| req(i, "ping", &json!({}))).collect();
    let out = run(&ctx(&dir), &msgs);
    assert_eq!(out.len(), 5);
    for (i, r) in out.iter().enumerate() {
        assert_eq!(r["id"], i64::try_from(i).unwrap() + 1);
        assert_eq!(r["result"], json!({}));
    }
}

#[test]
fn invalid_utf8_is_a_parse_error_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let mut out = Vec::new();
    let input: &[u8] = b"\xff\xfe\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
    serve(input, &mut out, &ctx(&dir)).unwrap();
    assert_eq!(String::from_utf8(out).unwrap().lines().count(), 2);
}

#[test]
fn tools_list_fits_the_token_budget() {
    let n = est_tokens(&serde_json::to_string(&json!({"tools": tools::list()})).unwrap());
    println!("tools/list est. tokens: {n}");
    assert!(n <= 1000, "tools/list is {n} tokens");
    for t in tools::list().as_array().unwrap() {
        assert_eq!(t["inputSchema"]["additionalProperties"], false);
    }
}

fn tool(c: &McpCtx, name: &str, args: &Value) -> (String, bool) {
    let out = run(c, &[call(1, name, args)]);
    (
        text(&out[0]).to_owned(),
        out[0]["result"]["isError"].as_bool().unwrap(),
    )
}

#[test]
fn remember_recall_why_review_flow() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&dir);
    let (saved, err) = tool(
        &c,
        "remember",
        &json!({"text": " Prefer rustfmt defaults ", "kind": "preference"}),
    );
    assert!(!err);
    let uid8 = saved.strip_prefix("Saved ").unwrap().to_owned();
    assert_eq!(uid8.len(), 8);
    let (again, _) = tool(
        &c,
        "remember",
        &json!({"text": "Prefer rustfmt defaults", "kind": "preference"}),
    );
    assert_eq!(again, format!("Already known {uid8}"));

    let (pack, err) = tool(&c, "recall", &json!({"query": "rustfmt"}));
    assert!(!err && pack.contains("Prefer rustfmt defaults"));

    let (why, err) = tool(&c, "why", &json!({"id": uid8}));
    assert!(!err && why.contains("(remember)"), "{why}");

    let (msg, err) = tool(&c, "review", &json!({"id": uid8, "action": "forget"}));
    assert!(!err && msg.contains("reversible"));
    assert_eq!(
        tool(&c, "recall", &json!({})).0,
        "No memories yet for this project."
    );

    let (msg, _) = tool(&c, "review", &json!({"id": uid8, "action": "confirm"}));
    assert!(msg.starts_with("Confirmed"));
    assert!(tool(&c, "recall", &json!({})).0.contains("rustfmt"));
}

#[test]
fn remember_is_always_project_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&dir);
    let uid = |r: String| r.rsplit(' ').next().unwrap().to_owned();
    let p = uid(tool(&c, "remember", &json!({"text": "scoped fact"})).0);
    // An agent can't plant cross-project memories: `global` is rejected.
    let (msg, err) = tool(
        &c,
        "remember",
        &json!({"text": "global fact", "global": true}),
    );
    assert!(err && msg.contains("unknown argument"), "{msg}");
    let store = crate::ops::open_existing(&c.data_dir).unwrap();
    let scope = |u: &str| store.memories_by_uid_prefix(u).unwrap()[0].scope.clone();
    assert_eq!(scope(&p), crate::model::Scope::Project("/work/demo".into()));
    // With no project at all there is nowhere narrower to put it.
    let no_project = McpCtx { project: None, ..c };
    let n = uid(tool(&no_project, "remember", &json!({"text": "orphan fact"})).0);
    assert_eq!(scope(&n), crate::model::Scope::Global);
}

#[test]
fn tool_input_errors_are_is_error_results() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&dir);
    for args in [
        json!({}),
        json!({"text": "   "}),
        json!({"text": "x".repeat(1001)}),
        json!({"text": "ok", "kind": "bogus"}),
        json!({"text": "ok", "global": "yes"}),
    ] {
        let (msg, err) = tool(&c, "remember", &args);
        assert!(err, "{args}: {msg}");
    }
    assert!(tool(&c, "recall", &json!({"budget": 5})).1);
    assert!(tool(&c, "recall", &json!({"budget": "big"})).1);
    // No DB yet: id-based tools report it instead of creating one.
    assert!(tool(&c, "why", &json!({"id": "abcd"})).1);
    assert!(tool(&c, "review", &json!({"id": "abcd", "action": "forget"})).1);
    assert!(tool(&c, "review", &json!({"id": "abcd", "action": "zap"})).1);
}

#[test]
fn review_reports_missing_and_ambiguous_ids() {
    use crate::model::{Kind, NewMemory, Origin, Provenance, Scope};
    use std::collections::HashMap;

    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&dir);
    let mut store = crate::ops::open_or_create(&c.data_dir).unwrap();
    // Birthday search: ~400 uids collide on 4 hex chars with near certainty.
    let mut by_prefix: HashMap<String, usize> = HashMap::new();
    let mut shared = None;
    for i in 0..3000 {
        let new = NewMemory {
            kind: Kind::Fact,
            text: format!("memory number {i}"),
            // Project-scoped: Global memories are read-only over MCP.
            scope: Scope::Project("/work/demo".into()),
            confidence: 0.5,
            importance: 1,
        };
        let uid = store
            .insert(&new, &Provenance::new(Origin::Remember))
            .unwrap()
            .uid()
            .to_owned();
        let n = by_prefix.entry(uid[..4].to_owned()).or_default();
        *n += 1;
        if *n == 2 {
            shared = Some(uid[..4].to_owned());
            break;
        }
    }
    let shared = shared.expect("no 4-char collision in 3000 uids");
    let (msg, err) = tool(&c, "review", &json!({"id": shared, "action": "forget"}));
    assert!(err && msg.contains("matches 2 memories"), "{msg}");
    assert!(msg.contains("fact memory number"), "{msg}");

    let (msg, err) = tool(&c, "review", &json!({"id": "g000", "action": "forget"}));
    assert!(err && msg.contains("hex"), "{msg}");
    let missing = ["0000", "ffff"]
        .into_iter()
        .find(|p| !by_prefix.contains_key(*p))
        .unwrap();
    let (msg, err) = tool(&c, "review", &json!({"id": missing, "action": "forget"}));
    assert!(
        err && msg == format!("No memory matches {missing}"),
        "{msg}"
    );
    let (msg, err) = tool(&c, "why", &json!({"id": shared}));
    assert!(err && msg.contains("matches 2 memories"), "{msg}");
}

#[test]
fn review_and_why_cannot_reach_other_projects() {
    // A client bound to /work/demo must not read or archive another repo's
    // memories, even with the right uid prefix.
    let dir = tempfile::tempdir().unwrap();
    let demo = ctx(&dir);
    let other = McpCtx {
        project: Some("/work/other".to_owned()),
        data_dir: demo.data_dir.clone(),
    };
    let uid = |r: String| r.rsplit(' ').next().unwrap().to_owned();
    let theirs = uid(tool(
        &other,
        "remember",
        &json!({"text": "other repo secret plan"}),
    )
    .0);
    // A Global memory (as extraction makes from the human's words), stored directly.
    let shared = {
        let mut st = crate::ops::open_existing(&other.data_dir).unwrap();
        let new = crate::model::NewMemory {
            kind: crate::model::Kind::Preference,
            text: "shared rule".into(),
            scope: crate::model::Scope::Global,
            confidence: 0.9,
            importance: 2,
        };
        st.insert(
            &new,
            &crate::model::Provenance::new(crate::model::Origin::Remember),
        )
        .unwrap()
        .uid()
        .to_owned()
    };

    let (msg, err) = tool(&demo, "why", &json!({"id": theirs}));
    assert!(err && msg.contains("No memory matches"), "{msg}");
    let (msg, err) = tool(&demo, "review", &json!({"id": theirs, "action": "forget"}));
    assert!(err && msg.contains("No memory matches"), "{msg}");
    // Still active for its own project.
    let (msg, err) = tool(&other, "why", &json!({"id": theirs}));
    assert!(!err && msg.contains("other repo secret plan"), "{msg}");
    // Global memories stay reachable from any project.
    let (_, err) = tool(&demo, "why", &json!({"id": shared}));
    assert!(!err);
}

#[test]
fn global_memories_are_read_only_and_hide_sources_over_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&dir);
    tool(&c, "remember", &json!({"text": "seed so the db exists"}));
    let shared = {
        let mut st = crate::ops::open_existing(&c.data_dir).unwrap();
        let new = crate::model::NewMemory {
            kind: crate::model::Kind::Preference,
            text: "global rule from elsewhere".into(),
            scope: crate::model::Scope::Global,
            confidence: 0.8,
            importance: 2,
        };
        let prov = crate::model::Provenance {
            source_path: Some("/other/project/session.jsonl".into()),
            line: Some(7),
            ..crate::model::Provenance::new(crate::model::Origin::Backfill)
        };
        st.insert(&new, &prov).unwrap().uid().to_owned()
    };
    let (msg, err) = tool(&c, "review", &json!({"id": shared, "action": "forget"}));
    assert!(err && msg.contains("No memory matches"), "{msg}");
    let (msg, err) = tool(&c, "why", &json!({"id": shared}));
    assert!(!err && msg.contains("global rule from elsewhere"), "{msg}");
    assert!(
        !msg.contains("session.jsonl"),
        "source file must be hidden: {msg}"
    );
}
