//! Codex harness over the synthetic fixture in `tests/fixtures/codex/`.

#![allow(clippy::unwrap_used, clippy::assert_is_empty, clippy::too_many_lines)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use recalla::harness::codex::{discover, parse_bytes, parse_session, shell_string};
use recalla::harness::{Event, EventKind, ParseStats};
use serde_json::json;

const ROOT: &str = "tests/fixtures/codex";
const SESSION: &str = "019b7a10-0000-7000-8000-000000000001";

fn fixture() -> PathBuf {
    let found = discover(Path::new(ROOT)).unwrap();
    assert_eq!(found.len(), 1);
    found[0].path.clone()
}

fn parse() -> (Vec<Event>, ParseStats, u64) {
    parse_session(&fixture(), 0).unwrap()
}

fn call(id: &str, name: &str) -> EventKind {
    EventKind::ToolCall {
        id: id.into(),
        name: name.into(),
    }
}

fn result(id: &str, is_error: bool) -> EventKind {
    EventKind::ToolResult {
        tool_use_id: id.into(),
        is_error,
    }
}

#[test]
fn golden_events() {
    let (events, _, _) = parse();
    let got: Vec<_> = events
        .iter()
        .map(|e| {
            (
                e.line,
                e.kind.clone(),
                e.text.as_str(),
                e.message_uuid.as_deref(),
                e.cwd.as_deref(),
            )
        })
        .collect();
    let demo = Some("/tmp/demo");
    let sub = Some("/tmp/demo/sub");
    let want = vec![
        (
            4,
            EventKind::UserText,
            "Please run the tests.",
            Some("msg_user_1"),
            demo,
        ),
        (
            7,
            EventKind::AssistantText,
            "I will run them now. 日本語 ✓",
            Some("msg_asst_1"),
            demo,
        ),
        (
            8,
            call("item_cmd1", "Bash"),
            r#"{"command":"cargo test"}"#,
            Some("item_cmd1"),
            demo,
        ),
        (
            8,
            result("item_cmd1", true),
            "1 test failed",
            Some("item_cmd1"),
            demo,
        ),
        (
            9,
            call("item_cmd2", "Bash"),
            r#"{"command":"git diff --quiet"}"#,
            Some("item_cmd2"),
            demo,
        ),
        (9, result("item_cmd2", false), "", Some("item_cmd2"), demo),
        (
            10,
            call("item_fc1", "Edit"),
            r#"{"file_path":"src/a.rs"}"#,
            Some("item_fc1"),
            demo,
        ),
        (
            10,
            call("item_fc1", "Edit"),
            r#"{"file_path":"src/b.rs"}"#,
            Some("item_fc1"),
            demo,
        ),
        (
            14,
            EventKind::UserText,
            "Commit with message ‘fix’ please.",
            Some("msg_user_2"),
            sub,
        ),
        (
            15,
            EventKind::Summary,
            "The user asked for tests and a commit.",
            None,
            sub,
        ),
        (
            19,
            EventKind::AssistantText,
            "All done.",
            Some("msg_asst_2"),
            sub,
        ),
    ];
    assert_eq!(got, want);
    for e in &events {
        assert_eq!(
            (e.harness, e.session_id.as_str(), e.sidechain),
            ("codex", SESSION, false)
        );
        assert!(e.timestamp_ms.is_some());
    }
    // The injected environment block never reaches stored text.
    assert!(
        events
            .iter()
            .all(|e| !e.text.contains("environment_context"))
    );
}

#[test]
fn stats_count_what_was_dropped() {
    let (_, stats, next) = parse();
    assert_eq!(
        (stats.lines, stats.events, stats.malformed_lines),
        (19, 11, 1)
    );
    assert!(!stats.partial_tail);
    assert_eq!(next, fs::metadata(fixture()).unwrap().len());
    let skipped: Vec<_> = stats
        .skipped_record_types
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    assert_eq!(
        skipped,
        [
            ("custom_tool_call", 1),
            ("item_completed/AgentMessage", 1),
            ("item_completed/UserMessage", 1),
            ("message(developer)", 1),
            ("reasoning", 1),
            ("session_meta", 1),
            ("token_count", 1),
            ("turn_context", 2),
            ("world_state", 1),
        ]
    );
}

