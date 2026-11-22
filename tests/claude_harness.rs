//! Claude Code harness over synthetic fixtures in `tests/fixtures/claude/`.

#![allow(
    clippy::unwrap_used,
    clippy::unnecessary_wraps,
    clippy::assert_is_empty
)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use recalla::harness::claude::{discover, parse_session};
use recalla::harness::{Event, EventKind, ParseStats};

const MAIN: &str =
    "tests/fixtures/claude/projects/-tmp-demo/11111111-1111-4111-8111-111111111111.jsonl";
const MALFORMED: &str = "tests/fixtures/claude/projects/-tmp-demo/malformed.jsonl";
const SESSION: &str = "11111111-1111-4111-8111-111111111111";

fn parse(path: &str) -> (Vec<Event>, ParseStats, u64) {
    parse_session(Path::new(path), 0).unwrap()
}

fn uuid(n: &str) -> Option<String> {
    Some(format!("a{n:0>7}-0000-4000-8000-{n:0>12}"))
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
fn main_fixture_golden_events() {
    let (events, _, _) = parse(MAIN);
    let got: Vec<_> = events
        .iter()
        .map(|e| {
            (
                e.line,
                e.kind.clone(),
                e.text.as_str(),
                e.message_uuid.clone(),
            )
        })
        .collect();
    let want = vec![
        (
            1,
            EventKind::UserText,
            "Please list the files in the demo project.",
            uuid("1"),
        ),
        (
            3,
            EventKind::AssistantText,
            "I will list the files.",
            uuid("2"),
        ),
        (
            3,
            call("toolu_demo01", "Bash"),
            r#"{"command":"ls -la"}"#,
            uuid("2"),
        ),
        (
            4,
            result("toolu_demo01", false),
            "Cargo.toml\nsrc",
            uuid("3"),
        ),
        (
            6,
            EventKind::AssistantText,
            "There is a manifest and a source folder.",
            uuid("5"),
        ),
        (
            7,
            result("toolu_demo02", false),
            "first part\nsecond part",
            uuid("6"),
        ),
        (9, EventKind::UserText, "Now open main.rs.", uuid("8")),
        (
            10,
            call("toolu_demo03", "Read"),
            r#"{"file_path":"/tmp/demo/src/main.rs"}"#,
            uuid("9"),
        ),
        (
            11,
            result("toolu_demo03", true),
            "File does not exist.",
            uuid("a"),
        ),
        (
            14,
            EventKind::Summary,
            "This session lists files in the demo project.",
            uuid("b"),
        ),
        (15, EventKind::Summary, "Listing demo project files", None),
        (
            16,
            EventKind::AssistantText,
            "Side task finished.",
            uuid("c"),
        ),
        (
            17,
            EventKind::UserText,
            "日本語のテストです 🎉 café",
            uuid("d"),
        ),
        (18, EventKind::UserText, "/clear\nnow", uuid("e")),
        (20, EventKind::AssistantText, "Done.", uuid("10")),
    ];
    assert_eq!(got, want);
}

#[test]
fn main_fixture_provenance_fields() {
    let (events, _, _) = parse(MAIN);
    let first = &events[0];
    assert_eq!(first.harness, "claude-code");
    assert_eq!(first.session_id, SESSION);
    assert_eq!(first.byte_offset, 0);
    assert_eq!(first.parent_uuid, None);
    assert_eq!(first.timestamp_ms, Some(1_735_787_045_678));
    assert_eq!(first.cwd.as_deref(), Some("/tmp/demo"));
    assert_eq!(first.git_branch.as_deref(), Some("main"));
    assert!(!first.sidechain);
    assert_eq!(first.source_path, Path::new(MAIN));
    assert_eq!(events[1].parent_uuid, uuid("1"));
    let side = events.iter().find(|e| e.line == 16).unwrap();
    assert!(side.sidechain);
    assert_eq!(side.git_branch.as_deref(), Some("feature/x"));
    // +02:00 offset is applied
    let offset = events.iter().find(|e| e.line == 17).unwrap();
    assert_eq!(offset.timestamp_ms, Some(1_735_779_856_000));
}

#[test]
fn main_fixture_stats() {
    let (events, stats, next) = parse(MAIN);
    assert_eq!(stats.lines, 20);
    assert_eq!(stats.events, 15);
    assert_eq!(stats.events, events.len() as u64);
    assert_eq!(stats.malformed_lines, 0);
    assert!(!stats.partial_tail);
    let counts = |m: &std::collections::BTreeMap<String, u64>| {
        m.iter().map(|(k, v)| (k.clone(), *v)).collect::<Vec<_>>()
    };
    assert_eq!(
        counts(&stats.skipped_record_types),
        [
            ("api-request", 1),
            ("attachment", 1),
            ("file-history-snapshot", 1),
            ("mode", 1),
            ("user(meta)", 1),
        ]
        .map(|(k, v)| (k.to_owned(), v))
    );
    assert_eq!(
        counts(&stats.skipped_blocks),
        [("image", 1), ("server_widget", 1), ("thinking", 1)].map(|(k, v)| (k.to_owned(), v))
    );
    assert_eq!(next, fs::metadata(MAIN).unwrap().len());
}

#[test]
fn no_dropped_content_leaks_into_events() {
    let (events, _, _) = parse(MAIN);
    for e in &events {
        for banned in [
            "injected",
            "placeholder reasoning",
            "placeholder-signature",
            "meta caveat",
            "AAAA",
            "cleared",
        ] {
            assert!(!e.text.contains(banned), "{banned} in {:?}", e.text);
        }
    }
}

#[test]
fn resume_from_any_event_offset_matches_the_tail() {
    let (events, _, _) = parse(MAIN);
    for e in &events {
        let (tail, stats, _) = parse_session(Path::new(MAIN), e.byte_offset).unwrap();
        let want: Vec<_> = events
            .iter()
            .filter(|x| x.byte_offset >= e.byte_offset)
            .cloned()
            .collect();
        assert_eq!(tail, want, "from line {}", e.line);
        assert_eq!(stats.events, want.len() as u64);
    }
}

#[test]
fn resume_at_end_is_empty() {
    let len = fs::metadata(MAIN).unwrap().len();
    let (events, stats, next) = parse_session(Path::new(MAIN), len).unwrap();
    assert!(events.is_empty() && stats.lines == 0 && next == len);
}

#[test]
fn resume_past_end_is_an_error() {
    let len = fs::metadata(MAIN).unwrap().len();
    let err = parse_session(Path::new(MAIN), len + 1).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn malformed_lines_are_counted_and_skipped() {
    let (events, stats, _) = parse(MALFORMED);
    assert_eq!(stats.lines, 5);
    assert_eq!(stats.malformed_lines, 2); // broken JSON + a JSON array; blank is ignored
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].text, "before the bad line");
    assert_eq!(
        (events[1].text.as_str(), events[1].line),
        ("after the bad line", 5)
    );
    // no sessionId: falls back to the file stem; bad timestamp is None
    assert_eq!(events[1].session_id, "malformed");
    assert_eq!(events[1].timestamp_ms, None);
}

