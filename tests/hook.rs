//! `recalla hook` as a child process: stdin JSON in, stdout/exit code/log out.

#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::Env;
use recalla::model::{Kind, NewMemory, Origin, Provenance, Scope};
use recalla::paths;
use recalla::store::Store;
use serde_json::{Value, json};

const PROJECT: &str = "/work/hook-it";
const FACT: &str = "Hook tests prefer the blue deploy lane";

fn hook(env: &Env, event: &str, stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_recalla"))
        .args(["hook", event, "--data-dir"])
        .arg(&env.data)
        .env("CLAUDE_CONFIG_DIR", &env.root)
        .env_remove("RECALLA_BUDGET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn log(env: &Env) -> String {
    fs::read_to_string(env.data.join("log/hook.log")).unwrap_or_default()
}

fn assert_silent_ok(out: &Output) {
    assert!(out.status.success(), "{:?}", out.status);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);
}

fn seed_memory(env: &Env) {
    let mut store = recalla::ops::open_or_create(&env.data).unwrap();
    let new = NewMemory {
        kind: Kind::Preference,
        text: FACT.into(),
        scope: Scope::Project(PROJECT.into()),
        confidence: 0.9,
        importance: 3,
    };
    store
        .insert(&new, &Provenance::new(Origin::Remember))
        .unwrap();
}

fn start_payload() -> String {
    json!({"session_id": "s", "cwd": PROJECT, "source": "startup"}).to_string()
}

/// A session transcript with a clear preference, inside the temp Claude root.
fn transcript(env: &Env) -> PathBuf {
    env.write_session(&json!({
        "id": "hook", "cwd": PROJECT,
        "turns": [{"user": "Always run make check before pushing in this repo."}],
    }))
}

fn capture_payload(path: &Path) -> String {
    json!({"transcript_path": path, "hook_event_name": "Stop"}).to_string()
}

#[test]
fn session_start_injects_pack_and_records_hook_injection() {
    let env = Env::new();
    seed_memory(&env);
    let out = hook(&env, "session-start", &start_payload());
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.lines().count(), 1, "one line: {text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(text.starts_with(
        r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"#
    ));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    assert!(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains(FACT)
    );
    let conn = rusqlite::Connection::open(paths::db_path(&env.data)).unwrap();
    let vias: Vec<String> = conn
        .prepare("SELECT via FROM injections")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(vias, ["hook"]);
    let (session, pack, tokens): (String, String, i64) = conn
        .query_row(
            "SELECT session_id, pack_text, candidate_tokens FROM injections",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(session, "s");
    assert_eq!(
        pack,
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
    );
    assert!(tokens > 0);
}

#[test]
fn session_start_without_memories_or_db_prints_nothing() {
    let env = Env::new();
    assert_silent_ok(&hook(&env, "session-start", &start_payload()));
    assert!(!env.data.exists(), "no DB must not be created");
    // DB exists but nothing matches this project.
    seed_memory(&env);
    let other = json!({"cwd": "/work/elsewhere"}).to_string();
    assert_silent_ok(&hook(&env, "session-start", &other));
}

#[test]
fn malformed_stdin_exits_zero_and_logs_without_content() {
    let env = Env::new();
    for event in ["session-start", "capture"] {
        let out = hook(&env, event, "{\"cwd\": \"top-secret-value\" oops");
        assert_silent_ok(&out);
    }
    let text = log(&env);
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.contains("session-start input") && text.contains("capture input"));
    assert!(!text.contains("top-secret-value"), "{text}");
    assert_silent_ok(&hook(&env, "capture", "[1]"));
    assert_silent_ok(&hook(&env, "session-start", "{}"));
    assert_eq!(log(&env).lines().count(), 4);
}

#[cfg(unix)]
#[test]
fn log_dir_and_file_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    hook(&env, "capture", "nope");
    let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(env.data.join("log")), 0o700);
    assert_eq!(mode(env.data.join("log/hook.log")), 0o600);
}

#[test]
fn capture_ingests_one_file_extracts_and_backfill_adds_nothing() {
    let env = Env::new();
    let wanted = transcript(&env);
    // A second transcript that must NOT be ingested by the hook.
    env.write_session(&json!({
        "id": "other", "cwd": PROJECT,
        "turns": [{"user": "Never touch the vendored folder."}],
    }));
    assert_silent_ok(&hook(&env, "capture", &capture_payload(&wanted)));
    assert_eq!(log(&env), "");

    let store = env.store();
    let sources = store.sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].path,
        fs::canonicalize(&wanted).unwrap().to_string_lossy()
    );
    assert!(
        env.active().iter().any(|m| m.text.contains("make check")),
        "extraction ran"
    );
    drop(store);

    // Idempotent hook, then a full backfill: same source key, no duplicates.
    assert_silent_ok(&hook(&env, "capture", &capture_payload(&wanted)));
    let report = env.ingest();
    assert_eq!(report.files_seen, 2);
    assert_eq!(
        report.files_unchanged, 1,
        "captured file is not re-ingested"
    );
    assert_eq!(report.files_new, 1);
    assert_eq!(env.store().sources().unwrap().len(), 2);
}

