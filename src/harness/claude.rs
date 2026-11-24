//! Claude Code adapter: `<config>/projects/<cwd>/<session>.jsonl` (+ subagents).
//!
//! Parsing is lenient (`serde_json::Value`, D-012): unknown or missing fields
//! are counted in [`ParseStats`], never fatal. Only user/assistant/summary
//! records carry conversation; everything else is telemetry and is dropped.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    Event, EventKind, Harness, ParseStats, SessionFile, Sink, bump, count_newlines, is_jsonl,
    parse_rfc3339_ms, real_entries, strip_spans,
};

const HARNESS_NAME: &str = "claude-code";

/// The [`Harness`] for Claude Code.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeCode;

impl Harness for ClaudeCode {
    fn name(&self) -> &'static str {
        HARNESS_NAME
    }

    fn discover(&self, root: &Path) -> io::Result<Vec<SessionFile>> {
        discover(root)
    }

    fn parse(&self, path: &Path, from_offset: u64) -> io::Result<(Vec<Event>, ParseStats, u64)> {
        parse_session(path, from_offset)
    }

    fn parse_bytes(
        &self,
        bytes: &[u8],
        base_offset: u64,
        base_line: u64,
        path: &Path,
    ) -> (Vec<Event>, ParseStats, u64) {
        parse_bytes(bytes, base_offset, base_line, path)
    }
}

/// Config dir from the environment: `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
#[must_use]
pub fn default_root() -> Option<PathBuf> {
    root_from_env(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        crate::paths::home_dir(),
    )
}

/// Pure form of [`default_root`]. Empty values count as unset.
#[must_use]
pub fn root_from_env(config_dir: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let non_empty = |v: Option<OsString>| v.filter(|s| !s.is_empty()).map(PathBuf::from);
    non_empty(config_dir).or_else(|| non_empty(home).map(|h| h.join(".claude")))
}

