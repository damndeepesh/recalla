//! Claude Code adapter: `<config>/projects/<cwd>/<session>.jsonl` (+ subagents).
//!
//! Parsing is lenient (`serde_json::Value`, D-012): unknown or missing fields
//! are counted in [`ParseStats`], never fatal. Only user/assistant/summary
//! records carry conversation; everything else is telemetry and is dropped.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Event, EventKind, Harness, ParseStats, SessionFile, parse_rfc3339_ms};

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
}

/// Config dir from the environment: `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
#[must_use]
pub fn default_root() -> Option<PathBuf> {
    root_from_env(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("HOME"),
    )
}

/// Pure form of [`default_root`]. Empty values count as unset.
#[must_use]
pub fn root_from_env(config_dir: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let non_empty = |v: Option<OsString>| v.filter(|s| !s.is_empty()).map(PathBuf::from);
    non_empty(config_dir).or_else(|| non_empty(home).map(|h| h.join(".claude")))
}

/// Entries of `dir` that are not symlinks, with whether each is a directory.
/// A missing directory is empty, not an error.
fn real_entries(dir: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
    // `read_dir` follows symlinks, so refuse a directory that is itself one
    // (e.g. a symlinked `subagents`); entries are checked below.
    match fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => return Ok(Vec::new()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry?;
        // DirEntry::file_type does not follow symlinks.
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        out.push((entry.path(), ft.is_dir()));
    }
    Ok(out)
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "jsonl")
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

/// Count `\n` in the first `limit` bytes of `file`, leaving it positioned there.
#[allow(clippy::naive_bytecount)] // a dependency isn't worth it here
fn count_newlines(file: &mut File, limit: u64) -> io::Result<u64> {
    let mut reader = BufReader::new(file.take(limit));
    let mut count = 0;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(count);
        }
        count += chunk.iter().filter(|&&b| b == b'\n').count() as u64;
        let n = chunk.len();
        reader.consume(n);
    }
}

/// Parse complete lines of a session file starting at `from_offset` (a line
/// start). Returns events, stats and the offset to resume from. A final chunk
/// without `\n` is left unconsumed (`partial_tail`), so a file still being
/// written is never half-read.
///
/// Memory is bounded by the file size (events are capped per event); callers
/// parse one file at a time.
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
    let mut line_no = count_newlines(&mut file, from_offset)?;
    let mut reader = BufReader::new(file);
    let fallback_session = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let (mut events, mut stats) = (Vec::new(), ParseStats::default());
    let (mut offset, mut buf) = (from_offset, Vec::new());
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        if buf.last() != Some(&b'\n') {
            stats.partial_tail = true;
            break;
        }
        line_no += 1;
        stats.lines += 1;
        let at = Position {
            path,
            fallback_session: &fallback_session,
            line: line_no,
            byte_offset: offset,
        };
        offset += n as u64;
        match decode_line(&buf) {
            Line::Record(record) => handle_record(&record, &at, &mut events, &mut stats),
            Line::Blank => {}
            Line::Malformed => stats.malformed_lines += 1,
        }
    }
    Ok((events, stats, offset))
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

fn bump(map: &mut BTreeMap<String, u64>, key: &str) {
    *map.entry(key.to_owned()).or_default() += 1;
}

/// Collects the events of one record, applying cleaning, caps and stats.
struct Sink<'a> {
    template: Event,
    events: &'a mut Vec<Event>,
    stats: &'a mut ParseStats,
}

impl Sink<'_> {
    fn push(&mut self, kind: EventKind, text: &str) {
        let cleaned;
        let text = if matches!(kind, EventKind::UserText | EventKind::ToolResult { .. }) {
            cleaned = clean_injected(text);
            cleaned.as_str()
        } else {
            text
        };
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        // Not truncated here: ingest redacts first, then truncates, so a cut
        // can never split a secret into fragments the redactor misses.
        self.stats.events += 1;
        self.events.push(Event {
            kind,
            text: text.to_owned(),
            ..self.template.clone()
        });
    }
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
    // Strip spans until nothing changes, then drop any stray tag left over, so
    // stored text never carries a live harness tag. Each pass is linear and
    // only removes text, so the loop ends within a few passes.
    loop {
        let before = out.len();
        for tag in SPAN_TAGS {
            if out.contains(tag) {
                out = remove_spans(&out, tag);
                out = out.replace(&format!("</{tag}>"), "");
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// Harness-injected spans whose content is not the user's words.
const SPAN_TAGS: [&str; 3] = ["system-reminder", "local-command-stdout", "command-message"];

/// Drop every `<tag>…</tag>` span, depth-counted for nesting. Linear: the next
/// opener and closer positions are cached and only re-searched once passed.
/// An unterminated opener drops the rest of the text (fail closed).
fn remove_spans(text: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let next = |from: usize, pat: &str| text[from..].find(pat).map(|i| from + i);
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    let (mut o, mut c) = (next(0, &open), next(0, &close));
    while let Some(start) = o {
        out.push_str(&text[pos..start]);
        pos = start + open.len();
        let mut depth = 1usize;
        o = next(pos, &open);
        while depth > 0 {
            while c.is_some_and(|ci| ci < pos) {
                c = next(pos, &close);
            }
            let Some(ci) = c else {
                return out;
            };
            match o {
                Some(oi) if oi < ci => {
                    depth += 1;
                    pos = oi + open.len();
                    o = next(pos, &open);
                }
                _ => {
                    depth -= 1;
                    pos = ci + close.len();
                    c = next(pos, &close);
                }
            }
        }
        while o.is_some_and(|oi| oi < pos) {
            o = next(pos, &open);
        }
    }
    out.push_str(&text[pos..]);
    out
}

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
        // Quadratic rescanning would take minutes; linear is milliseconds.
        let small = "<system-reminder>".repeat(25_000);
        let big = "<system-reminder>".repeat(100_000) + "</system-reminder>";
        let time = |t: &str| {
            let s = std::time::Instant::now();
            let _ = clean_injected(t);
            s.elapsed().as_secs_f64()
        };
        let (a, b) = (time(&small), time(&big));
        assert!(b < 0.02 || b / a.max(1e-6) < 8.0, "{a:.3}s -> {b:.3}s");
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