#[test]
fn capture_resumes_a_growing_transcript() {
    let env = Env::new();
    let path = transcript(&env);
    assert_silent_ok(&hook(&env, "capture", &capture_payload(&path)));
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    let more = json!({"type": "user", "uuid": "u-extra", "sessionId": "eval-hook", "cwd": PROJECT,
        "timestamp": "2026-11-01T11:00:00.000Z", "message": {"role": "user", "content": "Never edit generated files."}});
    writeln!(f, "{more}").unwrap();
    assert_silent_ok(&hook(&env, "capture", &capture_payload(&path)));
    let report = env.ingest();
    assert_eq!((report.files_unchanged, report.events_added), (1, 0));
}

/// The Stop hook can fire before the last records of a turn are written, and a
/// finished session never fires it again: the next session start catches up.
#[test]
fn session_start_catches_up_a_transcript_tail_that_capture_missed() {
    let env = Env::new();
    let path = transcript(&env);
    assert_silent_ok(&hook(&env, "capture", &capture_payload(&path)));
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    let late = json!({"type": "user", "uuid": "u-late", "sessionId": "eval-hook", "cwd": PROJECT,
        "timestamp": "2026-11-01T11:00:00.000Z", "message": {"role": "user", "content": "Never edit generated files."}});
    writeln!(f, "{late}").unwrap();
    let out = hook(&env, "session-start", &start_payload());
    assert!(out.status.success(), "{:?}", out.status);
    assert!(log(&env).is_empty(), "{}", log(&env));
    let report = env.ingest();
    assert_eq!((report.files_unchanged, report.events_added), (1, 0));
}

/// The session that is starting is not re-read: it is still being written.
#[test]
fn session_start_skips_its_own_transcript() {
    let env = Env::new();
    let path = transcript(&env);
    assert_silent_ok(&hook(&env, "capture", &capture_payload(&path)));
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(f, "{}", json!({"type": "user", "uuid": "u2", "sessionId": "eval-hook", "cwd": PROJECT,
        "timestamp": "2026-11-01T11:00:00.000Z", "message": {"role": "user", "content": "Never edit generated files."}})).unwrap();
    let payload =
        json!({"session_id": "eval-hook", "cwd": PROJECT, "source": "resume"}).to_string();
    hook(&env, "session-start", &payload);
    assert_eq!(env.ingest().events_added, 1);
}

#[test]
fn capture_refuses_paths_outside_the_claude_root() {
    let env = Env::new();
    let outside = env.root.parent().unwrap().join("elsewhere.jsonl");
    fs::copy(transcript(&env), &outside).unwrap();
    let in_root_not_projects = env.root.join("history.jsonl");
    fs::copy(&outside, &in_root_not_projects).unwrap();
    for bad in [
        outside.clone(),
        in_root_not_projects,
        // `..` escape that starts inside projects/
        env.root.join("projects/../../elsewhere.jsonl"),
    ] {
        assert_silent_ok(&hook(&env, "capture", &capture_payload(&bad)));
    }
    let text = log(&env);
    assert_eq!(text.matches(" capture refused: ").count(), 3, "{text}");
    assert!(!paths::db_path(&env.data).exists(), "nothing was ingested");
}

#[cfg(unix)]
#[test]
fn capture_refuses_symlink_escaping_the_root() {
    let env = Env::new();
    let secret = env.root.parent().unwrap().join("secret.jsonl");
    fs::write(&secret, "{}\n").unwrap();
    let link = env.session_path(PROJECT, "link");
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    assert_silent_ok(&hook(&env, "capture", &capture_payload(&link)));
    assert!(log(&env).contains("refused"));
}

#[test]
fn capture_is_silent_even_when_the_file_is_missing() {
    let env = Env::new();
    let missing = env.root.join("projects/x/none.jsonl");
    assert_silent_ok(&hook(&env, "capture", &capture_payload(&missing)));
    assert!(log(&env).contains("capture io"));
}

#[test]
fn locked_database_gives_up_within_bounds() {
    let env = Env::new();
    seed_memory(&env);
    let lock = rusqlite::Connection::open(paths::db_path(&env.data)).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    let out = hook(&env, "session-start", &start_payload());
    assert_silent_ok(&out);
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(log(&env).contains("session-start busy"), "{}", log(&env));
    drop(lock);
    let _ = Store::open(paths::db_path(&env.data)).unwrap();
}