fn write_main_copy(dir: &Path) -> PathBuf {
    let path = dir.join("s.jsonl");
    fs::copy(MAIN, &path).unwrap();
    path
}

#[test]
fn partial_tail_is_not_consumed_until_completed() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_main_copy(dir.path());
    let full_len = fs::metadata(&path).unwrap().len();
    let rec = r#"{"type":"user","uuid":"late","sessionId":"s","message":{"role":"user","content":"late arrival"}}"#;
    let (first, second) = rec.split_at(40);
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(first.as_bytes()).unwrap();

    let (events, stats, next) = parse_session(&path, 0).unwrap();
    assert!(stats.partial_tail);
    assert_eq!((events.len(), next), (15, full_len));

    writeln!(f, "{second}").unwrap();
    let (events, stats, next2) = parse_session(&path, next).unwrap();
    assert!(!stats.partial_tail);
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].text.as_str(), events[0].line),
        ("late arrival", 21)
    );
    assert_eq!(next2, fs::metadata(&path).unwrap().len());
}

#[test]
fn invalid_utf8_line_is_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let mut bytes = b"{\"type\":\"user\",\"message\":{\"content\":\"\xff\xfe\"}}\n".to_vec();
    bytes.extend_from_slice(br#"{"type":"assistant","message":{"content":"ok"}}"#);
    bytes.push(b'\n');
    fs::write(&path, bytes).unwrap();
    let (events, stats, _) = parse_session(&path, 0).unwrap();
    assert_eq!((stats.malformed_lines, events.len()), (1, 1));
    assert_eq!(events[0].text, "ok");
}

