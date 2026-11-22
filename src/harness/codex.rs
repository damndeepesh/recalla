//! Codex CLI adapter: `<codex home>/sessions/YYYY/MM/DD/rollout-*.jsonl`.
//!
//! Lines are `{type, timestamp, payload}`. Parsing is lenient (D-012): unknown
//! shapes are counted in [`ParseStats`], never fatal. Conversation comes from
//! `response_item` messages; commands and file edits from `event_msg` /
//! `item_completed` items (which carry the exit code). The `UserMessage` /
//! `AgentMessage` items repeat the messages and are ignored.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{
    Event, EventKind, Harness, ParseStats, SessionFile, Sink, bump, count_newlines, is_jsonl,
    parse_rfc3339_ms, real_entries, strip_spans,
};

const HARNESS_NAME: &str = "codex";

/// The [`Harness`] for Codex CLI.
#[derive(Debug, Clone, Copy, Default)]
pub struct Codex;

impl Harness for Codex {
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

/// Codex home from the environment: `CODEX_HOME`, else `$HOME/.codex`.
#[must_use]
pub fn default_root() -> Option<PathBuf> {
    root_from_env(std::env::var_os("CODEX_HOME"), std::env::var_os("HOME"))
}

/// Pure form of [`default_root`]. Empty values count as unset.
#[must_use]
pub fn root_from_env(codex_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let non_empty = |v: Option<OsString>| v.filter(|s| !s.is_empty()).map(PathBuf::from);
    non_empty(codex_home).or_else(|| non_empty(home).map(|h| h.join(".codex")))
}

/// Find every `*.jsonl` under `<root>/sessions`, at any depth, skipping
/// symlinks (files and directories), sorted by path. A missing root gives `[]`.
///
/// # Errors
/// I/O errors other than a missing directory.
pub fn discover(root: &Path) -> io::Result<Vec<SessionFile>> {
    let sessions = root.join("sessions");
    let mut found = Vec::new();
    let mut dirs = vec![sessions.clone()];
    while let Some(dir) = dirs.pop() {
        for (path, is_dir) in real_entries(&dir)? {
            if is_dir {
                dirs.push(path);
            } else if is_jsonl(&path) {
                let project_dir_name = path
                    .parent()
                    .and_then(|p| p.strip_prefix(&sessions).ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                found.push(SessionFile {
                    path,
                    project_dir_name,
                    is_subagent: false,
                });
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// Parse complete lines of a session file starting at `from_offset` (a line
/// start); same contract as the Claude adapter's `parse_session`.
///
/// # Errors
/// I/O errors, or `from_offset` beyond the end of the file.
pub fn parse_session(path: &Path, from_offset: u64) -> io::Result<(Vec<Event>, ParseStats, u64)> {
    let mut file = File::open(path)?;
    if from_offset > file.metadata()?.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resume offset is past the end of the file",
        ));
    }
    let base_line = count_newlines(&mut file, from_offset)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let (events, stats, next_rel) = parse_bytes(&bytes, from_offset, base_line, path);
    Ok((events, stats, from_offset + next_rel))
}

/// Parse the complete lines of `bytes`, which start at file offset
/// `base_offset` (a line start) after `base_line` lines. A final chunk without
/// `\n` is left unconsumed (`partial_tail`).
///
/// The session id and cwd live in earlier records (`session_meta`,
/// `turn_context`), so when `base_offset > 0` the prefix of `path` is scanned
/// for them; resuming yields the same events as a full parse. An unreadable
/// prefix falls back to the file stem and no cwd.
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
    let mut meta = State {
        session: None,
        cwd: None,
    };
    if base_offset > 0 {
        // Best effort: a missing/short file just leaves the meta empty.
        let _ = seed_state(path, base_offset, &mut meta);
    }
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
            Line::Record(record) => {
                handle_record(&record, &at, &mut meta, &mut events, &mut stats);
            }
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

/// What earlier records established: the session id and the running cwd.
struct State {
    session: Option<String>,
    cwd: Option<String>,
}

impl State {
    /// Applies a `session_meta` or `turn_context` record; others are ignored.
    fn update(&mut self, rec: &Value) {
        let Some(payload) = rec.get("payload") else {
            return;
        };
        let own = |k: &str| str_of(payload, k).map(str::to_owned);
        match str_of(rec, "type") {
            Some("session_meta") => {
                self.session = own("id")
                    .or_else(|| own("session_id"))
                    .or(self.session.take());
                self.cwd = own("cwd").or(self.cwd.take());
            }
            Some("turn_context") => self.cwd = own("cwd").or(self.cwd.take()),
            _ => {}
        }
    }
}

/// Replays the metadata records in `path[..upto]` into `meta`. Only lines
/// mentioning a metadata type are decoded.
fn seed_state(path: &Path, upto: u64, meta: &mut State) -> io::Result<()> {
    let mut reader = BufReader::new(File::open(path)?.take(upto));
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let has = |needle: &[u8]| line.windows(needle.len()).any(|w| w == needle);
        if has(b"session_meta") || has(b"turn_context") {
            if let Line::Record(rec) = decode_line(&line) {
                meta.update(&rec);
            }
        }
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

fn handle_record(
    rec: &Value,
    at: &Position<'_>,
    meta: &mut State,
    events: &mut Vec<Event>,
    stats: &mut ParseStats,
) {
    let rtype = str_of(rec, "type").unwrap_or("(none)");
    let Some(payload) = rec.get("payload").filter(|p| p.is_object()) else {
        bump(&mut stats.skipped_record_types, rtype);
        return;
    };
    meta.update(rec);
    let ptype = str_of(payload, "type").unwrap_or("(none)");
    let id = str_of(payload, "id").map(str::to_owned);
    let template = Event {
        harness: HARNESS_NAME,
        session_id: meta
            .session
            .clone()
            .unwrap_or_else(|| at.fallback_session.to_owned()),
        message_uuid: id,
        parent_uuid: None,
        source_path: at.path.to_path_buf(),
        line: at.line,
        byte_offset: at.byte_offset,
        timestamp_ms: str_of(rec, "timestamp").and_then(parse_rfc3339_ms),
        cwd: meta.cwd.clone(),
        git_branch: None,
        sidechain: false,
        kind: EventKind::Summary,
        text: String::new(),
    };
    let mut sink = Sink {
        template,
        events,
        stats,
        clean: clean_for_kind,
    };
    match (rtype, ptype) {
        ("response_item", "message") => push_message(&mut sink, payload),
        ("event_msg", "item_completed") => push_item(&mut sink, payload),
        ("compacted", _) => match str_of(payload, "message") {
            Some(text) => sink.push(EventKind::Summary, text),
            None => bump(&mut sink.stats.skipped_record_types, rtype),
        },
        ("response_item" | "event_msg", other) => {
            bump(&mut sink.stats.skipped_record_types, other);
        }
        _ => bump(&mut sink.stats.skipped_record_types, rtype),
    }
}

fn push_message(sink: &mut Sink<'_>, payload: &Value) {
    let (kind, part) = match str_of(payload, "role") {
        Some("user") => (EventKind::UserText, "input_text"),
        Some("assistant") => (EventKind::AssistantText, "output_text"),
        other => {
            let key = format!("message({})", other.unwrap_or("(none)"));
            bump(&mut sink.stats.skipped_record_types, &key);
            return;
        }
    };
    let Some(Value::Array(parts)) = payload.get("content") else {
        return;
    };
    let mut texts = Vec::new();
    for p in parts {
        match (str_of(p, "type"), str_of(p, "text")) {
            (Some(t), Some(text)) if t == part => texts.push(text),
            (t, _) => bump(&mut sink.stats.skipped_blocks, t.unwrap_or("(none)")),
        }
    }
    sink.push(kind, &texts.join("\n"));
}

fn push_item(sink: &mut Sink<'_>, payload: &Value) {
    let Some(item) = payload.get("item").filter(|i| i.is_object()) else {
        bump(&mut sink.stats.skipped_record_types, "item_completed");
        return;
    };
    let id = str_of(item, "id").unwrap_or_default();
    sink.template.message_uuid = Some(id.to_owned()).filter(|s| !s.is_empty());
    match str_of(item, "type") {
        Some("CommandExecution") => {
            let command = item.get("command").and_then(shell_string);
            let input = json!({ "command": command.unwrap_or_default() }).to_string();
            if let Some(cwd) = str_of(item, "cwd") {
                sink.template.cwd = Some(cwd.to_owned());
            }
            sink.push(
                EventKind::ToolCall {
                    id: id.to_owned(),
                    name: "Bash".to_owned(),
                },
                &input,
            );
            let failed = item.get("exit_code").and_then(Value::as_i64) != Some(0)
                || str_of(item, "status") == Some("failed");
            sink.push(
                EventKind::ToolResult {
                    tool_use_id: id.to_owned(),
                    is_error: failed,
                },
                str_of(item, "aggregated_output").unwrap_or_default(),
            );
        }
        Some("FileChange") => {
            let mut paths: Vec<&String> = item
                .get("changes")
                .and_then(Value::as_object)
                .map(|c| c.keys().collect())
                .unwrap_or_default();
            paths.sort();
            for path in paths {
                let input = json!({ "file_path": path }).to_string();
                let kind = EventKind::ToolCall {
                    id: id.to_owned(),
                    name: "Edit".to_owned(),
                };
                sink.push(kind, &input);
            }
        }
        other => {
            let key = format!("item_completed/{}", other.unwrap_or("(none)"));
            bump(&mut sink.stats.skipped_record_types, &key);
        }
    }
}

/// The shell text of a `command` value: a plain string as is; an argv list
/// `[sh|bash|zsh|fish, -c|-lc, CMD, ..]` gives `CMD`; any other argv is
/// shell-joined with simple quoting. `None` when it is neither.
#[must_use]
pub fn shell_string(command: &Value) -> Option<String> {
    if let Some(s) = command.as_str() {
        return Some(s.to_owned());
    }
    let argv: Vec<&str> = command
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<_>>()?;
    let shell = argv.first().and_then(|a| Path::new(a).file_name());
    if let [_, flag, script, ..] = argv[..]
        && matches!(flag, "-c" | "-lc")
        && shell.is_some_and(|n| ["sh", "bash", "zsh", "fish"].iter().any(|s| n == *s))
    {
        return Some(script.to_owned());
    }
    Some(argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "))
}

fn quote(arg: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !arg.is_empty() && arg.chars().all(plain) {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// Only user text carries injected context; tool output is kept as is.
fn clean_for_kind(kind: &EventKind, text: &str) -> String {
    if matches!(kind, EventKind::UserText) {
        clean_injected(text)
    } else {
        text.to_owned()
    }
}

/// Harness-injected spans whose content is not the user's words.
const SPAN_TAGS: [&str; 3] = ["environment_context", "user_instructions", "INSTRUCTIONS"];

/// Header Codex puts before injected project instructions.
const AGENTS_HEADER: &str = "# AGENTS.md instructions for";

/// Remove injected spans from user text. A message that is just the injected
/// AGENTS.md block (header + `<INSTRUCTIONS>`) leaves nothing.
fn clean_injected(text: &str) -> String {
    let out = strip_spans(text, &SPAN_TAGS);
    if out.trim_start().starts_with(AGENTS_HEADER) {
        String::new()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_resolution() {
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(root_from_env(os("/c"), os("/h")), Some(PathBuf::from("/c")));
        assert_eq!(
            root_from_env(None, os("/h")),
            Some(PathBuf::from("/h/.codex"))
        );
        assert_eq!(
            root_from_env(os(""), os("/h")),
            Some(PathBuf::from("/h/.codex"))
        );
        assert_eq!(root_from_env(None, None), None);
    }

    #[test]
    fn strips_injected_context() {
        let t = "<environment_context>\n<cwd>/x</cwd>\n</environment_context>\nfix the bug";
        assert_eq!(clean_injected(t).trim(), "fix the bug");
        let agents = "# AGENTS.md instructions for /x\n\n<INSTRUCTIONS>\nbe nice\n</INSTRUCTIONS>";
        assert_eq!(clean_injected(agents), "");
        assert_eq!(
            clean_injected("<user_instructions>a</user_instructions>go").trim(),
            "go"
        );
    }
}