#[test]
fn capture_updates_the_mirror_and_still_prints_nothing() {
    let env = Env::new();
    let path = transcript(&env);
    let out = hook(&env, "capture", &capture_payload(&path));
    assert_silent_ok(&out);
    let mirror = paths::mirror_dir(&env.data);
    let files = recalla::mirror::md_files(&mirror).unwrap();
    let n = env.store().memory_uids().unwrap().len();
    assert!(n > 0, "extraction found nothing");
    assert_eq!(files.len(), n);
}

fn anchored_memory(env: &Env, text: &str, path: &str) -> String {
    let mut store = recalla::ops::open_or_create(&env.data).unwrap();
    let new = NewMemory {
        kind: Kind::Gotcha,
        text: text.into(),
        scope: Scope::Project(PROJECT.into()),
        confidence: 0.9,
        importance: 3,
    };
    let m = store
        .insert(&new, &Provenance::new(Origin::Remember))
        .unwrap();
    let uid = m.uid().to_owned();
    let anchor = recalla::model::Anchor {
        path: path.into(),
        symbol: None,
        lang: None,
        content_hash: None,
        git_commit: None,
    };
    store.add_anchor(&uid, &anchor).unwrap();
    uid
}

fn pre_tool(tool: &str, file: &str, session: &str) -> String {
    json!({"hook_event_name": "PreToolUse", "tool_name": tool, "session_id": session,
           "cwd": PROJECT, "tool_input": {"file_path": file}})
    .to_string()
}

#[test]
fn pre_tool_use_injects_anchored_memories_once_per_session() {
    let env = Env::new();
    anchored_memory(
        &env,
        "billing.rs rounds cents half-even, not half-up",
        "src/billing.rs",
    );
    anchored_memory(&env, "unrelated file note", "src/other.rs");
    let file = format!("{PROJECT}/src/billing.rs");

    let out = hook(&env, "pre-tool-use", &pre_tool("Edit", &file, "s-1"));
    assert!(out.status.success() && out.stderr.is_empty());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let h = &v["hookSpecificOutput"];
    assert_eq!(h["hookEventName"], "PreToolUse");
    assert!(
        h.get("permissionDecision").is_none(),
        "never decides permissions"
    );
    let text = h["additionalContext"].as_str().unwrap();
    assert!(
        text.contains("half-even") && !text.contains("unrelated"),
        "{text}"
    );

    // Same session, same file: nothing more. A new session gets it again.
    assert_silent_ok(&hook(&env, "pre-tool-use", &pre_tool("Read", &file, "s-1")));
    let again = hook(&env, "pre-tool-use", &pre_tool("Read", &file, "s-2"));
    assert!(!again.stdout.is_empty());
    // The ledger recorded both injections as hook injections.
    let stats = Store::open(paths::db_path(&env.data))
        .unwrap()
        .injection_stats()
        .unwrap();
    assert_eq!(stats.injections, 2);
}

#[test]
fn pre_tool_use_ignores_other_tools_outside_files_and_missing_data() {
    let env = Env::new();
    assert_silent_ok(&hook(
        &env,
        "pre-tool-use",
        &pre_tool("Read", &format!("{PROJECT}/a.rs"), "s"),
    ));
    anchored_memory(&env, "a.rs is special", "a.rs");
    for payload in [
        pre_tool("Bash", &format!("{PROJECT}/a.rs"), "s"),
        pre_tool("Read", "/elsewhere/a.rs", "s"),
        pre_tool("Read", &format!("{PROJECT}/b.rs"), "s"),
        pre_tool("Read", &format!("{PROJECT}/a.rs/../../x"), "s"),
        r#"{"tool_name":"Read"}"#.to_owned(),
        "not json".to_owned(),
    ] {
        let out = hook(&env, "pre-tool-use", &payload);
        assert!(out.status.success(), "{payload}");
        assert!(out.stdout.is_empty(), "{payload}: {:?}", out.stdout);
    }
}

#[test]
fn pre_tool_use_stays_within_its_token_cap() {
    let env = Env::new();
    for i in 0..12 {
        anchored_memory(&env, &format!("note {i}: {}", "word ".repeat(60)), "big.rs");
    }
    let out = hook(
        &env,
        "pre-tool-use",
        &pre_tool("Edit", &format!("{PROJECT}/big.rs"), "s"),
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        recalla::pack::est_tokens(text) <= 300,
        "{}",
        recalla::pack::est_tokens(text)
    );
    assert!(text.lines().count() <= 6);
}
