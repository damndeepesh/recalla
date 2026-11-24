//! Extractor eval over the labeled synthetic sessions, plus pipeline and CLI
//! integration over the same data.
//!
//! Sessions in `tests/fixtures/extract/sessions.json` are turned into Claude
//! Code JSONL in a temp dir, ingested, extracted, and compared with the
//! labels. Run with `--nocapture` to see the precision/recall table.

#![allow(
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::assert_is_empty
)]

mod common;

use common::{Env, Writer};

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use recalla::model::{Kind, Memory, Status};
use recalla::store::Filter;
use serde_json::{Value, json};

#[derive(Default)]
struct Tally {
    tp: u32,
    fp: u32,
    fn_: u32,
}

impl Tally {
    fn precision(&self) -> f64 {
        ratio(self.tp, self.tp + self.fp)
    }

    fn recall(&self) -> f64 {
        ratio(self.tp, self.tp + self.fn_)
    }
}

fn ratio(n: u32, d: u32) -> f64 {
    if d == 0 {
        1.0
    } else {
        f64::from(n) / f64::from(d)
    }
}

/// Greedy one-to-one match of labels to memories by kind and substring.
fn score(sessions: &[Value], memories: &[Memory]) -> BTreeMap<String, Tally> {
    let mut by_kind: BTreeMap<String, Tally> = BTreeMap::new();
    let mut used = vec![false; memories.len()];
    for label in sessions
        .iter()
        .flat_map(|s| s["expected"].as_array().unwrap())
    {
        let (kind, needle) = (
            label["kind"].as_str().unwrap(),
            label["contains"].as_str().unwrap().to_lowercase(),
        );
        let hit = memories.iter().enumerate().position(|(i, m)| {
            !used[i] && m.kind.as_str() == kind && m.text.to_lowercase().contains(&needle)
        });
        let t = by_kind.entry(kind.to_owned()).or_default();
        if let Some(i) = hit {
            used[i] = true;
            t.tp += 1;
        } else {
            t.fn_ += 1;
            println!("FN  {kind}: {needle}");
        }
    }
    for (m, _) in memories.iter().zip(&used).filter(|(_, u)| !**u) {
        by_kind.entry(m.kind.to_string()).or_default().fp += 1;
        println!("FP  {}: {}", m.kind, m.text);
    }
    by_kind
}

fn label_count(sessions: &[Value], kind: &str) -> usize {
    sessions
        .iter()
        .flat_map(|s| s["expected"].as_array().unwrap())
        .filter(|l| l["kind"] == kind)
        .count()
}

#[test]
fn eval_precision_and_recall() {
    let env = Env::new();
    let sessions = env.write_all();
    // The same session again as a Codex rollout: its memories merge with the
    // Claude ones (same labels, no extra memories) and gain Codex provenance.
    env.write_codex_session(rust_cli(&sessions));
    env.backfill();
    let by_kind = score(&sessions, &env.active());
    let harnesses = |m: &Memory| -> Vec<String> {
        m.provenance
            .iter()
            .filter_map(|p| p.harness.clone())
            .collect()
    };
    let unwrap = env
        .active()
        .into_iter()
        .find(|m| m.text.contains("Never use unwrap in library code"))
        .unwrap();
    let seen = harnesses(&unwrap);
    assert!(
        seen.iter().any(|h| h == "codex") && seen.iter().any(|h| h == "claude-code"),
        "{seen:?}"
    );

    println!(
        "{:<12}{:>4}{:>4}{:>4}{:>8}{:>8}",
        "kind", "TP", "FP", "FN", "P", "R"
    );
    let mut all = Tally::default();
    for (kind, t) in &by_kind {
        println!(
            "{kind:<12}{:>4}{:>4}{:>4}{:>8.2}{:>8.2}",
            t.tp,
            t.fp,
            t.fn_,
            t.precision(),
            t.recall()
        );
        (all.tp, all.fp, all.fn_) = (all.tp + t.tp, all.fp + t.fp, all.fn_ + t.fn_);
    }
    println!(
        "{:<12}{:>4}{:>4}{:>4}{:>8.2}{:>8.2}",
        "overall",
        all.tp,
        all.fp,
        all.fn_,
        all.precision(),
        all.recall()
    );

    // The fixtures include sessions that must yield nothing (subagent
    // prompts, questions, compound non-procedures): any extra memory is a bug.
    assert_eq!(all.fp, 0, "unexpected memories extracted");
    assert!(
        all.precision() >= 0.80,
        "overall precision {}",
        all.precision()
    );
    for (kind, t) in &by_kind {
        if label_count(&sessions, kind) >= 5 {
            assert!(t.precision() >= 0.80, "{kind} precision {}", t.precision());
        }
    }
}