/// Find `projects/*/*.jsonl` and `projects/*/*/subagents/*.jsonl` under
/// `root`, skipping symlinks, sorted by path. A missing root gives `[]`.
///
/// # Errors
/// I/O errors other than a missing directory.
pub fn discover(root: &Path) -> io::Result<Vec<SessionFile>> {
    let mut found = Vec::new();
    for (project, is_dir) in real_entries(&root.join("projects"))? {
        if !is_dir {
            continue;
        }
        let project_dir_name = project
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for (path, is_dir) in real_entries(&project)? {
            if is_dir {
                for (sub, sub_is_dir) in real_entries(&path.join("subagents"))? {
                    if !sub_is_dir && is_jsonl(&sub) {
                        found.push(SessionFile {
                            path: sub,
                            project_dir_name: project_dir_name.clone(),
                            is_subagent: true,
                        });
                    }
                }
            } else if is_jsonl(&path) {
                found.push(SessionFile {
                    path,
                    project_dir_name: project_dir_name.clone(),
                    is_subagent: false,
                });
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// Parse complete lines of a session file starting at `from_offset` (a line
/// start). Returns events, stats and the offset to resume from. A final chunk
/// without `\n` is left unconsumed (`partial_tail`), so a file still being
/// written is never half-read.
///
/// Reads the tail `[from_offset, len)` into memory and hands it to
/// [`parse_bytes`]; callers parse one file at a time.
///
/// # Errors
/// I/O errors, or `from_offset` beyond the end of the file (truncated or
/// replaced file; the caller decides whether to rescan).
pub fn parse_session(path: &Path, from_offset: u64) -> io::Result<(Vec<Event>, ParseStats, u64)> {
    let mut file = File::open(path)?;
    if from_offset > file.metadata()?.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resume offset is past the end of the file",
        ));
    }
    // Event.line is absolute, so resuming still has to count preceding lines.
    let base_line = count_newlines(&mut file, from_offset)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let (events, stats, next_rel) = parse_bytes(&bytes, from_offset, base_line, path);
    Ok((events, stats, from_offset + next_rel))
}

/// Parse the complete lines of `bytes`, which start at file offset
/// `base_offset` (a line start) after `base_line` lines. `path` is only
/// recorded as provenance. Returns events, stats and how many bytes were
/// consumed; a final chunk without `\n` is left unconsumed (`partial_tail`).
#[must_use]
pub fn parse_bytes(
    bytes: &[u8],
    base_offset: u64,
    base_line: u64,
    path: &Path,
) -> (Vec<Event>, ParseStats, u64) {
    let fallback_session = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (mut events, mut stats) = (Vec::new(), ParseStats::default());
    let (mut consumed, mut line_no) = (0usize, base_line);
    while consumed < bytes.len() {
        let rest = &bytes[consumed..];
        let Some(end) = rest.iter().position(|&b| b == b'\n') else {
            stats.partial_tail = true;
            break;
        };
        let line = &rest[..=end];
        line_no += 1;
        stats.lines += 1;
        let at = Position {
            path,
            fallback_session: &fallback_session,
            line: line_no,
            byte_offset: base_offset + consumed as u64,
        };
        consumed += line.len();
        match decode_line(line) {
            Line::Record(record) => handle_record(&record, &at, &mut events, &mut stats),
            Line::Blank => {}
            Line::Malformed => stats.malformed_lines += 1,
        }
    }
    (events, stats, consumed as u64)
}

enum Line {
    Blank,
    Malformed,
    Record(Value),
}

fn decode_line(raw: &[u8]) -> Line {
    let Ok(text) = std::str::from_utf8(raw) else {
        return Line::Malformed;
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    match serde_json::from_str::<Value>(text) {
        Ok(v) if v.is_object() => Line::Record(v),
        _ => Line::Malformed,
    }
}

struct Position<'a> {
    path: &'a Path,
    fallback_session: &'a str,
    line: u64,
    byte_offset: u64,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

fn handle_record(rec: &Value, at: &Position<'_>, events: &mut Vec<Event>, stats: &mut ParseStats) {
    let rtype = str_of(rec, "type").unwrap_or("(none)");
    let is_summary = rtype == "summary" || rec.get("isCompactSummary") == Some(&Value::Bool(true));
    let is_conversation = matches!(rtype, "user" | "assistant");
    if !is_summary && !is_conversation {
        bump(&mut stats.skipped_record_types, rtype);
        return;
    }
    if !is_summary && rec.get("isMeta") == Some(&Value::Bool(true)) {
        bump(&mut stats.skipped_record_types, "user(meta)");
        return;
    }
    let owned = |k: &str| str_of(rec, k).map(str::to_owned);
    let template = Event {
        harness: HARNESS_NAME,
        session_id: owned("sessionId").unwrap_or_else(|| at.fallback_session.to_owned()),
        message_uuid: owned("uuid"),
        parent_uuid: owned("parentUuid"),
        source_path: at.path.to_path_buf(),
        line: at.line,
        byte_offset: at.byte_offset,
        timestamp_ms: str_of(rec, "timestamp").and_then(parse_rfc3339_ms),
        cwd: owned("cwd"),
        git_branch: owned("gitBranch"),
        sidechain: rec.get("isSidechain") == Some(&Value::Bool(true)),
        kind: EventKind::Summary,
        text: String::new(),
    };
    let mut sink = Sink {
        template,
        events,
        stats,
        clean: clean_for_kind,
    };
    let content = rec.get("message").and_then(|m| m.get("content"));
    if is_summary {
        // Either `{type:"summary", summary:"..."}` or a user record flagged
        // `isCompactSummary` whose message content holds the text.
        let text = str_of(rec, "summary")
            .map(str::to_owned)
            .or_else(|| content.map(|c| joined_text(c, sink.stats)));
        sink.push(EventKind::Summary, text.as_deref().unwrap_or_default());
    } else if let Some(content) = content {
        push_content(&mut sink, content, rtype == "user");
    }
}

fn push_content(sink: &mut Sink<'_>, content: &Value, user: bool) {
    let text_kind = || {
        if user {
            EventKind::UserText
        } else {
            EventKind::AssistantText
        }
    };
    let blocks = match content {
        Value::String(s) => {
            sink.push(text_kind(), s);
            return;
        }
        Value::Array(blocks) => blocks,
        _ => return,
    };
    for block in blocks {
        match str_of(block, "type") {
            Some("text") => sink.push(text_kind(), str_of(block, "text").unwrap_or_default()),
            Some("tool_use") => {
                let input = block
                    .get("input")
                    .map_or_else(|| "{}".to_owned(), Value::to_string);
                let kind = EventKind::ToolCall {
                    id: str_of(block, "id").unwrap_or_default().to_owned(),
                    name: str_of(block, "name").unwrap_or_default().to_owned(),
                };
                sink.push(kind, &input);
            }
            Some("tool_result") => {
                let text = block
                    .get("content")
                    .map(|c| joined_text(c, sink.stats))
                    .unwrap_or_default();
                let kind = EventKind::ToolResult {
                    tool_use_id: str_of(block, "tool_use_id").unwrap_or_default().to_owned(),
                    is_error: block.get("is_error") == Some(&Value::Bool(true)),
                };
                sink.push(kind, &text);
            }
            other => bump(&mut sink.stats.skipped_blocks, other.unwrap_or("(none)")),
        }
    }
}

/// A string, or the text blocks of an array joined by newlines. Other block
/// types (images, ...) are dropped and counted.
fn joined_text(content: &Value, stats: &mut ParseStats) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for b in blocks {
                match (str_of(b, "type"), str_of(b, "text")) {
                    (Some("text"), Some(t)) => parts.push(t),
                    (t, _) => bump(&mut stats.skipped_blocks, t.unwrap_or("(none)")),
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

/// Claude cleans user text and tool results; assistant text is kept as is.
fn clean_for_kind(kind: &EventKind, text: &str) -> String {
    if matches!(kind, EventKind::UserText | EventKind::ToolResult { .. }) {
        clean_injected(text)
    } else {
        text.to_owned()
    }
}

/// Remove harness-injected spans from user-side text: `<system-reminder>` and
/// `<local-command-stdout>` spans (with content), the duplicate
/// `<command-message>` span, and the `<command-name>`/`<command-args>` tags
/// themselves (their inner text is kept).
fn clean_injected(text: &str) -> String {
    // Unwrap command tags FIRST: removing them after the span pass could glue
    // fragments into a new, live tag (`<system-rem<command-name></command-name>inder>`).
    let mut out = text.to_owned();
    for tag in ["command-name", "command-args"] {
        for t in [format!("<{tag}>"), format!("</{tag}>")] {
            out = out.replace(&t, "");
        }
    }
    strip_spans(&out, &SPAN_TAGS)
}

/// Harness-injected spans whose content is not the user's words.
const SPAN_TAGS: [&str; 3] = ["system-reminder", "local-command-stdout", "command-message"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_multiple_multiline_reminders() {
        let t = "a<system-reminder>x\ny</system-reminder> b <system-reminder>z</system-reminder>c";
        assert_eq!(clean_injected(t), "a b c");
    }

    #[test]
    fn nested_reminders_are_removed_whole() {
        let t =
            "hi<system-reminder>a<system-reminder>b</system-reminder>INJECTED</system-reminder>bye";
        assert_eq!(clean_injected(t), "hibye");
    }

    #[test]
    fn stripping_cannot_rebuild_a_live_tag() {
        let t = "a<system-rem<command-name></command-name>inder>hidden</system-reminder>b";
        assert_eq!(clean_injected(t), "ab");
        let stray = "x</system-reminder>y";
        assert_eq!(clean_injected(stray), "xy");
    }

    #[test]
    fn many_openers_stay_linear() {
        // Absolute bound, not a timing ratio (ratios of sub-millisecond runs
        // are noise). Quadratic rescanning of 100k openers takes minutes.
        let big = "<system-reminder>".repeat(100_000) + "</system-reminder>";
        let s = std::time::Instant::now();
        let _ = clean_injected(&big);
        let secs = s.elapsed().as_secs_f64();
        assert!(secs < 2.0, "took {secs:.3}s");
    }

    #[test]
    fn unterminated_reminder_drops_rest() {
        assert_eq!(clean_injected("keep<system-reminder>secret stuff"), "keep");
    }

    #[test]
    fn command_wrappers() {
        let t = "<command-message>m</command-message><command-name>/go</command-name>\n<command-args>now</command-args><local-command-stdout>out</local-command-stdout>";
        assert_eq!(clean_injected(t).trim(), "/go\nnow");
    }

    #[test]
    fn empty_tool_result_is_kept_but_empty_prose_is_not() {
        let line = |content: &str| {
            format!("{{\"type\":\"user\",\"uuid\":\"u\",\"message\":{{\"content\":{content}}}}}\n")
        };
        let body = [
            line(r#"[{"type":"tool_result","tool_use_id":"t1","content":"","is_error":false}]"#),
            line(r#"[{"type":"tool_result","tool_use_id":"t2","content":[]}]"#),
            line(r#"[{"type":"tool_result","tool_use_id":"t3"}]"#),
            line(r#"[{"type":"text","text":"  "}]"#),
            line(r#""""#),
        ]
        .concat();
        let (events, stats, _) = parse_bytes(body.as_bytes(), 0, 0, Path::new("/s.jsonl"));
        let got: Vec<_> = events
            .iter()
            .map(|e| (e.kind.clone(), e.text.as_str()))
            .collect();
        let result = |id: &str| EventKind::ToolResult {
            tool_use_id: id.into(),
            is_error: false,
        };
        assert_eq!(
            got,
            [(result("t1"), ""), (result("t2"), ""), (result("t3"), "")]
        );
        assert_eq!(stats.events, 3);
    }

    #[test]
    fn root_resolution() {
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(root_from_env(os("/c"), os("/h")), Some(PathBuf::from("/c")));
        assert_eq!(
            root_from_env(None, os("/h")),
            Some(PathBuf::from("/h/.claude"))
        );
        assert_eq!(
            root_from_env(os(""), os("/h")),
            Some(PathBuf::from("/h/.claude"))
        );
        assert_eq!(root_from_env(None, None), None);
    }
}
