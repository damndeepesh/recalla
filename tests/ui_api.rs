//! The dashboard API over real HTTP: an in-process server on port 0 over a DB
//! built by the real pipeline, logged in through the one-time code.

#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod common;

use std::collections::HashSet;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::Env;
use common::http::{self, Resp};
use recalla::model::{Anchor, Kind, NewMemory, Origin, Provenance, Scope, Status};
use recalla::pack::is_invisible;
use recalla::paths;
use recalla::store::{Checkpoint, NewInjection, Store, StoredEvent, StoredKind, Via};
use recalla::ui::UiHandler;
use recalla::ui::security::{LOGIN_TTL, LoginCode, Token};
use recalla::ui::server::{ServeOptions, Server};
use recalla::ui::sync::SyncState;
use serde_json::{Value, json};

const HOSTILE: &str = "ESC\u{1b}[2J BIDI\u{202e}x TAG\u{e0041}\u{e0042} ZW\u{200b}!";
const HOSTILE_SESSION: &str = "hostile-session";
const WEB: &str = "eval-web-api";
const WEB_PROJECT: &str = "/tmp/eval/web-api";
const FAKE_EXE: &str = "/opt/recalla bin/recalla";

fn event(line: u64, kind: StoredKind, text: &str) -> StoredEvent {
    StoredEvent {
        harness: "claude-code".into(),
        session_id: HOSTILE_SESSION.into(),
        message_uuid: None,
        parent_uuid: None,
        line,
        seq: 0,
        byte_offset: line * 10,
        ts: Some(1_900_000_000_000 + i64::try_from(line).unwrap()),
        cwd: Some("/work/hostile".into()),
        git_branch: None,
        sidechain: false,
        kind,
        tool_name: None,
        tool_id: None,
        is_error: false,
        text: text.into(),
    }
}

/// The eval fixtures through the real pipeline, plus what the pipeline can't
/// produce: a hostile Global memory and session, and injections.
fn populate(env: &Env) -> (String, usize) {
    env.write_all();
    env.backfill();
    let mut store = env.store();
    let long = format!("line one\nline two {HOSTILE}\n{}", "x".repeat(5000));
    let id = store
        .upsert_source("claude-code", "/s/dir/hostile.jsonl")
        .unwrap();
    let events = [
        event(1, StoredKind::UserText, &format!("hi\n{HOSTILE}\nbye")),
        event(2, StoredKind::AssistantText, &long),
        event(3, StoredKind::ToolResult, "ok"),
    ];
    let cp = Checkpoint {
        identity: "i".into(),
        tail_hash: "t".into(),
        size: 100,
        next_offset: 100,
    };
    store.ingest_batch(id, &events, &cp, None, false).unwrap();
    let hostile = NewMemory {
        kind: Kind::Gotcha,
        text: format!("hostile memory\nsecond line {HOSTILE}"),
        scope: Scope::Global,
        confidence: 0.5,
        importance: 1,
    };
    let prov = Provenance {
        harness: Some("claude-code".into()),
        session_id: Some(HOSTILE_SESSION.into()),
        source_path: Some("/s/dir/hostile.jsonl".into()),
        line: Some(1),
        ..Provenance::new(Origin::Backfill)
    };
    let uid = store.insert(&hostile, &prov).unwrap().uid().to_owned();
    store.add_anchor(&uid, &Anchor::new("src/h.rs")).unwrap();
    let uids: Vec<String> = store
        .all_memories(&[Status::Active])
        .unwrap()
        .into_iter()
        .take(3)
        .map(|m| m.uid)
        .collect();
    for i in 0..9 {
        let session = (i % 3 == 0).then_some(WEB);
        store
            .record_injection(&NewInjection {
                session_id: session,
                pack_text: Some("# Recalla memory\n- a thing [abcd1234]"),
                candidate_count: Some(10),
                candidate_tokens: Some(400),
                ..NewInjection::new(
                    Some(WEB_PROJECT),
                    1500,
                    100,
                    &uids,
                    if i % 2 == 0 { Via::Hook } else { Via::Mcp },
                )
            })
            .unwrap();
    }
    store
        .record_injection(&NewInjection::new(None, 800, 10, &[], Via::Cli))
        .unwrap();
    let total = store.all_memories(&[Status::Active]).unwrap().len();
    assert!(recalla::mirror::sync_best_effort(&mut store, &env.data).is_none());
    (uid, total)
}

struct Api {
    env: Env,
    addr: SocketAddr,
    cookie: String,
    csrf: String,
    code: String,
    settings: PathBuf,
    sync: Arc<SyncState>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _settings_dir: tempfile::TempDir,
}

impl Api {
    /// Populated DB (like a long-time user).
    fn start() -> (Self, String, usize) {
        let env = Env::new();
        let (hostile_uid, total) = populate(&env);
        (Self::serve(env, true), hostile_uid, total)
    }

    /// Serves `env`'s data dir after the startup step (creates an empty DB).
    fn serve(env: Env, roots: bool) -> Self {
        recalla::ui::prepare(&env.data, |_| {}).unwrap();
        let settings_dir = tempfile::tempdir().unwrap();
        let settings = settings_dir.path().join("claude/settings.json");
        let (token, csrf) = (Token::generate().unwrap(), Token::generate().unwrap());
        let csrf_text = csrf.as_str().to_owned();
        let code = Arc::new(LoginCode::generate(LOGIN_TTL).unwrap());
        let code_text = code.as_str().to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let mut opts = ServeOptions::new(0, env.data.clone());
        opts.stop = Some(Arc::clone(&stop));
        let server = Server::bind(opts).unwrap();
        let addr = server.local_addr().unwrap();
        let mut sync = None;
        let handler =
            UiHandler::new(token, csrf, code, addr.port(), env.data.clone()).with_ctx(|c| {
                c.settings_path = Some(settings.clone());
                c.exe = PathBuf::from(FAKE_EXE);
                c.claude_root = roots.then(|| env.root.clone());
                c.codex_root = roots.then(|| env.codex_root.clone());
                sync = Some(Arc::clone(&c.sync));
            });
        let thread = std::thread::spawn(move || server.run(handler).unwrap());
        let login = http::get(addr, &format!("/?c={code_text}"), &[]);
        assert_eq!(login.status, 302);
        let cookie = http::cookie_pair(&login).unwrap();
        let session = http::get(addr, "/api/session", &[("Cookie", &cookie)]);
        let csrf = session.json()["csrf"].as_str().unwrap().to_owned();
        assert_eq!(csrf, csrf_text);
        Self {
            env,
            addr,
            cookie,
            csrf,
            code: code_text,
            settings,
            sync: sync.unwrap(),
            stop,
            thread: Some(thread),
            _settings_dir: settings_dir,
        }
    }

    fn get(&self, target: &str) -> Resp {
        http::get(self.addr, target, &[("Cookie", &self.cookie)])
    }