#[test]
fn resume_from_every_line_start_matches_a_full_parse() {
    let path = fixture();
    let (all, _, _) = parse();
    let bytes = fs::read(&path).unwrap();
    let mut offsets = vec![0u64];
    offsets.extend(
        bytes
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i as u64 + 1),
    );
    for off in offsets {
        let (tail, _, next) = parse_session(&path, off).unwrap();
        let want: Vec<_> = all
            .iter()
            .filter(|e| e.byte_offset >= off)
            .cloned()
            .collect();
        assert_eq!(tail, want, "offset {off}");
        assert_eq!(next, bytes.len() as u64);
    }
    assert!(parse_session(&path, bytes.len() as u64 + 1).is_err());
}

#[test]
fn partial_tail_is_left_unconsumed_then_completed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-x.jsonl");
    let full = fs::read(fixture()).unwrap();
    let cut = full.len() - 5; // mid last line
    fs::write(&path, &full[..cut]).unwrap();
    let (events, stats, next) = parse_session(&path, 0).unwrap();
    assert!(stats.partial_tail);
    assert!(events.iter().all(|e| e.line < 19));
    let last_line_start = full[..full.len() - 1]
        .iter()
        .rposition(|&b| b == b'\n')
        .unwrap()
        + 1;
    assert_eq!(next, last_line_start as u64);

    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&full[cut..]).unwrap();
    let (more, stats, end) = parse_session(&path, next).unwrap();
    assert!(!stats.partial_tail);
    assert_eq!((more.len(), more[0].line, end), (1, 19, full.len() as u64));
    // The session id survives the resume even though session_meta was skipped.
    assert_eq!(more[0].session_id, SESSION);
}

#[test]
fn parse_bytes_without_a_readable_prefix_falls_back_to_the_stem() {
    let line = br#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#;
    let mut bytes = line.to_vec();
    bytes.push(b'\n');
    let (events, _, n) = parse_bytes(&bytes, 100, 7, Path::new("/nope/rollout-abc.jsonl"));
    assert_eq!(n, bytes.len() as u64);
    assert_eq!(
        (
            events[0].session_id.as_str(),
            events[0].line,
            events[0].byte_offset
        ),
        ("rollout-abc", 8, 100)
    );
}

#[test]
fn discovery_is_recursive_sorted_and_refuses_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let day = |d: &str| {
        let p = root.join("sessions/2026/01").join(d);
        fs::create_dir_all(&p).unwrap();
        p
    };
    for (d, name) in [("03", "rollout-b.jsonl"), ("02", "rollout-a.jsonl")] {
        fs::write(day(d).join(name), "{}\n").unwrap();
    }
    fs::write(day("02").join("notes.txt"), "x").unwrap();
    // A symlinked file and a symlinked directory pointing outside the tree.
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("rollout-evil.jsonl"), "{}\n").unwrap();
    symlink(
        outside.path().join("rollout-evil.jsonl"),
        day("02").join("link.jsonl"),
    )
    .unwrap();
    symlink(outside.path(), root.join("sessions/2026/linked")).unwrap();

    let names: Vec<_> = discover(root)
        .unwrap()
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["rollout-a.jsonl", "rollout-b.jsonl"]);

    // A symlinked `sessions` directory is refused too.
    let other = tempfile::tempdir().unwrap();
    symlink(root.join("sessions"), other.path().join("sessions")).unwrap();
    assert!(discover(other.path()).unwrap().is_empty());
    assert!(discover(&root.join("missing")).unwrap().is_empty());
}

#[test]
fn shell_string_shapes() {
    let s = |v| shell_string(&v);
    let some = |t: &str| Some(t.to_owned());
    assert_eq!(
        s(json!(["bash", "-lc", "cargo test && ls"])),
        some("cargo test && ls")
    );
    assert_eq!(s(json!(["/bin/zsh", "-c", "echo hi"])), some("echo hi"));
    assert_eq!(
        s(json!(["fish", "-c", "echo hi", "extra"])),
        some("echo hi")
    );
    // not a shell wrapper: shell-joined
    assert_eq!(
        s(json!(["git", "log", "--oneline"])),
        some("git log --oneline")
    );
    assert_eq!(s(json!(["bash", "script.sh"])), some("bash script.sh"));
    assert_eq!(
        s(json!(["python", "-c", "print(1)"])),
        some("python -c 'print(1)'")
    );
    assert_eq!(s(json!(["bash", "-x", "a b"])), some("bash -x 'a b'"));
    assert_eq!(s(json!(["echo", "it's", ""])), some(r"echo 'it'\''s' ''"));
    assert_eq!(s(json!("ls -la")), some("ls -la"));
    assert_eq!(s(json!([])), some(""));
    assert_eq!(s(json!([1, 2])), None);
    assert_eq!(s(json!(null)), None);
}
