//! `recalla mcp` as a child process: legacy and 2026 sessions over real pipes.

#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use recalla::paths;
use recalla::store::Store;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(20);
const PROJECT: &str = "/work/mcp-it";

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: i64,
}

impl Server {
    fn start(data_dir: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_recalla"))
            .args(["mcp", "--project", PROJECT, "--data-dir"])
            .arg(data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            lines,
            next_id: 0,
        }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(TIMEOUT)
            .expect("no response in time");
        serde_json::from_str(&line).expect("stdout must carry only JSON")
    }

    fn request(&mut self, method: &str, params: &Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let resp = self.recv();
        assert_eq!(resp["id"], id);
        resp
    }

    fn tool(&mut self, name: &str, args: &Value) -> String {
        let resp = self.request("tools/call", &json!({"name": name, "arguments": args}));
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
        assert!(self.lines.try_recv().is_err(), "unexpected extra output");
    }
}

#[test]
fn legacy_session_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mut s = Server::start(&data);

    let init = s.request(
        "initialize",
        &json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "recalla");
    s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let list = s.request("tools/list", &json!({}));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 4);

    // Planted secret, built at runtime so no literal is committed.
    let token = format!(
        "{}{}",
        ["gh", "p_"].concat(),
        "aB3dE6gH9jK2mN5pQ8sT1vW4yZ7cF0hJ3kLm"
    );
    let saved = s.tool(
        "remember",
        &json!({"text": format!("Deploys use token {token} and the blue lane"), "kind": "gotcha"}),
    );
    let uid8 = saved.strip_prefix("Saved ").unwrap().to_owned();
    let marker = s.tool(
        "remember",
        &json!({"text": "Always run make check before pushing", "kind": "preference"}),
    );
    assert!(marker.starts_with("Saved "));

    let recall = s.tool("recall", &json!({"query": "deploys"}));
    assert!(recall.contains("blue lane"), "{recall}");
    assert!(recall.contains("make check"), "{recall}");
    assert!(!recall.contains(&token));

    let why = s.tool("why", &json!({"id": uid8}));
    assert!(why.contains("(remember)"), "{why}");
    assert!(!why.contains(&token));

    let forgot = s.tool("review", &json!({"id": uid8, "action": "forget"}));
    assert!(forgot.contains("reversible"), "{forgot}");
    let after = s.tool("recall", &json!({}));
    assert!(!after.contains("blue lane"), "{after}");
    assert!(after.contains("make check"), "{after}");
    s.finish();

    let store = Store::open(paths::db_path(&data)).unwrap();
    let values = store.all_text_values().unwrap();
    assert!(!values.is_empty());
    assert!(
        values.iter().all(|v| !v.contains(&token)),
        "secret reached the database"
    );
    assert!(values.iter().any(|v| v.contains("[REDACTED")));
    let stats = store.injection_stats().unwrap();
    assert_eq!(stats.injections, 2, "one ledger row per non-empty recall");
}

#[test]
fn modern_session_is_stateless() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Server::start(&dir.path().join("data"));
    let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28"});

    let d = s.request("server/discover", &json!({}));
    assert_eq!(d["result"]["resultType"], "complete");
    assert_eq!(
        d["result"]["supportedVersions"],
        json!(["2026-07-28", "2025-11-25"])
    );

    let r = s.request(
        "tools/call",
        &json!({"name": "remember", "arguments": {"text": "Use cargo nextest"}, "_meta": meta}),
    );
    assert_eq!(r["result"]["resultType"], "complete");
    assert_eq!(r["result"]["isError"], false);

    let r = s.request(
        "tools/call",
        &json!({"name": "recall", "arguments": {}, "_meta": meta}),
    );
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cargo nextest")
    );

    let bad = s.request(
        "tools/list",
        &json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "1999-01-01"}}),
    );
    assert_eq!(bad["error"]["code"], -32022);
    s.finish();
}