/// Imperative prompts ("Decision for this project: ...", "Rule: ...", "We always ...",
/// "Going forward, ..."): every label found, nothing extra, including the near-misses.
#[test]
fn imperative_prompts_are_extracted_exactly() {
    let doc: Value = serde_json::from_str(
        &fs::read_to_string("tests/fixtures/extract/imperative.json").unwrap(),
    )
    .unwrap();
    let sessions = doc["sessions"].as_array().unwrap().clone();
    let env = Env::new();
    for s in &sessions {
        env.write_session(s);
    }
    env.backfill();
    let by_kind = score(&sessions, &env.active());
    for (kind, t) in &by_kind {
        println!("{kind:<12} tp {} fp {} fn {}", t.tp, t.fp, t.fn_);
        assert_eq!((t.fp, t.fn_), (0, 0), "{kind}");
    }
    assert!(!by_kind.is_empty());
}

fn rust_cli(sessions: &[Value]) -> &Value {
    sessions.iter().find(|s| s["id"] == "rust-cli").unwrap()
}

#[test]
fn codex_format_session_scores_like_the_claude_one() {
    let sessions: Value =
        serde_json::from_str(&fs::read_to_string(common::SESSIONS).unwrap()).unwrap();
    let cli = rust_cli(sessions["sessions"].as_array().unwrap()).clone();
    let tallies = |by_kind: &BTreeMap<String, Tally>| -> Vec<(String, u32, u32, u32)> {
        by_kind
            .iter()
            .map(|(k, t)| (k.clone(), t.tp, t.fp, t.fn_))
            .collect()
    };
    let claude = Env::new();
    claude.write_session(&cli);
    claude.backfill();
    let want = tallies(&score(std::slice::from_ref(&cli), &claude.active()));

    let only = Env::new();
    only.write_codex_session(&cli);
    let (ing, _) = only.backfill();
    assert_eq!(ing.files_by_harness.get("codex"), Some(&1));
    let memories = only.active();
    // Same labels, same per-kind outcome as the Claude-format run, no FP.
    let got = tallies(&score(&[cli], &memories));
    assert_eq!(got, want);
    assert!(
        got.iter().all(|(_, tp, fp, _)| *fp == 0 && *tp > 0),
        "{got:?}"
    );
    assert!(!memories.is_empty());
    for m in &memories {
        assert!(
            m.provenance
                .iter()
                .all(|p| p.harness.as_deref() == Some("codex")),
            "{:?}",
            m.provenance
        );
        let p = &m.provenance[0];
        assert_eq!(p.session_id.as_deref(), Some("eval-rust-cli-codex"));
        let path = p.source_path.as_deref().unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(
            text.lines()
                .nth(usize::try_from(p.line.unwrap()).unwrap() - 1)
                .is_some()
        );
    }
}

#[test]
fn second_run_is_a_no_op() {
    let env = Env::new();
    env.write_all();
    let (ing, first) = env.backfill();
    assert!(first.events_processed > 0 && first.events_processed == ing.events_added);
    assert!(first.new_total() > 0);
    let before = env.active().len();

    let (ing, second) = env.backfill();
    assert_eq!(ing.events_added, 0);
    assert_eq!(second.events_processed, 0);
    assert_eq!(second.new_total() + second.duplicate_total(), 0);
    assert_eq!(env.active().len(), before);
}

#[test]
fn appended_session_processes_only_new_events() {
    let env = Env::new();
    let sessions = env.write_all();
    let (_, first) = env.backfill();

    let extra = json!({
        "id": "late", "cwd": "/tmp/eval/late",
        "turns": [
            {"user": "Always keep the changelog entries sorted by date, newest first."},
            {"assistant": "Understood, I will keep them sorted."},
        ],
    });
    env.write_session(&extra);
    let (ing, second) = env.backfill();
    assert_eq!(ing.events_added, 2);
    assert_eq!(second.events_processed, 2);
    assert_eq!(second.memories_new.get("preference"), Some(&1));
    assert_eq!(second.memories_new.len(), 1);
    assert!(sessions.len() > 1 && first.events_processed > 2);
}

