//! Code anchors end to end: extraction anchors a memory to a symbol, edits
//! flag it stale, `confirm` re-anchors, packs show why.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::Env;
use recalla::anchor::{self, CheckReport};
use recalla::mcp::{McpCtx, serve};
use recalla::model::{Anchor, Kind, Memory, NewMemory, Origin, Provenance, Scope, Status};
use recalla::ops::{self, PackArgs};
use recalla::store::{Store, Via};
use serde_json::{Value, json};

const STORE_RS: &str = "pub struct Store { n: u32 }

impl Store {
    pub fn insert(&mut self, x: u32) -> u32 {
        self.n += x;
        self.n
    }

    pub fn other(&self) -> u32 {
        self.n
    }
}
";
const CART_TS: &str = "export class Cart {\n  total(): number {\n    return 1;\n  }\n}\n";
const DECISION: &str = "We decided to keep `Store::insert` cheap in src/store.rs for now.";

/// `/tmp` on unix (a short, unsymlinked path; macOS `$TMPDIR` is not); the
/// system temp dir elsewhere.
fn tmp_root() -> PathBuf {
    if cfg!(unix) {
        "/tmp".into()
    } else {
        std::env::temp_dir()
    }
}

/// A temp dir outside any git repo, with the usual random name (`.tmpXXXX`):
/// paths are redacted without the entropy layer, so it survives into the
/// session's cwd and project key. Unix only, like CI.
fn tmp() -> tempfile::TempDir {
    tempfile::tempdir_in(tmp_root()).unwrap()
}

/// A temp dir with a fixed, readable name. Needed where an absolute path
/// travels inside a tool call's input: that text still gets full redaction,
/// which would blank a random directory name.
fn named_tmp(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("recalla-{name}"))
        .rand_bytes(0)
        .tempdir_in(tmp_root())
        .unwrap()
}