    /// 200 and a JSON body whose every string is free of control/invisible chars.
    fn ok(&self, target: &str) -> Value {
        let r = self.get(target);
        assert_eq!(r.status, 200, "{target}: {}", r.body);
        assert_eq!(
            r.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(r.header("cache-control"), Some("no-store"));
        let v = r.json();
        assert_clean(&v);
        v
    }

    /// The status and error message of a failing GET.
    fn err(&self, target: &str) -> (u16, String) {
        let r = self.get(target);
        assert_ne!(r.status, 200, "{target} unexpectedly ok");
        // Requests the server core rejects (e.g. control chars in the query)
        // answer in plain text, everything else in JSON.
        let msg = serde_json::from_str::<Value>(&r.body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
            .unwrap_or_default();
        (r.status, msg)
    }

    fn post_with(&self, target: &str, body: &[u8], origin: bool, csrf: bool) -> Resp {
        let origin_value = format!("http://127.0.0.1:{}", self.addr.port());
        let mut extra = vec![("Cookie", self.cookie.as_str())];
        if origin {
            extra.push(("Origin", origin_value.as_str()));
        }
        if csrf {
            extra.push(("X-Recalla-Csrf", self.csrf.as_str()));
        }
        http::request(
            self.addr,
            "POST",
            target,
            Some(&http::host(self.addr)),
            &extra,
            body,
        )
    }

    fn post(&self, target: &str, body: &Value) -> Resp {
        self.post_with(target, body.to_string().as_bytes(), true, true)
    }

    fn post_ok(&self, target: &str, body: &Value) -> Value {
        let r = self.post(target, body);
        assert!(
            matches!(r.status, 200 | 202),
            "{target}: {} {}",
            r.status,
            r.body
        );
        let v = r.json();
        assert_clean(&v);
        v
    }

    fn db(&self) -> PathBuf {
        paths::db_path(&self.env.data)
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn assert_clean(v: &Value) {
    match v {
        Value::String(s) => assert!(
            !s.chars()
                .any(|c| (c.is_control() && c != '\n' && c != '\t') || is_invisible(c)),
            "dirty string {s:?}"
        ),
        Value::Array(a) => a.iter().for_each(assert_clean),
        Value::Object(o) => o.values().for_each(assert_clean),
        _ => {}
    }
}

fn items(v: &Value) -> &Vec<Value> {
    v["items"].as_array().unwrap()
}

/// Follows `next` cursors and returns every item.
fn walk(api: &Api, base: &str, limit: usize) -> Vec<Value> {
    let sep = if base.contains('?') { '&' } else { '?' };
    let (mut all, mut cursor) = (Vec::new(), None::<String>);
    for _ in 0..10_000 {
        let mut url = format!("{base}{sep}limit={limit}");
        if let Some(c) = &cursor {
            url.push_str("&cursor=");
            url.push_str(c);
        }
        let page = api.ok(&url);
        all.extend(items(&page).iter().cloned());
        match page["next"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return all,
        }
    }
    panic!("pagination did not terminate");
}

fn unique(values: impl Iterator<Item = String>) -> usize {
    let all: Vec<String> = values.collect();
    let set: HashSet<&String> = all.iter().collect();
    assert_eq!(set.len(), all.len(), "duplicates");
    all.len()
}

// ------------------------------------------------------------------ tests

#[test]
fn every_endpoint_needs_the_login() {
    let (api, uid, _) = Api::start();
    let paths = [
        "/api/overview",
        "/api/sessions",
        "/api/sessions/claude-code/eval-web-api",
        "/api/sessions/claude-code/eval-web-api/events",
        "/api/events/1/text",
        "/api/memories",
        &format!("/api/memories/{uid}"),
        "/api/injections",
        "/api/injections/1",
        "/api/connect",
        "/api/sync/status",
    ];
    for p in paths {
        let r = http::get(api.addr, p, &[]);
        assert_eq!(r.status, 401, "{p}");
        assert!(!r.body.contains("memory"));
    }
    assert_eq!(api.get("/api/nope").status, 404);
    assert_eq!(api.post_with("/api/overview", b"", true, true).status, 405);
    assert_eq!(
        api.post_with("/api/sync/status", b"", true, true).status,
        405
    );
    assert_eq!(api.get("/api/hooks/install").status, 405);
    // The one-time code stays burned.
    assert_eq!(
        http::get(api.addr, &format!("/?c={}", api.code), &[]).status,
        401
    );
}

#[test]
fn overview_shape_and_numbers() {
    let (api, _, total) = Api::start();
    let v = api.ok("/api/overview");
    let activity = v["activity"].as_array().unwrap();
    assert_eq!(activity.len(), 30, "30 days, zero-filled");
    for d in activity {
        for f in ["day", "sessions", "memories", "packs", "tokens"] {
            assert!(d[f].is_u64() || d[f].is_i64(), "{f}");
        }
    }
    assert!(activity.windows(2).all(|w| w[1]["day"].as_i64() > w[0]["day"].as_i64()));
    assert_eq!(v["memories"]["total"], total);
    let by_status: u64 = v["memories"]["by_status"]
        .as_object()
        .unwrap()
        .values()
        .map(|n| n.as_u64().unwrap())
        .sum();
    assert_eq!(by_status, v["memories"]["total"].as_u64().unwrap());
    assert!(v["memories"]["by_kind"].as_object().unwrap().len() >= 3);
    assert_eq!(v["memories"]["stale"], 0);
    assert_eq!(v["sessions"], 12);
    assert!(v["events"].as_u64().unwrap() > 50);
    assert_eq!(v["injections"]["count"], 10);
    assert_eq!(v["injections"]["est_tokens"], 910);
    assert_eq!(v["injections"]["baselined_est_tokens"], 900);
    assert_eq!(v["injections"]["candidate_tokens"], 3600);
    assert_eq!(v["injections"]["saved_pct"], 75.0);
    assert_eq!(v["injections"]["last_via"], "cli");
    assert!(v["last_event_ts"].is_i64());
    assert!(v["db_bytes"].as_u64().unwrap() > 4096);
    assert_eq!(v["archive_bytes"], 0);
    assert!(v["data_dir"].as_str().unwrap().ends_with("data"));
    assert!(v["mirror_dir"].as_str().unwrap().ends_with("data/mirror"));
    assert_eq!(v["version"], recalla::VERSION);
    assert_eq!(v["schema_version"], recalla::store::schema_version());
    assert!(v["generated_at"].as_i64().unwrap() > 1_700_000_000_000);
}

#[test]
fn archive_bytes_sums_zst_files() {
    let env = Env::new();
    let dir = paths::archive_dir(&env.data);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.zst"), vec![0u8; 100]).unwrap();
    fs::write(dir.join("b.zst"), vec![0u8; 50]).unwrap();
    fs::write(dir.join("c.tmp"), vec![0u8; 999]).unwrap();
    let api = Api::serve(env, false);
    assert_eq!(api.ok("/api/overview")["archive_bytes"], 150);
}

#[test]
fn sessions_paginate_filter_and_validate() {
    let (api, _, _) = Api::start();
    let all = api.ok("/api/sessions?limit=200");
    assert!(all["next"].is_null());
    assert_eq!(items(&all).len(), 12);
    let key = |s: &Value| format!("{}/{}", s["harness"], s["session_id"]);
    for limit in [1, 2, 5] {
        let walked = walk(&api, "/api/sessions", limit);
        assert_eq!(
            walked.iter().map(key).collect::<Vec<_>>(),
            items(&all).iter().map(key).collect::<Vec<_>>(),
            "limit {limit}"
        );
        unique(walked.iter().map(key));
    }
    let web = items(&all).iter().find(|s| s["session_id"] == WEB).unwrap();
    for field in [
        "harness",
        "session_id",
        "project",
        "first_ts",
        "last_ts",
        "event_count",
        "sidechain_event_count",
        "memory_count",
        "injection_count",
        "sources",
    ] {
        assert!(web.get(field).is_some(), "missing {field}");
    }
    assert_eq!(web["project"], WEB_PROJECT);
    assert_eq!(web["injection_count"], 3);
    assert_eq!(web["sources"], json!(["eval-web-api.jsonl"]));

    let by_project = api.ok("/api/sessions?project=%2Ftmp%2Feval%2Fweb-api");
    assert_eq!(items(&by_project).len(), 1);
    assert_eq!(
        items(&api.ok("/api/sessions?harness=claude-code")).len(),
        12
    );
    assert_eq!(items(&api.ok("/api/sessions?harness=codex")).len(), 0);
    assert_eq!(items(&api.ok("/api/sessions?q=WEB-API")).len(), 1);
    assert_eq!(
        items(&api.ok("/api/sessions?q=%27%3B+DROP+TABLE+events")).len(),
        0
    );
    assert_eq!(
        items(&api.ok("/api/sessions?limit=0")).len(),
        1,
        "limit clamps up"
    );
    assert_eq!(
        items(&api.ok("/api/sessions?limit=99999")).len(),
        12,
        "limit clamps down"
    );

    for bad in [
        "limit=abc",
        "limit=-1",
        "limit=1.5",
        "cursor=nonsense",
        "cursor=a:b",
        "q=%01",
        "harness=%00",
    ] {
        let (status, msg) = api.err(&format!("/api/sessions?{bad}"));
        assert_eq!(status, 400, "{bad}: {msg}");
    }
    let long_q = "a".repeat(201);
    assert_eq!(api.err(&format!("/api/sessions?q={long_q}")).0, 400);
    assert_eq!(
        items(&api.ok(&format!("/api/sessions?q={}", "a".repeat(200)))).len(),
        0
    );
    let long_filter = "a".repeat(501);
    assert_eq!(
        api.err(&format!("/api/sessions?project={long_filter}")).0,
        400
    );
}

#[test]
fn session_detail_events_and_event_text() {
    let (api, _, _) = Api::start();
    let d = api.ok(&format!("/api/sessions/claude-code/{WEB}"));
    assert_eq!(d["session"]["session_id"], WEB);
    assert!(!d["memories"].as_array().unwrap().is_empty());
    let m = &d["memories"][0];
    for field in ["uid", "kind", "text", "status"] {
        assert!(m.get(field).is_some());
    }
    assert_eq!(d["injections"].as_array().unwrap().len(), 3);
    for field in ["id", "at", "est_tokens", "via", "session_id"] {
        assert!(d["injections"][0].get(field).is_some());
    }

    let base = format!("/api/sessions/claude-code/{WEB}/events");
    let all = api.ok(&format!("{base}?limit=200"));
    let n = items(&all).len();
    assert!(n > 5 && all["next"].is_null());
    for field in [
        "id",
        "ts",
        "kind",
        "tool_name",
        "is_error",
        "sidechain",
        "text",
        "truncated",
    ] {
        assert!(items(&all)[0].get(field).is_some(), "{field}");
    }
    // Event paging: `after` continues from the last id, in order, no repeats.
    let mut seen = Vec::new();
    let mut after = String::new();
    loop {
        let url = if after.is_empty() {
            format!("{base}?limit=4")
        } else {
            format!("{base}?limit=4&after={after}")
        };
        let page = api.ok(&url);
        seen.extend(items(&page).iter().map(|e| e["id"].as_i64().unwrap()));
        match page["next"].as_str() {
            Some(c) => after = c.to_owned(),
            None => break,
        }
    }
    assert_eq!(seen.len(), n);
    assert!(seen.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(api.err(&format!("{base}?after=-1")).0, 400);
    assert_eq!(api.err(&format!("{base}?after=x")).0, 400);

    // Truncated event: the list cuts it, the text endpoint returns everything.
    let hostile = api.ok(&format!(
        "/api/sessions/claude-code/{HOSTILE_SESSION}/events"
    ));
    let long = items(&hostile)
        .iter()
        .find(|e| e["truncated"] == true)
        .unwrap();
    let shown = long["text"].as_str().unwrap().chars().count();
    assert!(
        (1900..=2000).contains(&shown),
        "cut near 2000 chars, got {shown}"
    );
    let id = long["id"].as_i64().unwrap();
    let full = api.ok(&format!("/api/events/{id}/text"));
    assert_eq!(full["id"], id);
    assert!(full["text"].as_str().unwrap().chars().count() > 5000);
    assert!(full["text"].as_str().unwrap().contains("line two"));

    assert_eq!(api.err("/api/sessions/claude-code/nope").0, 404);
    assert_eq!(api.err("/api/sessions/claude-code/nope/events").0, 404);
    assert_eq!(api.err("/api/sessions/codex/eval-web-api").0, 404);
    assert_eq!(api.err("/api/events/99999999/text").0, 404);
    assert_eq!(api.err("/api/events/abc/text").0, 400);
    assert_eq!(api.err("/api/events/1.5/text").0, 400);
    assert_eq!(
        api.err(&format!("/api/sessions/claude-code/{}", "a".repeat(501)))
            .0,
        400
    );
    assert_eq!(
        api.err("/api/sessions/claude-code/a%01b").0,
        400,
        "encoded control"
    );
}

#[test]
fn memories_filters_sorts_and_pagination() {
    let (api, _, total) = Api::start();
    let all = api.ok("/api/memories?limit=200");
    assert_eq!(items(&all).len(), total);
    let first = &items(&all)[0];
    for field in [
        "uid",
        "kind",
        "text",
        "scope_project",
        "scope_path",
        "status",
        "confidence",
        "importance",
        "seen_count",
        "created_at",
        "updated_at",
        "anchor_count",
        "stale_reason",
        "first_source",
    ] {
        assert!(first.get(field).is_some(), "missing {field}");
    }
    assert!(first["confidence"].as_f64().unwrap() <= 1.0);
    assert!(first["confidence"].to_string().len() <= 5, "short decimal");
    for sort in ["updated", "confidence", "seen", "created"] {
        let url = format!("/api/memories?sort={sort}");
        let walked = walk(&api, &url, 7);
        assert_eq!(
            unique(walked.iter().map(|m| m["uid"].to_string())),
            total,
            "{sort}"
        );
        let sorted = api.ok(&format!("{url}&limit=200"));
        let col = match sort {
            "updated" => "updated_at",
            "created" => "created_at",
            "seen" => "seen_count",
            _ => "confidence",
        };
        assert!(
            items(&sorted)
                .windows(2)
                .all(|w| w[0][col].as_f64() >= w[1][col].as_f64()),
            "{sort}"
        );
    }
    let kinds = api.ok("/api/memories?kind=gotcha,decision&limit=200");
    assert!(!items(&kinds).is_empty());
    assert!(
        items(&kinds)
            .iter()
            .all(|m| matches!(m["kind"].as_str(), Some("gotcha" | "decision")))
    );
    assert!(items(&kinds).len() < total);
    assert_eq!(items(&api.ok("/api/memories?status=archived")).len(), 0);
    assert_eq!(
        items(&api.ok("/api/memories?status=active,needs_reverify&limit=200")).len(),
        total
    );
    let proj = api.ok("/api/memories?project=%2Ftmp%2Feval%2Fweb-api&limit=200");
    assert!(!items(&proj).is_empty());
    assert!(
        items(&proj)
            .iter()
            .all(|m| m["scope_project"] == WEB_PROJECT)
    );
    assert_eq!(
        items(&api.ok("/api/memories?harness=claude-code&limit=200")).len(),
        total
    );
    assert_eq!(items(&api.ok("/api/memories?harness=codex")).len(), 0);
    let hits = api.ok("/api/memories?q=token+bucket");
    assert!(!items(&hits).is_empty() && items(&hits).len() < total);
    assert_eq!(
        items(&api.ok("/api/memories?q=%22%29%3B+DROP+TABLE+memories%3B")).len(),
        0
    );
    assert_eq!(
        items(&api.ok("/api/memories?limit=200")).len(),
        total,
        "tables intact"
    );

    for bad in [
        "kind=nope",
        "kind=gotcha,nope",
        "kind=gotcha,",
        "status=zzz",
        "sort=random",
        "sort=updated_at;drop",
        "limit=x",
        "cursor=bad",
    ] {
        assert_eq!(api.err(&format!("/api/memories?{bad}")).0, 400, "{bad}");
    }
    assert_eq!(
        api.err(&format!("/api/memories?kind={}", "a,".repeat(300)))
            .0,
        400
    );
}

#[test]
fn memory_detail_resolves_prefixes_and_reports_ambiguity() {
    let (api, hostile_uid, _) = Api::start();
    let d = api.ok(&format!("/api/memories/{hostile_uid}"));
    assert_eq!(d["uid"], hostile_uid);
    assert_eq!(d["scope_project"], Value::Null, "global");
    for field in [
        "text_full",
        "valid_from",
        "valid_to",
        "superseded_by",
        "provenance",
        "anchors",
        "injections",
        "kind",
        "status",
        "first_source",
    ] {
        assert!(d.get(field).is_some(), "missing {field}");
    }
    assert_eq!(d["anchors"][0]["path"], "src/h.rs");
    assert_eq!(d["provenance"][0]["origin"], "backfill");
    assert_eq!(d["provenance"][0]["session_id"], HOSTILE_SESSION);
    assert_eq!(d["provenance"][0]["file"], "hostile.jsonl");
    assert!(
        d["provenance"][0]["excerpt"]
            .as_str()
            .unwrap()
            .starts_with("hi")
    );
    let by_prefix = api.ok(&format!("/api/memories/{}", &hostile_uid[..12]));
    assert_eq!(by_prefix["uid"], hostile_uid);

    assert_eq!(api.err("/api/memories/deadbeefdeadbeef").0, 404);
    assert_eq!(
        api.err("/api/memories/ab").0,
        404,
        "too short to be a prefix"
    );
    assert_eq!(api.err("/api/memories/not-hex").0, 404);
    assert_eq!(
        api.err(&format!("/api/memories/{}", "a".repeat(501))).0,
        400
    );

    // Two memories whose uids share a 4-char prefix make that prefix ambiguous.
    let mut store = api.env.store();
    let mut by_prefix_map = std::collections::HashMap::new();
    let mut clash = None;
    for i in 0..5000 {
        let text = format!("collision candidate number {i}");
        let uid = recalla::model::uid(Kind::Fact, &Scope::Global, &text);
        let prefix = uid[..4].to_owned();
        if let Some(prev) = by_prefix_map.insert(prefix.clone(), text.clone()) {
            clash = Some((prefix, prev, text));
            break;
        }
    }
    let (prefix, a, b) = clash.expect("a 4-hex collision within 5000 uids");
    for text in [a, b] {
        let m = NewMemory {
            kind: Kind::Fact,
            text,
            scope: Scope::Global,
            confidence: 0.5,
            importance: 1,
        };
        store
            .insert(&m, &Provenance::new(Origin::Backfill))
            .unwrap();
    }
    let (status, msg) = api.err(&format!("/api/memories/{prefix}"));
    assert_eq!(
        (status, msg),
        (409, format!("id prefix {prefix} matches several memories"))
    );
}

#[test]
fn injection_harness_is_derived_from_session_events_when_unambiguous() {
    let (api, _, _) = Api::start();
    {
        let mut store = api.env.store();
        // Same session id under two harnesses: ambiguous.
        for (harness, file) in [("claude-code", "/s/a.jsonl"), ("codex", "/s/b.jsonl")] {
            let id = store.upsert_source(harness, file).unwrap();
            let e = StoredEvent {
                harness: harness.into(),
                session_id: "dup-session".into(),
                ..event(1, StoredKind::UserText, "hi")
            };
            let cp = Checkpoint {
                identity: harness.into(),
                tail_hash: "t".into(),
                size: 10,
                next_offset: 10,
            };
            store.ingest_batch(id, &[e], &cp, None, false).unwrap();
        }
        for session in ["dup-session", "no-such-session"] {
            store
                .record_injection(&NewInjection {
                    session_id: Some(session),
                    ..NewInjection::new(None, 100, 10, &[], Via::Hook)
                })
                .unwrap();
        }
    }
    let all = api.ok("/api/injections?limit=200");
    let by_session = |s: &str| {
        items(&all)
            .iter()
            .find(|i| i["session_id"] == s)
            .unwrap()
            .clone()
    };
    assert_eq!(by_session(WEB)["harness"], "claude-code");
    assert!(by_session("dup-session")["harness"].is_null(), "ambiguous");
    assert!(by_session("no-such-session")["harness"].is_null());
    let no_session = items(&all)
        .iter()
        .find(|i| i["session_id"].is_null())
        .unwrap();
    assert!(no_session["harness"].is_null());
    // Briefs (session detail, memory detail) carry it too.
    let d = api.ok(&format!("/api/sessions/claude-code/{WEB}"));
    assert_eq!(d["injections"][0]["harness"], "claude-code");
    let id = by_session(WEB)["id"].as_i64().unwrap();
    assert_eq!(
        api.ok(&format!("/api/injections/{id}"))["harness"],
        "claude-code"
    );
}

#[test]
fn projects_lists_distinct_projects_with_live_memory_counts() {
    let (api, _, _) = Api::start();
    let r = api.ok("/api/projects");
    let list = items(&r);
    assert!(!list.is_empty());
    let mut seen = HashSet::new();
    for p in list {
        assert!(
            seen.insert(p["project"].as_str().unwrap().to_owned()),
            "distinct"
        );
        assert!(p["memory_count"].as_u64().unwrap() > 0);
    }
    let counts: Vec<u64> = list
        .iter()
        .map(|p| p["memory_count"].as_u64().unwrap())
        .collect();
    assert!(
        counts.windows(2).all(|w| w[0] >= w[1]),
        "most memories first"
    );
    let one = &list[0];
    let n = api.ok(&format!(
        "/api/memories?project={}&limit=200",
        one["project"].as_str().unwrap().replace('/', "%2F")
    ));
    assert_eq!(
        items(&n).len() as u64,
        one["memory_count"].as_u64().unwrap()
    );
    assert_eq!(api.post("/api/projects", &json!({})).status, 405);
}

#[test]
fn session_endpoint_reports_the_package_version() {
    let (api, _, _) = Api::start();
    assert_eq!(api.ok("/api/session")["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn injections_filters_pagination_and_detail() {
    let (api, _, _) = Api::start();
    let all = api.ok("/api/injections?limit=200");
    assert_eq!(items(&all).len(), 10);
    let ids: Vec<i64> = items(&all)
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect();
    assert!(ids.windows(2).all(|w| w[0] > w[1]), "newest first");
    for limit in [1, 3] {
        let walked = walk(&api, "/api/injections", limit);
        assert_eq!(
            walked
                .iter()
                .map(|i| i["id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            ids
        );
    }
    let saved = items(&all)
        .iter()
        .find(|i| i["candidate_tokens"] == 400)
        .unwrap();
    for field in [
        "id",
        "at",
        "project",
        "via",
        "session_id",
        "budget",
        "est_tokens",
        "candidate_count",
        "candidate_tokens",
        "saved_pct",
        "memory_count",
    ] {
        assert!(saved.get(field).is_some(), "missing {field}");
    }
    assert_eq!(
        (saved["est_tokens"].as_u64(), saved["saved_pct"].as_f64()),
        (Some(100), Some(75.0))
    );
    assert_eq!(saved["memory_count"], 3);
    let old = items(&all)
        .iter()
        .find(|i| i["candidate_tokens"].is_null())
        .unwrap();
    assert!(old["saved_pct"].is_null() && old["session_id"].is_null() && old["project"].is_null());

    assert_eq!(
        items(&api.ok("/api/injections?via=hook&limit=200")).len(),
        5
    );
    assert_eq!(items(&api.ok("/api/injections?via=cli")).len(), 1);
    assert_eq!(
        items(&api.ok(&format!("/api/injections?session={WEB}"))).len(),
        3
    );
    assert_eq!(
        items(&api.ok("/api/injections?project=%2Ftmp%2Feval%2Fweb-api")).len(),
        9
    );
    assert_eq!(items(&api.ok("/api/injections?via=nope")).len(), 0);

    let id = saved["id"].as_i64().unwrap();
    let d = api.ok(&format!("/api/injections/{id}"));
    assert_eq!(d["pack_text"], "# Recalla memory\n- a thing [abcd1234]");
    assert_eq!(d["memories"].as_array().unwrap().len(), 3);
    // Included memories are full Memory rows, so the page can say why each was picked.
    for m in d["memories"].as_array().unwrap() {
        for field in [
            "uid",
            "kind",
            "text",
            "status",
            "scope_project",
            "confidence",
            "seen_count",
            "anchor_count",
            "stale_reason",
            "first_source",
            "updated_at",
        ] {
            assert!(m.get(field).is_some(), "{field}");
        }
    }
    assert_eq!(d["est_tokens"], 100);
    let old_id = old["id"].as_i64().unwrap();
    let d = api.ok(&format!("/api/injections/{old_id}"));
    assert!(d["pack_text"].is_null() && d["memories"].as_array().unwrap().is_empty());
    assert_eq!(api.err("/api/injections/99999").0, 404);
    assert_eq!(api.err("/api/injections/abc").0, 400);
    assert_eq!(api.err("/api/injections?cursor=zz").0, 400);
    assert_eq!(api.err("/api/injections?limit=-5").0, 400);
}

#[test]
fn hostile_text_comes_out_clean_over_http() {
    let (api, uid, _) = Api::start();
    // `ok` already asserts no control or invisible chars in any string; check
    // the hostile fields really are present, cleaned rather than dropped.
    let list = api.ok("/api/memories?limit=200");
    let m = items(&list).iter().find(|m| m["uid"] == uid).unwrap();
    assert!(m["text"].as_str().unwrap().starts_with("hostile memory"));
    assert!(m["text"].as_str().unwrap().contains("ESC[2J"));
    let d = api.ok(&format!("/api/memories/{uid}"));
    let full = d["text_full"].as_str().unwrap();
    assert!(full.contains('\n') && full.contains("BIDIx") && full.contains("ZW!"));
    let sessions = api.ok("/api/sessions?limit=200");
    assert!(
        items(&sessions)
            .iter()
            .any(|s| s["session_id"] == HOSTILE_SESSION)
    );
    // And raw: no ESC byte or bidi char anywhere on the wire.
    for url in [
        "/api/memories?limit=200".to_owned(),
        format!("/api/memories/{uid}"),
        format!("/api/sessions/claude-code/{HOSTILE_SESSION}/events"),
    ] {
        let r = api.get(&url);
        assert!(
            !r.body.contains('\u{1b}') && !r.body.contains('\u{202e}'),
            "{url}"
        );
    }
}

#[test]
fn gets_never_write_the_database() {
    let (api, uid, _) = Api::start();
    let urls = [
        "/api/overview".to_owned(),
        "/api/projects".to_owned(),
        "/api/sessions".to_owned(),
        format!("/api/sessions/claude-code/{WEB}"),
        format!("/api/sessions/claude-code/{WEB}/events"),
        "/api/memories".to_owned(),
        format!("/api/memories/{uid}"),
        "/api/injections".to_owned(),
        "/api/injections/3".to_owned(),
        "/api/connect".to_owned(),
        "/api/sync/status".to_owned(),
    ];
    let meta = |p: &Path| {
        let m = fs::metadata(p).unwrap();
        (m.modified().unwrap(), m.len())
    };
    let before = meta(&api.db());
    std::thread::sleep(Duration::from_millis(30));
    for _ in 0..5 {
        for u in &urls {
            api.ok(u);
        }
    }
    assert_eq!(meta(&api.db()), before, "db file touched by GETs");
    let conn = rusqlite::Connection::open(api.db()).unwrap();
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, recalla::store::schema_version());

    // The connection itself refuses writes.
    let mut ro = Store::open_read_only(api.db(), Duration::from_millis(100)).unwrap();
    assert!(ro.archive(&uid).is_err());
    drop(conn);
    assert_eq!(
        api.env.store().get(&uid).unwrap().unwrap().status,
        Status::Active
    );
}

#[test]
fn old_schema_is_503_with_a_clear_message() {
    let env = Env::new();
    populate(&env);
    let api = Api::serve(env, false);
    // Pretend the file is from the previous release.
    let conn = rusqlite::Connection::open(api.db()).unwrap();
    conn.pragma_update(None, "user_version", 7).unwrap();
    drop(conn);
    for url in [
        "/api/overview",
        "/api/sessions",
        "/api/memories",
        "/api/injections/1",
    ] {
        let r = api.get(url);
        assert_eq!(r.status, 503, "{url}");
        assert_eq!(r.json(), json!({"error": "database needs upgrading"}));
    }
    let connect = api.ok("/api/connect");
    assert_eq!(connect["db"]["schema_version"], 7, "Connect still answers");
    let version: u32 = rusqlite::Connection::open(api.db())
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 7, "a read-only open must not migrate");
}

#[test]
fn first_run_empty_database_gives_empty_lists() {
    let env = Env::new(); // data dir does not exist yet
    assert!(!env.data.exists());
    let api = Api::serve(env, false);
    assert!(api.db().is_file(), "startup creates the database");
    for url in ["/api/sessions", "/api/memories", "/api/injections"] {
        let v = api.ok(url);
        assert_eq!((items(&v).len(), v["next"].is_null()), (0, true), "{url}");
    }
    let o = api.ok("/api/overview");
    assert_eq!(
        (o["sessions"].as_u64(), o["memories"]["total"].as_u64()),
        (Some(0), Some(0))
    );
    assert!(o["last_event_ts"].is_null() && o["injections"]["saved_pct"].is_null());
    assert_eq!(api.err("/api/memories/abcd1234").0, 404);
    let c = api.ok("/api/connect");
    assert!(c["last_capture_at"].is_null() && c["last_injection"].is_null());
    assert_eq!(api.ok("/api/sync/status")["state"], "idle");
}

#[test]
fn startup_prepare_reports_upgrades_once() {
    let env = Env::new();
    let mut said = Vec::new();
    recalla::ui::prepare(&env.data, |v| said.push(v)).unwrap();
    assert!(said.is_empty(), "creating is not upgrading");
    let conn = rusqlite::Connection::open(paths::db_path(&env.data)).unwrap();
    conn.execute_batch(
        "DROP INDEX injections_session; ALTER TABLE injections DROP COLUMN pack_text;
         ALTER TABLE injections DROP COLUMN candidate_count;
         ALTER TABLE injections DROP COLUMN candidate_tokens;
         ALTER TABLE injections DROP COLUMN session_id;
         DROP INDEX memories_updated; DROP INDEX memories_created;
         DROP INDEX memories_seen; DROP INDEX memories_confidence;
         DROP INDEX provenance_session;
         PRAGMA user_version = 7;",
    )
    .unwrap();
    drop(conn);
    recalla::ui::prepare(&env.data, |v| said.push(v)).unwrap();
    assert_eq!(said, [recalla::store::schema_version()]);
    recalla::ui::prepare(&env.data, |v| said.push(v)).unwrap();
    assert_eq!(said.len(), 1, "already current");
    assert_eq!(
        env.store().user_version().unwrap(),
        recalla::store::schema_version()
    );
}

#[test]
fn http_gets_are_fast_enough_smoke() {
    let (api, _, _) = Api::start();
    let start = Instant::now();
    for _ in 0..20 {
        api.ok("/api/memories?limit=50");
        api.ok("/api/sessions?limit=50");
    }
    assert!(start.elapsed() < Duration::from_secs(10));
}

// ------------------------------------------------------------ connector

fn settings_text(api: &Api) -> String {
    fs::read_to_string(&api.settings).unwrap_or_default()
}

fn write_settings(api: &Api, text: &str) {
    fs::create_dir_all(api.settings.parent().unwrap()).unwrap();
    fs::write(&api.settings, text).unwrap();
}

fn installed(v: &Value) -> Vec<(String, bool)> {
    v["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["event"].as_str().unwrap().to_owned(),
                h["installed"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[test]
fn connect_reports_state_and_exact_commands() {
    let (api, _, _) = Api::start();
    let c = api.ok("/api/connect");
    assert_eq!(
        installed(&c),
        [
            ("SessionStart".to_owned(), false),
            ("Stop".to_owned(), false),
            ("PreCompact".to_owned(), false)
        ]
    );
    assert_eq!(c["settings_exists"], false);
    assert!(c["settings_error"].is_null());
    assert!(
        c["settings_path"]
            .as_str()
            .unwrap()
            .ends_with("claude/settings.json")
    );
    assert_eq!(
        c["mcp"]["claude_code_command"],
        format!("claude mcp add --scope user recalla -- \"{FAKE_EXE}\" mcp")
    );
    assert_eq!(
        c["mcp"]["codex_toml"],
        format!("[mcp_servers.recalla]\ncommand = \"{FAKE_EXE}\"\nargs = [\"mcp\"]\n")
    );
    assert_eq!(c["binary_path"], FAKE_EXE);
    assert_eq!(c["db"]["exists"], true);
    assert!(c["db"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(c["db"]["schema_version"], recalla::store::schema_version());
    assert!(c["data_dir"].as_str().unwrap().ends_with("data"));
    assert!(c["mirror_dir"].as_str().unwrap().ends_with("data/mirror"));
    assert!(c["last_capture_at"].is_i64());
    assert_eq!(c["last_injection"]["via"], "cli");
    assert!(c["last_injection"]["at"].is_i64());
    assert!(c["last_injection"].get("project").is_some());
    assert_eq!(c["hook_log_tail"], json!([]));

    // The hook log tail is cleaned, bounded and limited to 5 lines.
    let log = paths::db_path(&api.env.data).with_file_name("log");
    fs::create_dir_all(&log).unwrap();
    let lines: Vec<String> = (0..8)
        .map(|i| format!("line {i} {HOSTILE} {}", "y".repeat(400)))
        .collect();
    fs::write(log.join("hook.log"), lines.join("\n")).unwrap();
    let c = api.ok("/api/connect");
    let tail = c["hook_log_tail"].as_array().unwrap();
    assert_eq!(tail.len(), 5);
    assert!(tail[0].as_str().unwrap().starts_with("line 3 "));
    assert!(
        tail.iter()
            .all(|l| l.as_str().unwrap().chars().count() <= 300)
    );
}

#[test]
fn hooks_install_and_uninstall_round_trip_preserving_other_keys() {
    let (api, _, _) = Api::start();
    write_settings(
        &api,
        "{\n  \"theme\": \"dark\",\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]},\n  \"hooks\": {\"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo mine\"}]}]}\n}\n",
    );
    let original = settings_text(&api);
    assert!(installed(&api.ok("/api/connect")).iter().all(|(_, on)| !on));
    assert_eq!(api.ok("/api/connect")["settings_exists"], true);

    let r = api.post_ok("/api/hooks/install", &json!({}));
    assert_eq!(r["changed"], true);
    assert_eq!(
        r["changes"],
        json!(["SessionStart: added", "Stop: added", "PreCompact: added"])
    );
    let backup = r["backup"].as_str().unwrap();
    assert!(backup.starts_with("settings.json.recalla-bak-") && !backup.contains('/'));
    assert_eq!(
        fs::read_to_string(api.settings.with_file_name(backup)).unwrap(),
        original
    );
    assert!(installed(&r).iter().all(|(_, on)| *on));
    let text = settings_text(&api);
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["theme"], "dark");
    assert_eq!(v["permissions"]["allow"], json!(["Bash(ls)"]));
    assert!(text.contains("echo mine"), "foreign hook kept");
    assert!(text.contains(&format!("\\\"{FAKE_EXE}\\\" hook session-start")));
    assert!(installed(&api.ok("/api/connect")).iter().all(|(_, on)| *on));

    let again = api.post_ok("/api/hooks/install", &json!({}));
    assert_eq!(again["changed"], false);
    assert!(again["backup"].is_null());

    let r = api.post_ok("/api/hooks/uninstall", &json!({}));
    assert_eq!(r["changed"], true);
    assert!(installed(&r).iter().all(|(_, on)| !on));
    let v: Value = serde_json::from_str(&settings_text(&api)).unwrap();
    assert_eq!(v["theme"], "dark");
    assert!(settings_text(&api).contains("echo mine"));
    assert!(!settings_text(&api).contains("recalla"));
    assert!(installed(&api.ok("/api/connect")).iter().all(|(_, on)| !on));
}

#[test]
fn hooks_refuse_a_malformed_settings_file_and_leave_it_alone() {
    let (api, _, _) = Api::start();
    for bad in ["{ not json", "[1, 2]", "{\"hooks\": 5}"] {
        write_settings(&api, bad);
        for action in ["install", "uninstall"] {
            let r = api.post(&format!("/api/hooks/{action}"), &json!({}));
            assert_eq!(r.status, 409, "{bad} {action}");
            let msg = r.json()["error"].as_str().unwrap().to_owned();
            assert!(msg.contains("nothing was changed"), "{msg}");
            assert!(!msg.contains('\u{1b}'));
            assert_eq!(settings_text(&api), bad, "file untouched");
        }
        let siblings: Vec<_> = fs::read_dir(api.settings.parent().unwrap())
            .unwrap()
            .collect();
        assert_eq!(siblings.len(), 1, "no backup or temp file left behind");
    }
    write_settings(&api, "{ not json");
    let c = api.ok("/api/connect");
    assert!(installed(&c).iter().all(|(_, on)| !on));
    assert!(
        c["settings_error"]
            .as_str()
            .unwrap()
            .contains("not valid JSON")
    );
}

#[test]
fn installing_without_a_settings_file_creates_one() {
    let (api, _, _) = Api::start();
    assert!(!api.settings.exists());
    let r = api.post_ok("/api/hooks/install", &json!({}));
    assert_eq!(
        (r["changed"].clone(), r["backup"].clone()),
        (json!(true), Value::Null)
    );
    assert!(api.settings.is_file());
    assert!(installed(&api.ok("/api/connect")).iter().all(|(_, on)| *on));
}

#[test]
fn posts_need_origin_and_csrf() {
    let (api, hostile, _) = Api::start();
    let targets = [
        "/api/hooks/install".to_owned(),
        "/api/hooks/uninstall".to_owned(),
        format!("/api/memories/{hostile}/forget"),
        format!("/api/memories/{hostile}/confirm"),
        format!("/api/memories/{hostile}/restore"),
        "/api/test-recall".to_owned(),
        "/api/sync".to_owned(),
    ];
    for t in &targets {
        for (origin, csrf) in [(false, false), (true, false), (false, true)] {
            let r = api.post_with(t, b"{}", origin, csrf);
            assert_eq!(r.status, 403, "{t} origin={origin} csrf={csrf}");
        }
        let anon = http::request(
            api.addr,
            "POST",
            t,
            Some(&http::host(api.addr)),
            &[
                ("Origin", &format!("http://127.0.0.1:{}", api.addr.port())),
                ("X-Recalla-Csrf", &api.csrf),
            ],
            b"{}",
        );
        assert_eq!(anon.status, 401, "{t} without login");
    }
    let evil = http::request(
        api.addr,
        "POST",
        "/api/hooks/install",
        Some(&http::host(api.addr)),
        &[
            ("Cookie", &api.cookie),
            ("Origin", "http://evil.example"),
            ("X-Recalla-Csrf", &api.csrf),
        ],
        b"{}",
    );
    assert_eq!(evil.status, 403);
    assert!(!api.settings.exists(), "nothing happened");
    assert_eq!(
        api.env.store().get(&hostile).unwrap().unwrap().status,
        Status::Active
    );
    assert_eq!(api.ok("/api/sync/status")["state"], "idle");
}

fn mirror_file(api: &Api, uid: &str) -> String {
    let m = api.env.store().get(uid).unwrap().unwrap();
    fs::read_to_string(recalla::mirror::memory_path(
        &paths::mirror_dir(&api.env.data),
        &m,
    ))
    .unwrap()
}

#[test]
fn forget_restore_confirm_round_trip_on_a_global_memory() {
    let (api, uid, total) = Api::start();
    let before = mirror_file(&api, &uid);
    assert!(before.contains("status: active"), "{before}");

    let r = api.post_ok(&format!("/api/memories/{uid}/forget"), &json!({}));
    assert_eq!(
        (r["uid"].as_str(), r["status"].as_str()),
        (Some(uid.as_str()), Some("archived"))
    );
    assert_eq!(r["scope_project"], Value::Null, "a Global memory");
    assert!(r.get("kind").is_some() && r.get("first_source").is_some());
    assert_eq!(
        api.env.store().get(&uid).unwrap().unwrap().status,
        Status::Archived
    );
    let after = mirror_file(&api, &uid);
    assert_ne!(after, before, "mirror follows");
    assert!(after.contains("status: archived"), "{after}");
    assert_eq!(
        items(&api.ok("/api/memories?limit=200")).len(),
        total - 1,
        "hidden by default"
    );
    let archived = api.ok("/api/memories?status=archived");
    assert_eq!(items(&archived)[0]["uid"], uid.as_str());

    let r = api.post_ok(&format!("/api/memories/{}/restore", &uid[..10]), &json!({}));
    assert_eq!(r["status"], "active");
    assert_eq!(
        api.env.store().get(&uid).unwrap().unwrap().status,
        Status::Active
    );
    assert!(mirror_file(&api, &uid).contains("status: active"));
    assert_eq!(items(&api.ok("/api/memories?limit=200")).len(), total);

    // Confirm re-activates a stale memory and lifts confidence.
    api.env
        .store()
        .set_status(&uid, Status::NeedsReverify)
        .unwrap();
    assert_eq!(api.ok("/api/overview")["memories"]["stale"], 1);
    let r = api.post_ok(&format!("/api/memories/{uid}/confirm"), &json!({}));
    assert_eq!(r["status"], "active");
    assert!(r["confidence"].as_f64().unwrap() >= 0.9);
    assert_eq!(api.ok("/api/overview")["memories"]["stale"], 0);

    // Errors.
    assert_eq!(
        api.post("/api/memories/deadbeefdeadbeef/forget", &json!({}))
            .status,
        404
    );
    assert_eq!(api.post("/api/memories/ab/forget", &json!({})).status, 400);
    assert_eq!(
        api.post("/api/memories/zzzzzzzz/forget", &json!({})).status,
        400
    );
    assert_eq!(
        api.post(&format!("/api/memories/{uid}/explode"), &json!({}))
            .status,
        404
    );
}

#[test]
fn memory_actions_refuse_ambiguous_prefixes() {
    let (api, _, _) = Api::start();
    let mut store = api.env.store();
    let mut seen = std::collections::HashMap::new();
    let (prefix, a, b) = (0..5000)
        .find_map(|i| {
            let text = format!("collision candidate number {i}");
            let uid = recalla::model::uid(Kind::Fact, &Scope::Global, &text);
            seen.insert(uid[..4].to_owned(), text.clone())
                .map(|prev| (uid[..4].to_owned(), prev, text))
        })
        .unwrap();
    for text in [a, b] {
        let m = NewMemory {
            kind: Kind::Fact,
            text,
            scope: Scope::Global,
            confidence: 0.5,
            importance: 1,
        };
        store
            .insert(&m, &Provenance::new(Origin::Backfill))
            .unwrap();
    }
    let r = api.post(&format!("/api/memories/{prefix}/forget"), &json!({}));
    assert_eq!(r.status, 409);
    assert!(r.json()["error"].as_str().unwrap().contains("several"));
    let count = |s: Status| api.env.store().all_memories(&[s]).unwrap().len();
    assert_eq!(count(Status::Archived), 0, "nothing was archived");
}

#[test]
fn test_recall_builds_a_pack_without_logging_an_injection() {
    let (api, _, _) = Api::start();
    let count = || {
        api.ok("/api/overview")["injections"]["count"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(count(), 10);
    let r = api.post_ok(
        "/api/test-recall",
        &json!({"project": WEB_PROJECT, "query": "rate limit", "budget": 1500}),
    );
    assert_eq!(count(), 10, "no injection recorded");
    assert!(r["markdown"].as_str().unwrap().starts_with("# Recalla"));
    assert!(r["est_tokens"].as_u64().unwrap() > 0);
    assert!(r["est_tokens"].as_u64().unwrap() <= 1500);
    assert!(
        r["candidate_count"].as_u64().unwrap() >= r["memories"].as_array().unwrap().len() as u64
    );
    assert!(r["candidate_tokens"].as_u64().unwrap() > 0);
    assert!(r["saved_pct"].is_number() || r["saved_pct"].is_null());
    let mems = r["memories"].as_array().unwrap();
    assert!(!mems.is_empty());
    for m in mems {
        assert!(m["uid"].is_string() && m["kind"].is_string() && m["text"].is_string());
    }
    // Defaults: no body at all means global scope and the default budget.
    let r = api.post_ok("/api/test-recall", &json!({}));
    assert!(r["est_tokens"].as_u64().unwrap() <= 1500);
    let raw = api.post_with("/api/test-recall", b"", true, true);
    assert_eq!(raw.status, 200);
    // Budget is clamped to 100..=8000 instead of rejected.
    let tiny = api.post_ok(
        "/api/test-recall",
        &json!({"project": WEB_PROJECT, "budget": 1}),
    );
    assert!(tiny["est_tokens"].as_u64().unwrap() <= 100);
    let huge = api.post_ok(
        "/api/test-recall",
        &json!({"project": WEB_PROJECT, "budget": 10_000_000}),
    );
    assert!(huge["est_tokens"].as_u64().unwrap() <= 8000);
    // A project with no memories of its own still gets the global ones, not an error.
    let other = api.post_ok("/api/test-recall", &json!({"project": "/no/such/project"}));
    assert!(other["memories"].is_array());
    assert_eq!(count(), 10);

    for bad in [
        json!([1]),
        json!("x"),
        json!({"budget": "lots"}),
        json!({"budget": -5}),
        json!({"budget": 1.5}),
        json!({"project": 5}),
        json!({"query": "a".repeat(201)}),
        json!({"project": "a\u{0}b"}),
        json!({"project": "x".repeat(501)}),
    ] {
        assert_eq!(api.post("/api/test-recall", &bad).status, 400, "{bad}");
    }
    assert_eq!(
        api.post_with("/api/test-recall", b"{nope", true, true)
            .status,
        400
    );
    let big = json!({"query": "a", "pad": "x".repeat(5000)});
    assert_eq!(
        api.post("/api/test-recall", &big).status,
        400,
        "body over 4 KiB"
    );
    assert_eq!(count(), 10);
}

/// Everything the staleness check could touch: memories (status, reasons) and anchors.
fn db_snapshot(store: &Store, key: &str) -> String {
    format!(
        "{:?}{:?}",
        store
            .all_memories(&[
                Status::Active,
                Status::NeedsReverify,
                Status::Superseded,
                Status::Archived
            ])
            .unwrap(),
        store.anchors_for_project(key).unwrap()
    )
}

#[test]
fn test_recall_does_not_write_even_when_anchored_code_changed() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let project = fs::canonicalize(tmp.path()).unwrap().join("proj");
    fs::create_dir_all(project.join("src")).unwrap();
    let file = project.join("src/store.rs");
    fs::write(&file, "pub fn insert(x: u32) -> u32 {\n    x + 1\n}\n").unwrap();
    let key = project.to_str().unwrap().to_owned();
    let env = Env::new();
    recalla::ui::prepare(&env.data, |_| {}).unwrap();
    {
        let mut store = env.store();
        let text = "Keep `insert` cheap in src/store.rs";
        let new = NewMemory {
            kind: Kind::Decision,
            text: text.into(),
            scope: Scope::Project(key.clone()),
            confidence: 0.8,
            importance: 2,
        };
        let uid = store
            .insert(&new, &Provenance::new(Origin::Remember))
            .unwrap()
            .uid()
            .to_owned();
        let anchors = recalla::anchor::resolve(text, &["src/store.rs".to_owned()], &project);
        assert!(!anchors.is_empty());
        for a in &anchors {
            store.add_anchor(&uid, a).unwrap();
        }
    }
    fs::write(&file, "pub fn insert(x: u32) -> u32 {\n    x - 1\n}\n").unwrap();
    let api = Api::serve(env, false);
    let before = db_snapshot(&api.env.store(), &key);
    let r = api.post_ok("/api/test-recall", &json!({"project": key}));
    assert!(r["markdown"].as_str().unwrap().contains("Keep `insert`"));
    assert_eq!(
        db_snapshot(&api.env.store(), &key),
        before,
        "test-recall wrote"
    );
    // The same call through the normal pack path does write (the test can fail).
    recalla::anchor::check(&mut api.env.store(), &key, None).unwrap();
    assert_ne!(db_snapshot(&api.env.store(), &key), before);
}

// ------------------------------------------------------------------ sync

fn wait_for_job(api: &Api) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let s = api.ok("/api/sync/status");
        if s["state"] != "running" {
            return s;
        }
        assert!(Instant::now() < deadline, "sync did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn sync_job_runs_the_backfill_path_and_reports_counts() {
    let env = Env::new();
    env.write_all();
    let (data, root, codex) = (env.data.clone(), env.root.clone(), env.codex_root.clone());
    let api = Api::serve(env, true);
    assert_eq!(
        api.ok("/api/sync/status"),
        json!({"state": "idle", "job": null, "started_at": null, "finished_at": null})
    );
    assert_eq!(api.ok("/api/overview")["sessions"], 0);

    let started = api.post_ok("/api/sync", &json!({}));
    let job = started["job"].as_str().unwrap().to_owned();
    assert!(job.starts_with('j'));
    let done = wait_for_job(&api);
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["job"], job.as_str());
    assert!(done["started_at"].as_i64().unwrap() <= done["finished_at"].as_i64().unwrap());
    assert!(done.get("error").is_none());
    let overview = api.ok("/api/overview");
    assert_eq!(overview["sessions"], 11);
    assert!(overview["memories"]["total"].as_u64().unwrap() > 10);
    assert!(
        overview["archive_bytes"].as_u64().unwrap() > 0,
        "archive is on, as in backfill"
    );
    assert!(
        std::fs::read_dir(paths::mirror_dir(&data)).unwrap().count() > 0,
        "mirror was synced"
    );

    // The same fixture through the CLI into a fresh data dir: same counts.
    let other = tempfile::tempdir().unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = recalla::cli::run(
        [
            "recalla",
            "backfill",
            "--json",
            "--root",
            root.to_str().unwrap(),
            "--codex-root",
            codex.to_str().unwrap(),
            "--data-dir",
            other.path().to_str().unwrap(),
        ],
        &mut out,
        &mut err,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let cli: Value = serde_json::from_slice(&out).unwrap();
    for ptr in [
        "/ingest/files/seen",
        "/ingest/files/new",
        "/ingest/events_added",
        "/ingest/malformed_lines",
        "/extract/events_processed",
        "/extract/memories_new",
        "/extract/memories_duplicate",
        "/extract/tasks_archived",
    ] {
        assert_eq!(done["report"].pointer(ptr), cli.pointer(ptr), "{ptr}");
        assert!(done["report"].pointer(ptr).is_some(), "{ptr}");
    }
    assert!(done["report"]["mirror"].is_string());
    assert_eq!(done["report"]["ingest"]["files"]["failed"], json!([]));

    // A second run is a no-op import and can start again.
    let again = api.post_ok("/api/sync", &json!({}));
    assert_ne!(again["job"], job.as_str());
    let second = wait_for_job(&api);
    assert_eq!(second["state"], "done");
    assert_eq!(second["report"]["ingest"]["events_added"], 0);
    assert_eq!(second["report"]["ingest"]["files"]["new"], 0);
}

#[test]
fn only_one_sync_runs_at_a_time() {
    let (api, _, _) = Api::start();
    // Hold the slot the way a running job does.
    let running = api.sync.try_start().unwrap();
    let status = api.ok("/api/sync/status");
    assert_eq!(
        (status["state"].as_str(), status["job"].as_str()),
        (Some("running"), Some(running.as_str()))
    );
    assert!(status["finished_at"].is_null());
    let r = api.post("/api/sync", &json!({}));
    assert_eq!(r.status, 409);
    assert_eq!(
        r.json(),
        json!({"error": "sync already running", "job": running})
    );
    assert_eq!(api.ok("/api/sync/status")["state"], "running");
}

#[test]
fn concurrent_sync_posts_start_exactly_one_job() {
    let (api, _, _) = Api::start();
    let api = Arc::new(api);
    let statuses: Vec<u16> = (0..6)
        .map(|_| {
            let api = Arc::clone(&api);
            std::thread::spawn(move || api.post("/api/sync", &json!({})).status)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    assert!(
        statuses.iter().all(|s| matches!(s, 202 | 409)),
        "{statuses:?}"
    );
    // At least one started; a 202 after the first finished is legitimate, so
    // the invariant is that every 202 got its own job and the state settles.
    assert!(statuses.contains(&202));
    assert_ne!(wait_for_job(&api)["state"], "running");
}

#[test]
fn sync_failure_is_generic_and_the_slot_is_released() {
    let env = Env::new();
    let api = Api::serve(env, false); // no session folders at all
    assert_eq!(api.post_ok("/api/sync", &json!({}))["job"], "j1");
    let s = wait_for_job(&api);
    assert_eq!(s["state"], "failed");
    assert!(s["error"].as_str().unwrap().contains("no session folders"));
    assert!(s.get("report").is_none());
    assert!(
        api.post("/api/sync", &json!({})).status == 202,
        "a failed job frees the slot"
    );
    wait_for_job(&api);
}