#[test]
fn rebuild_archives_and_re_extracts_the_same_set() {
    let env = Env::new();
    env.write_all();
    let (_, first) = env.backfill();
    let texts = |ms: &[Memory]| {
        let mut t: Vec<_> = ms.iter().map(|m| (m.kind, m.text.clone())).collect();
        t.sort_by(|a, b| a.1.cmp(&b.1));
        t
    };
    let before = texts(&env.active());

    let rebuilt = env.extract(true);
    assert!(rebuilt.restarted);
    assert_eq!(rebuilt.events_processed, first.events_processed);
    assert_eq!(rebuilt.memories_archived_for_rebuild, before.len() as u64);
    assert_eq!(rebuilt.new_total(), 0);
    assert_eq!(texts(&env.active()), before);
    // Nothing was lost along the way: still no archived leftovers.
    assert!(
        env.store()
            .all_memories(&[Status::Archived])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn provenance_points_at_the_source_line() {
    let env = Env::new();
    env.write_all();
    env.backfill();
    let pref = env
        .active()
        .into_iter()
        .find(|m| m.kind == Kind::Preference && m.text.contains("token bucket"))
        .expect("token bucket preference");
    let p = &pref.provenance[0];
    let path = p.source_path.as_deref().unwrap();
    assert!(path.ends_with("eval-web-api.jsonl"), "{path}");
    assert_eq!(p.session_id.as_deref(), Some("eval-web-api"));
    assert_eq!(p.message_uuid.as_deref(), Some("eval-web-api-0001"));
    let line = usize::try_from(p.line.unwrap()).unwrap();
    let text = fs::read_to_string(path).unwrap();
    assert!(text.lines().nth(line - 1).unwrap().contains("token bucket"));
    assert!(
        pref.anchors
            .iter()
            .any(|a| a.path == "src/limits/bucket.rs")
    );
}

fn append(path: &Path, body: &str) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
}

#[test]
fn completing_a_task_archives_its_memory() {
    let env = Env::new();
    let cwd = "/tmp/eval/tasks";
    let todo = |status: &str| {
        json!({"tool": "TodoWrite", "result": "ok", "error": false, "input": {"todos": [
            {"content": "Migrate the billing tables", "status": status},
            {"content": "Write the rollout notes", "status": "pending"},
        ]}})
    };
    let mut w = Writer::new("eval-tasks", cwd);
    let path = env.session_path(cwd, "eval-tasks");
    fs::write(&path, w.turns(&[todo("pending")])).unwrap();
    env.backfill();
    let tasks = |env: &Env, st: Status| {
        env.store()
            .all_memories(&[st])
            .unwrap()
            .into_iter()
            .filter(|m| m.kind == Kind::Task)
            .map(|m| m.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(tasks(&env, Status::Active).len(), 2);

    append(&path, &w.turns(&[todo("completed")]));
    let (_, r) = env.backfill();
    assert_eq!(r.tasks_archived, 1);
    assert_eq!(
        tasks(&env, Status::Archived),
        ["Migrate the billing tables"]
    );
    assert_eq!(tasks(&env, Status::Active), ["Write the rollout notes"]);
}

#[test]
fn search_finds_the_token_bucket_preference() {
    let env = Env::new();
    env.write_all();
    env.backfill();
    let hits = env
        .store()
        .search("token bucket", &Filter::default(), 10)
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.memory.kind == Kind::Preference && h.memory.text.contains("token bucket"))
    );

    let (code, out, _) = env.cli(&["search", "token bucket", "--kind", "preference"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("preference") && out.contains("token bucket"),
        "{out}"
    );
    assert!(out.contains("from eval-we"), "{out}");
    let (code, out, _) = env.cli(&["search", "token bucket", "--json", "--limit", "1"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((code, v.as_array().unwrap().len()), (0, 1));
    assert!(v[0]["provenance"][0]["line"].is_number());
}

#[test]
fn cli_missing_db_exits_2_and_creates_nothing() {
    let env = Env::new();
    for args in [&["extract"][..], &["search", "anything"]] {
        let (code, out, err) = env.cli(args);
        assert_eq!((code, out.as_str()), (2, ""));
        assert!(err.contains("no database"), "{err}");
    }
    assert!(!env.data.exists());
}

#[test]
fn cli_backfill_extracts_and_reports() {
    let env = Env::new();
    env.write_all();
    let (code, out, err) = env.cli(&["backfill"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("extracted") && out.contains("new memories"),
        "{out}"
    );
    let (_, out, _) = env.cli(&["extract"]);
    assert!(out.contains("0 events -> 0 new memories"), "{out}");
    let (_, out, _) = env.cli(&["extract", "--rebuild", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["restarted"], true);

    let fresh = Env::new();
    fresh.write_all();
    let (code, out, _) = fresh.cli(&["backfill", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(code == 0 && v["ingest"]["events_added"].as_u64().unwrap() > 0);
    assert!(v["extract"]["events_processed"].as_u64().unwrap() > 0);

    let skip = Env::new();
    skip.write_all();
    let (_, out, _) = skip.cli(&["backfill", "--no-extract"]);
    assert!(!out.contains("extracted"));
    assert!(
        skip.store()
            .all_memories(&[Status::Active])
            .unwrap()
            .is_empty()
    );
}