struct Fx {
    env: Env,
    /// Canonical project dir; also the project key (it has no `.git`).
    project: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Fx {
    /// A git-less project with Rust and TS files and one session whose
    /// decision mentions `Store::insert`, already ingested and extracted.
    fn new() -> Self {
        Self::with_turns(&[
            DECISION,
            "We decided to keep `Cart.total` simple in web/cart.ts.",
        ])
    }

    fn with_turns(sentences: &[&str]) -> Self {
        let turns: Vec<Value> = sentences.iter().map(|s| json!({"assistant": s})).collect();
        Self::with_turn_values(tmp(), |_| turns)
    }

    /// As `with_turns`, with raw session turns built from the project dir
    /// (after the standard files exist, before ingest).
    fn with_turn_values(tmp: tempfile::TempDir, turns: impl FnOnce(&Path) -> Vec<Value>) -> Self {
        let project = fs::canonicalize(tmp.path()).unwrap().join("proj");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(project.join("web")).unwrap();
        fs::write(project.join("src/store.rs"), STORE_RS).unwrap();
        fs::write(project.join("web/cart.ts"), CART_TS).unwrap();
        let turns = turns(&project);
        let env = Env::new();
        env.write_session(&json!({
            "id": "anch", "cwd": project.to_str().unwrap(), "turns": turns,
        }));
        env.backfill();
        Self {
            env,
            project,
            _tmp: tmp,
        }
    }

    fn key(&self) -> &str {
        self.project.to_str().unwrap()
    }

    fn write(&self, rel: &str, body: &str) {
        fs::write(self.project.join(rel), body).unwrap();
    }

    fn memory(&self, needle: &str) -> Memory {
        let store = self.env.store();
        let found = store
            .all_memories(&[Status::Active, Status::NeedsReverify])
            .unwrap()
            .into_iter()
            .find(|m| m.text.contains(needle))
            .unwrap_or_else(|| panic!("no memory with {needle:?}"));
        store.get(&found.uid).unwrap().unwrap()
    }

    fn check(&self) -> CheckReport {
        anchor::check(&mut self.env.store(), self.key(), None).unwrap()
    }

    fn status_and_reason(&self, needle: &str) -> (Status, Option<String>) {
        let m = self.memory(needle);
        let reason = self
            .env
            .store()
            .anchors_for_project(self.key())
            .unwrap()
            .into_iter()
            .find(|r| r.0 == m.uid)
            .and_then(|r| r.3);
        (m.status, reason)
    }
}

#[test]
fn memory_is_anchored_to_its_symbol_after_extraction() {
    let fx = Fx::new();
    let m = fx.memory("Store::insert");
    assert_eq!(m.anchors.len(), 1, "{:?}", m.anchors);
    let a = &m.anchors[0];
    assert_eq!(a.path, "src/store.rs");
    assert_eq!(a.symbol.as_deref(), Some("Store::insert"));
    assert_eq!(a.lang.as_deref(), Some("rust"));
    assert!(a.content_hash.is_some());

    let ts = fx.memory("Cart.total");
    assert_eq!(ts.anchors[0].symbol.as_deref(), Some("Cart.total"));
    assert_eq!(
        fx.check(),
        CheckReport {
            checked: 2,
            ..CheckReport::default()
        }
    );
}

#[test]
fn editing_the_body_marks_needs_reverify_with_the_reason() {
    let fx = Fx::new();
    fx.write(
        "src/store.rs",
        &STORE_RS.replace("self.n += x;", "self.n += x * 2;"),
    );
    let r = fx.check();
    assert_eq!((r.checked, r.stale_new), (2, 1));
    assert_eq!(
        fx.status_and_reason("Store::insert"),
        (
            Status::NeedsReverify,
            Some("symbol changed: Store::insert".to_owned())
        )
    );
    assert_eq!(fx.memory("Cart.total").status, Status::Active);
    // Idempotent, and changing the code back does not clear it.
    assert_eq!(fx.check().stale_new, 0);
    fx.write("src/store.rs", STORE_RS);
    let back = fx.check();
    assert_eq!((back.stale_new, back.recovered), (0, 1));
    assert_eq!(fx.memory("Store::insert").status, Status::NeedsReverify);
}

#[test]
fn editing_another_fn_in_the_file_stays_active() {
    let fx = Fx::new();
    fx.write(
        "src/store.rs",
        &STORE_RS.replace(
            "pub fn other(&self) -> u32 {\n        self.n",
            "pub fn other(&self) -> u32 {\n        self.n + 100",
        ),
    );
    assert_eq!(fx.check().stale_new, 0);
    assert_eq!(
        fx.status_and_reason("Store::insert"),
        (Status::Active, None)
    );
}

#[test]
fn reformatting_the_fn_stays_active() {
    let fx = Fx::new();
    let squashed = STORE_RS.replace(
        "pub fn insert(&mut self, x: u32) -> u32 {\n        self.n += x;\n        self.n\n    }",
        "pub fn insert(&mut self,\n\n    x: u32)   ->   u32 { self.n += x;   self.n }",
    );
    assert_ne!(squashed, STORE_RS);
    fx.write("src/store.rs", &squashed);
    assert_eq!(fx.check().stale_new, 0);
    assert_eq!(
        fx.status_and_reason("Store::insert"),
        (Status::Active, None)
    );
}

#[test]
fn deleting_the_file_marks_file_removed() {
    let fx = Fx::new();
    fs::remove_file(fx.project.join("src/store.rs")).unwrap();
    assert_eq!(fx.check().stale_new, 1);
    assert_eq!(
        fx.status_and_reason("Store::insert"),
        (Status::NeedsReverify, Some("file removed".to_owned()))
    );
}

#[test]
fn renaming_the_fn_marks_symbol_removed() {
    let fx = Fx::new();
    fx.write("src/store.rs", &STORE_RS.replace("fn insert", "fn put"));
    assert_eq!(fx.check().stale_new, 1);
    assert_eq!(
        fx.status_and_reason("Store::insert"),
        (
            Status::NeedsReverify,
            Some("symbol removed: Store::insert".to_owned())
        )
    );
}

#[test]
fn confirm_reanchors_and_a_later_check_is_clean() {
    let fx = Fx::new();
    let before = fx.memory("Store::insert").anchors[0].content_hash.clone();
    fx.write(
        "src/store.rs",
        &STORE_RS.replace("self.n += x;", "self.n += x + 1;"),
    );
    fx.check();
    let uid = fx.memory("Store::insert").uid;
    assert_eq!(fx.memory("Store::insert").status, Status::NeedsReverify);

    let (code, out, err) = fx.env.cli(&["confirm", &uid[..8]]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Confirmed"), "{out}");

    let m = fx.memory("Store::insert");
    assert_eq!(m.status, Status::Active);
    assert_ne!(m.anchors[0].content_hash, before, "new hash expected");
    assert_eq!(fx.status_and_reason("Store::insert").1, None);
    let r = fx.check();
    assert_eq!((r.stale_new, r.recovered), (0, 0));

    let (code, _, err) = fx.env.cli(&["confirm", "0000"]);
    assert_ne!(code, 0);
    assert!(err.contains("no memory matches"), "{err}");
}

#[test]
fn cli_check_reports_and_flags() {
    let fx = Fx::new();
    fx.write("src/store.rs", &STORE_RS.replace("fn insert", "fn put"));
    let (code, out, err) = fx.env.cli(&["check", "--project", fx.key(), "--json"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (v["checked"].as_u64(), v["stale_new"].as_u64()),
        (Some(2), Some(1))
    );
    let (code, _, err) = fx.env.cli(&["check", "--project", "relative/dir"]);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn a_path_outside_the_root_is_never_anchored() {
    let tmp = tmp();
    let base = fs::canonicalize(tmp.path()).unwrap();
    fs::create_dir_all(base.join("proj")).unwrap();
    fs::create_dir_all(base.join("outside")).unwrap();
    fs::write(base.join("outside/x.rs"), "pub fn leak() {}\n").unwrap();
    let env = Env::new();
    let abs = format!(
        "We decided to move `leak` into {}/outside/x.rs now.",
        base.display()
    );
    env.write_session(&json!({
        "id": "esc", "cwd": base.join("proj").to_str().unwrap(),
        "turns": [
            {"assistant": "We decided to move `leak` into ../outside/x.rs now."},
            {"assistant": abs},
        ],
    }));
    env.backfill();
    let store = env.store();
    let all = store.all_memories(&[Status::Active]).unwrap();
    let hits: Vec<_> = all
        .iter()
        .filter(|m| m.text.contains("outside/x.rs"))
        .collect();
    assert_eq!(hits.len(), 2, "{all:?}");
    for m in hits {
        let m = store.get(&m.uid).unwrap().unwrap();
        assert!(
            m.anchors.is_empty(),
            "anchored outside root: {:?}",
            m.anchors
        );
    }
}

#[test]
fn pack_shows_the_reason_within_budget() {
    let fx = Fx::new();
    fx.write(
        "src/store.rs",
        &STORE_RS.replace("self.n += x;", "self.n -= x;"),
    );
    for budget in [1500, 120] {
        let args = PackArgs {
            budget,
            project: Some(fx.key().to_owned()),
            query: None,
            no_log: true,
            via: Via::Cli,
            session_id: None,
        };
        // The pack itself runs the (budgeted) auto-check.
        let p = ops::pack_for(&fx.env.data, &args).unwrap();
        assert!(p.est_tokens <= budget, "{} > {budget}", p.est_tokens);
        if budget == 1500 {
            assert!(p.markdown.contains("## Needs re-verify"), "{}", p.markdown);
            assert!(
                p.markdown.contains("(symbol changed: Store::insert)"),
                "{}",
                p.markdown
            );
        }
        assert_eq!(p.est_tokens, recalla::pack::est_tokens(&p.markdown));
    }
}

#[test]
fn the_time_budget_is_respected() {
    let tmp = tmp();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let key = root.to_str().unwrap().to_owned();
    let mut store = Store::open_in_memory().unwrap();
    for i in 0..200 {
        let rel = format!("f{i}.rs");
        fs::write(
            root.join(&rel),
            format!("pub fn func_{i}() -> u32 {{ {i} }}\n"),
        )
        .unwrap();
        let new = NewMemory {
            kind: Kind::Fact,
            text: format!("`func_{i}` returns {i}"),
            scope: Scope::Project(key.clone()),
            confidence: 0.5,
            importance: 1,
        };
        let uid = store
            .insert(&new, &Provenance::new(Origin::Remember))
            .unwrap()
            .uid()
            .to_owned();
        let anchors = anchor::resolve(&new.text, &[rel], &root);
        assert_eq!(anchors.len(), 1);
        store.replace_anchors(&uid, &anchors).unwrap();
        fs::write(
            root.join(format!("f{i}.rs")),
            format!("pub fn func_{i}() -> u32 {{ {i} + 1 }}\n"),
        )
        .unwrap();
    }
    let t = Instant::now();
    let r = anchor::check(&mut store, &key, Some(Duration::from_millis(1))).unwrap();
    assert!(r.skipped_budget > 0, "{r:?}");
    assert_eq!(r.checked + r.skipped_budget, 200);
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    // Unbudgeted, the rest gets examined.
    let full = anchor::check(&mut store, &key, None).unwrap();
    assert_eq!(full.checked, 200);
    assert_eq!(full.stale_new + r.stale_new, 200);
}

#[test]
fn mcp_review_confirm_reanchors() {
    let fx = Fx::new();
    fx.write(
        "src/store.rs",
        &STORE_RS.replace("self.n += x;", "self.n += x + 7;"),
    );
    fx.check();
    let m = fx.memory("Store::insert");
    assert_eq!(m.status, Status::NeedsReverify);
    let old_hash = m.anchors[0].content_hash.clone();

    let ctx = McpCtx {
        data_dir: fx.env.data.clone(),
        project: Some(fx.key().to_owned()),
    };
    let msgs = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-11-25", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "review", "arguments": {"id": &m.uid[..10], "action": "confirm"}}}),
    ];
    let input = msgs
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    serve(Cursor::new(input), &mut out, &ctx).unwrap();
    let last: Value =
        serde_json::from_str(String::from_utf8(out).unwrap().lines().last().unwrap()).unwrap();
    assert_eq!(last["result"]["isError"], false, "{last}");

    let after = fx.memory("Store::insert");
    assert_eq!(after.status, Status::Active);
    assert_ne!(after.anchors[0].content_hash, old_hash);
    assert_eq!(fx.status_and_reason("Store::insert").1, None);
    assert_eq!(fx.check().stale_new, 0);
}

#[test]
fn session_start_stays_fast_and_shows_stale_memories() {
    let fx = Fx::new();
    fx.write(
        "src/store.rs",
        &STORE_RS.replace("self.n += x;", "self.n = x;"),
    );
    let payload = json!({"session_id": "s", "cwd": fx.key(), "source": "startup"}).to_string();
    let t = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_recalla"))
        .args(["hook", "session-start", "--data-dir"])
        .arg(&fx.env.data)
        .env("CLAUDE_CONFIG_DIR", &fx.env.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let took = t.elapsed();
    assert!(out.status.success() && out.stderr.is_empty());
    // Generous bound: debug build under a parallel test run. A regression that
    // matters (e.g. a full re-parse per hook) costs seconds, not milliseconds.
    assert!(took < Duration::from_secs(3), "{took:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let ctxt = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctxt.contains("(symbol changed: Store::insert)"), "{ctxt}");
}

#[allow(dead_code)]
fn _unused(_: &Path, _: Anchor) {}

const LIMITS_RS: &str =
    "pub struct SlidingWindow { n: u32 }\n\npub struct Window { n: u32 }\n\nfn unrelated() {}\n";
const ROOT_CAUSE: &str = "The root cause is that `Window` was renamed in the limits module last week, so the import is stale.";

/// A session that edits `file`, hits a failing command, then states the gotcha.
fn window_turns(file: &Path, gotcha: &str) -> Vec<Value> {
    vec![
        json!({"user": "Why does the build fail?"}),
        json!({"tool": "Edit", "input": {"file_path": file.to_str().unwrap(),
            "old_string": "a", "new_string": "b"}, "result": "ok", "error": false}),
        json!({"tool": "Bash", "input": {"command": "cargo build"},
            "result": "error[E0432]: unresolved import in the limits module", "error": true}),
        json!({"assistant": gotcha}),
    ]
}

fn gotcha_anchors(fx: &Fx) -> Vec<(String, Option<String>)> {
    fx.memory("root cause")
        .anchors
        .into_iter()
        .map(|a| (a.path, a.symbol))
        .collect()
}

#[test]
fn random_tempdir_project_key_survives_redaction() {
    let fx = Fx::new();
    let name = fx.project.parent().unwrap().file_name().unwrap();
    assert!(name.to_str().unwrap().starts_with(".tmp"));
    assert_eq!(
        fx.memory("Store::insert").scope,
        Scope::Project(fx.key().to_owned())
    );
}

#[test]
fn window_file_gives_a_symbol_anchor_when_the_text_has_no_path() {
    let fx = Fx::with_turn_values(named_tmp("win-symbol"), |p| {
        fs::write(p.join("src/limits.rs"), LIMITS_RS).unwrap();
        window_turns(&p.join("src/limits.rs"), ROOT_CAUSE)
    });
    assert_eq!(
        gotcha_anchors(&fx),
        [("src/limits.rs".to_owned(), Some("Window".to_owned()))]
    );
}

#[test]
fn window_files_without_a_matching_identifier_give_no_anchors() {
    let fx = Fx::with_turn_values(named_tmp("win-nomatch"), |p| {
        fs::write(p.join("src/limits.rs"), LIMITS_RS).unwrap();
        window_turns(
            &p.join("src/limits.rs"),
            "The root cause is that `Gadget` was renamed in the limits module last week.",
        )
    });
    assert_eq!(gotcha_anchors(&fx), []);
}

#[test]
fn window_file_outside_the_root_is_ignored() {
    let outside = named_tmp("win-outside");
    let file = fs::canonicalize(outside.path()).unwrap().join("limits.rs");
    fs::write(&file, LIMITS_RS).unwrap();
    // Control: the same session with the file inside the root does anchor.
    let fx = Fx::with_turn_values(named_tmp("win-inside"), |p| {
        fs::write(p.join("src/limits.rs"), LIMITS_RS).unwrap();
        window_turns(&p.join("src/limits.rs"), ROOT_CAUSE)
    });
    assert_eq!(gotcha_anchors(&fx).len(), 1);
    let fx = Fx::with_turn_values(named_tmp("win-root"), |_| window_turns(&file, ROOT_CAUSE));
    assert_eq!(gotcha_anchors(&fx), []);
}

#[test]
fn root_and_home_are_never_anchor_roots() {
    assert!(recalla::anchor::project_dir("/").is_none());
    if let Some(home) = std::env::var_os("HOME") {
        assert!(recalla::anchor::project_dir(&home.to_string_lossy()).is_none());
    }
}