#[test]
fn huge_line_is_kept_whole_for_redaction() {
    // Truncation happens in ingest after redaction, never in the harness.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let text = format!("{}{}", "a".repeat(100_000), "日".repeat(100_000));
    let rec = format!(r#"{{"type":"user","message":{{"content":"{text}"}}}}"#);
    fs::write(&path, format!("{rec}\n")).unwrap();
    let (events, _, _) = parse_session(&path, 0).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].text, text);
}

#[cfg(unix)]
#[test]
fn symlinked_subagents_dir_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let sess = root.join("projects/p/sess");
    fs::create_dir_all(&sess).unwrap();
    fs::write(root.join("projects/p/sess.jsonl"), "").unwrap();
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("leak.jsonl"), "").unwrap();
    std::os::unix::fs::symlink(&outside, sess.join("subagents")).unwrap();
    let found = discover(&root).unwrap();
    assert!(found.iter().all(|f| f.path.starts_with(&root)), "{found:?}");
    assert!(!found.iter().any(|f| f.is_subagent));
}

#[test]
fn discovers_main_and_subagent_files_sorted() {
    let found = discover(Path::new("tests/fixtures/claude")).unwrap();
    let got: Vec<_> = found
        .iter()
        .map(|f| {
            (
                f.path.file_name().unwrap().to_str().unwrap(),
                f.is_subagent,
                f.project_dir_name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "11111111-1111-4111-8111-111111111111.jsonl",
                false,
                "-tmp-demo"
            ),
            ("agent-x.jsonl", true, "-tmp-demo"),
            ("malformed.jsonl", false, "-tmp-demo"),
        ]
    );
}

#[test]
fn subagent_file_parses() {
    let found = discover(Path::new("tests/fixtures/claude")).unwrap();
    let sub = found.iter().find(|f| f.is_subagent).unwrap();
    let (events, _, _) = parse_session(&sub.path, 0).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.sidechain));
}

#[test]
fn missing_root_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(discover(&dir.path().join("nope")).unwrap().is_empty());
    assert!(discover(dir.path()).unwrap().is_empty()); // root without projects/
}

#[cfg(unix)]
#[test]
fn symlinks_are_skipped() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let proj = dir.path().join("projects/p");
    fs::create_dir_all(&proj).unwrap();
    fs::write(proj.join("real.jsonl"), "").unwrap();
    fs::write(outside.path().join("secret.jsonl"), "").unwrap();
    symlink(outside.path().join("secret.jsonl"), proj.join("link.jsonl")).unwrap();
    // a symlinked session dir and a symlinked project dir pointing outside
    fs::create_dir_all(outside.path().join("subagents")).unwrap();
    fs::write(outside.path().join("subagents/x.jsonl"), "").unwrap();
    symlink(outside.path(), proj.join("sess")).unwrap();
    symlink(outside.path(), dir.path().join("projects/q")).unwrap();

    let found = discover(dir.path()).unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].path.ends_with("p/real.jsonl"));
}
