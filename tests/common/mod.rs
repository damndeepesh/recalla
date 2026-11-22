//! Shared test helpers: synthetic Claude Code sessions in a temp dir, and
//! ingest / extract / CLI drivers over them.

#![allow(dead_code, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use recalla::cli;
use recalla::extract::{self, Options as ExtractOptions};
use recalla::ingest::{self, Options};
use recalla::model::{Memory, Status};
use recalla::paths;
use recalla::store::Store;
use serde_json::{Value, json};

pub const SESSIONS: &str = "tests/fixtures/extract/sessions.json";

/// Emits Claude Code records for one session, keeping the uuid chain and clock.
pub struct Writer {
    pub session: String,
    pub cwd: String,
    pub n: u32,
    pub tools: u32,
    pub parent: Option<String>,
    pub sidechain: bool,
}

impl Writer {
    pub fn new(session: &str, cwd: &str) -> Self {
        Self {
            session: session.into(),
            cwd: cwd.into(),
            n: 0,
            tools: 0,
            parent: None,
            sidechain: false,
        }
    }

    fn record(&mut self, kind: &str, content: &Value) -> String {
        self.n += 1;
        let uuid = format!("{}-{:04}", self.session, self.n);
        let rec = json!({
            "type": kind,
            "uuid": uuid,
            "parentUuid": self.parent,
            "sessionId": self.session,
            "timestamp": format!("2026-11-01T10:{:02}:{:02}.000Z", self.n / 60, self.n % 60),
            "cwd": self.cwd,
            "isSidechain": self.sidechain,
            "message": {"role": kind, "content": content},
        });
        self.parent = Some(uuid);
        format!("{rec}\n")
    }

    pub fn turns(&mut self, turns: &[Value]) -> String {
        let mut out = String::new();
        for t in turns {
            if let Some(u) = t.get("user") {
                out += &self.record("user", u);
            } else if let Some(a) = t.get("assistant") {
                out += &self.record("assistant", &json!([{"type": "text", "text": a}]));
            } else {
                self.tools += 1;
                let id = format!("toolu_eval_{}", self.tools);
                let call =
                    json!([{"type": "tool_use", "id": id, "name": t["tool"], "input": t["input"]}]);
                out += &self.record("assistant", &call);
                let result = json!([{
                    "type": "tool_result", "tool_use_id": id,
                    "content": t["result"], "is_error": t["error"],
                }]);
                out += &self.record("user", &result);
            }
        }
        out
    }
}

pub struct Env {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub data: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap().join("claude");
        fs::create_dir_all(root.join("projects")).unwrap();
        let data = tmp.path().join("data");
        Self {
            _tmp: tmp,
            root,
            data,
        }
    }

    pub fn session_path(&self, cwd: &str, session: &str) -> PathBuf {
        let dir = self.root.join("projects").join(cwd.replace('/', "-"));
        fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{session}.jsonl"))
    }

    pub fn write_session(&self, s: &Value) -> PathBuf {
        let (id, cwd) = (s["id"].as_str().unwrap(), s["cwd"].as_str().unwrap());
        let session = format!("eval-{id}");
        let sidechain = s["sidechain"] == true;
        let path = if sidechain {
            let dir = self
                .session_path(cwd, &session)
                .with_extension("")
                .join("subagents");
            fs::create_dir_all(&dir).unwrap();
            dir.join("agent-eval.jsonl")
        } else {
            self.session_path(cwd, &session)
        };
        let mut writer = Writer::new(&session, cwd);
        writer.sidechain = sidechain;
        fs::write(&path, writer.turns(s["turns"].as_array().unwrap())).unwrap();
        path
    }

    pub fn write_all(&self) -> Vec<Value> {
        let doc: Value = serde_json::from_str(&fs::read_to_string(SESSIONS).unwrap()).unwrap();
        let sessions = doc["sessions"].as_array().unwrap().clone();
        for s in &sessions {
            self.write_session(s);
        }
        sessions
    }

    pub fn ingest(&self) -> ingest::Report {
        let opts = Options {
            root: self.root.clone(),
            data_dir: self.data.clone(),
            archive: false,
            dry_run: false,
        };
        ingest::run(&opts).unwrap()
    }

    pub fn store(&self) -> Store {
        Store::open(paths::db_path(&self.data)).unwrap()
    }

    pub fn extract(&self, rebuild: bool) -> extract::Report {
        extract::run(&mut self.store(), &ExtractOptions { rebuild }).unwrap()
    }

    /// Ingest then extract, as `recalla backfill` does.
    pub fn backfill(&self) -> (ingest::Report, extract::Report) {
        (self.ingest(), self.extract(false))
    }

    pub fn active(&self) -> Vec<Memory> {
        self.store().all_memories(&[Status::Active]).unwrap()
    }

    pub fn cli(&self, args: &[&str]) -> (u8, String, String) {
        let data = self.data.to_str().unwrap();
        let root = self.root.to_str().unwrap();
        let mut full = vec!["recalla"];
        full.extend_from_slice(args);
        full.extend(["--data-dir", data]);
        if args[0] == "backfill" {
            full.extend(["--root", root, "--no-archive"]);
        }
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = cli::run(full, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
}
