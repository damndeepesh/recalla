//! opencode adapter. opencode keeps history in one SQLite database, which has
//! no byte offsets to resume from. [`export`] therefore reads it read-only and
//! writes one deterministic, append-only JSONL file per session under the
//! Recalla data dir; [`Opencode`] parses those files, so checkpoints, the raw
//! archive and redaction work exactly as for the other agents (D-032).
//!
//! Export line: `{v, session, cwd, sidechain, ts, msg, kind, id?, name?, error?, text}`
//! with `kind` one of `user`, `assistant`, `tool_call`, `tool_result`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{
    Event, EventKind, Harness, ParseStats, SessionFile, count_newlines, is_jsonl, real_entries,
};
use crate::store::{self, OpencodePart, OpencodeSession};

const HARNESS_NAME: &str = "opencode";
/// Database file inside opencode's data dir.
pub const DB_FILE: &str = "opencode.db";

/// The [`Harness`] for opencode (reads the exported JSONL).
#[derive(Debug, Clone, Copy, Default)]
pub struct Opencode;

impl Harness for Opencode {
    fn name(&self) -> &'static str {
        HARNESS_NAME
    }

    fn discover(&self, root: &Path) -> io::Result<Vec<SessionFile>> {
        let mut found: Vec<SessionFile> = real_entries(root)?
            .into_iter()
            .filter(|(p, is_dir)| !is_dir && is_jsonl(p))
            .map(|(path, _)| SessionFile {
                path,
                project_dir_name: String::new(),
                is_subagent: false,
            })
            .collect();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    fn parse(&self, path: &Path, from_offset: u64) -> io::Result<(Vec<Event>, ParseStats, u64)> {
        let bytes = fs::read(path)?;
        let from = usize::try_from(from_offset)
            .ok()
            .filter(|f| *f <= bytes.len())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "offset beyond end of file")
            })?;
        let base_line = count_newlines(&bytes[..from], from_offset)?;
        let (ev, stats, used) = parse_bytes(&bytes[from..], from_offset, base_line, path);
        Ok((ev, stats, from_offset + used))
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

/// opencode's data dir: `$XDG_DATA_HOME/opencode`, else `~/.local/share/opencode`.
#[must_use]
pub fn default_root() -> Option<PathBuf> {
    root_from_env(std::env::var_os("XDG_DATA_HOME"), crate::paths::home_dir())
}

/// Pure form of [`default_root`]. Empty values count as unset.
#[must_use]
pub fn root_from_env(xdg_data: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let non_empty = |v: Option<OsString>| v.filter(|s| !s.is_empty()).map(PathBuf::from);
    non_empty(xdg_data)
        .map(|d| d.join("opencode"))
        .or_else(|| non_empty(home).map(|h| h.join(".local").join("share").join("opencode")))
}

/// Where the export for a data dir lives.
#[must_use]
pub fn export_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("import").join(HARNESS_NAME)
}

/// Maps opencode's lowercase tools and camelCase inputs to the Claude-style
/// names the extractor keys on. Unknown tools keep their input, capitalized.
fn normalize_tool(tool: &str, input: &Value) -> (String, Value) {
    let pick = |from: &[(&str, &str)]| -> Value {
        let mut out = Map::new();
        for (src, dst) in from {
            if let Some(v) = input.get(*src) {
                out.insert((*dst).to_owned(), v.clone());
            }
        }
        Value::Object(out)
    };
    match tool {
        "bash" => ("Bash".into(), pick(&[("command", "command")])),
        "edit" => (
            "Edit".into(),
            pick(&[
                ("filePath", "file_path"),
                ("oldString", "old_string"),
                ("newString", "new_string"),
            ]),
        ),
        "write" => (
            "Write".into(),
            pick(&[("filePath", "file_path"), ("content", "content")]),
        ),
        "read" => ("Read".into(), pick(&[("filePath", "file_path")])),
        other => {
            let mut name = other.to_owned();
            if let Some(first) = name.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            (name, input.clone())
        }
    }
}

/// Export lines for one part (zero, one or two: a tool call and its result).
fn part_lines(s: &OpencodeSession, p: &OpencodePart) -> Vec<Value> {
    let Ok(d) = serde_json::from_str::<Value>(&p.data) else {
        return Vec::new();
    };
    let base = |kind: &str| {
        json!({"v": 1, "session": s.id, "cwd": s.directory, "sidechain": s.sidechain,
               "ts": p.time_ms, "msg": p.message_id, "kind": kind})
    };
    let with = |mut v: Value, extra: Value| {
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        v
    };
    match d.get("type").and_then(Value::as_str) {
        Some("text") => {
            let text = d.get("text").and_then(Value::as_str).unwrap_or_default();
            let kind = match p.role.as_str() {
                "user" => "user",
                "assistant" => "assistant",
                _ => return Vec::new(),
            };
            if text.trim().is_empty() {
                return Vec::new();
            }
            vec![with(base(kind), json!({ "text": text }))]
        }
        Some("tool") => {
            let (Some(tool), Some(id)) = (
                d.get("tool").and_then(Value::as_str),
                d.get("callID").and_then(Value::as_str),
            ) else {
                return Vec::new();
            };
            let state = d.get("state").cloned().unwrap_or(Value::Null);
            let input = state.get("input").cloned().unwrap_or(Value::Null);
            let (name, input) = normalize_tool(tool, &input);
            let mut out = vec![with(
                base("tool_call"),
                json!({"id": id, "name": name, "text": input.to_string()}),
            )];
            let status = state.get("status").and_then(Value::as_str);
            let field = |k: &str| state.get(k).and_then(Value::as_str).map(str::to_owned);
            let result = match status {
                Some("completed") => Some((false, field("output"))),
                Some("error") => Some((true, field("error").or_else(|| field("output")))),
                _ => None, // pending / running: no result yet
            };
            if let Some((error, text)) = result {
                out.push(with(
                    base("tool_result"),
                    json!({"id": id, "error": error, "text": text.unwrap_or_default()}),
                ));
            }
            out
        }
        _ => Vec::new(),
    }
}

fn session_body(s: &OpencodeSession) -> String {
    let mut out = String::new();
    for line in s.parts.iter().flat_map(|p| part_lines(s, p)) {
        out.push_str(&line.to_string());
        out.push('\n');
    }
    out
}

/// Counts from [`export`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportStats {
    pub sessions: usize,
    pub written: usize,
}

/// Writes one JSONL file per session of the opencode database `db` into `out`
/// (0700 dir, 0600 files). A file whose old content is a prefix of the new is
/// appended to, so ingest resumes it; unchanged files are not touched.
///
/// # Errors
/// An unreadable or non-opencode database, or I/O errors writing the export.
pub fn export(db: &Path, out: &Path) -> io::Result<ExportStats> {
    let sessions = store::read_opencode(db)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    crate::paths::ensure_dir(out)?;
    let mut stats = ExportStats::default();
    for s in &sessions {
        let body = session_body(s);
        if body.is_empty() {
            continue;
        }
        stats.sessions += 1;
        let file = out.join(format!("{}.jsonl", safe_name(&s.id)));
        let old = fs::read_to_string(&file).unwrap_or_default();
        if old == body {
            continue;
        }
        if body.starts_with(&old) && !old.is_empty() {
            use std::io::Write as _;
            fs::OpenOptions::new()
                .append(true)
                .open(&file)?
                .write_all(&body.as_bytes()[old.len()..])?;
        } else {
            fs::write(&file, &body)?;
        }
        crate::paths::restrict_file(&file)?;
        stats.written += 1;
    }
    Ok(stats)
}

/// A session id as a file stem: ids are `ses_...` alphanumerics, but never
/// trust a foreign database with a path.
fn safe_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn parse_bytes(
    bytes: &[u8],
    base_offset: u64,
    base_line: u64,
    path: &Path,
) -> (Vec<Event>, ParseStats, u64) {
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
        let offset = base_offset + consumed as u64;
        consumed += line.len();
        let Ok(text) = std::str::from_utf8(line) else {
            stats.malformed_lines += 1;
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(text.trim()) {
            Ok(v) if v.is_object() => match event_from(&v, path, line_no, offset) {
                Some(e) => events.push(e),
                None => {
                    *stats
                        .skipped_record_types
                        .entry("unknown".into())
                        .or_default() += 1;
                }
            },
            _ => stats.malformed_lines += 1,
        }
    }
    stats.events = events.len() as u64;
    (events, stats, consumed as u64)
}

fn event_from(v: &Value, path: &Path, line: u64, byte_offset: u64) -> Option<Event> {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let text = s("text").unwrap_or_default().to_owned();
    let kind = match s("kind")? {
        "user" => EventKind::UserText,
        "assistant" => EventKind::AssistantText,
        "tool_call" => EventKind::ToolCall {
            id: s("id")?.to_owned(),
            name: s("name")?.to_owned(),
        },
        "tool_result" => EventKind::ToolResult {
            tool_use_id: s("id")?.to_owned(),
            is_error: v.get("error").and_then(Value::as_bool).unwrap_or(false),
        },
        _ => return None,
    };
    Some(Event {
        harness: HARNESS_NAME,
        session_id: s("session")?.to_owned(),
        message_uuid: s("msg").map(str::to_owned),
        parent_uuid: None,
        source_path: path.to_path_buf(),
        line,
        byte_offset,
        timestamp_ms: v.get("ts").and_then(Value::as_i64),
        cwd: s("cwd").filter(|c| !c.is_empty()).map(str::to_owned),
        git_branch: None,
        sidechain: v.get("sidechain").and_then(Value::as_bool).unwrap_or(false),
        kind,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests_support::opencode_fixture;

    #[test]
    fn export_then_parse_round_trips_the_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(DB_FILE);
        opencode_fixture(&db);
        let out = dir.path().join("out");
        let st = export(&db, &out).unwrap();
        assert_eq!(
            st,
            ExportStats {
                sessions: 1,
                written: 1
            }
        );
        let files = Opencode.discover(&out).unwrap();
        assert_eq!(files.len(), 1);
        let (ev, stats, used) = Opencode.parse(&files[0].path, 0).unwrap();
        assert_eq!(used, fs::metadata(&files[0].path).unwrap().len());
        assert_eq!(stats.malformed_lines, 0);
        let kinds: Vec<_> = ev.iter().map(|e| &e.kind).collect();
        assert!(matches!(kinds[0], EventKind::UserText));
        assert_eq!(ev[0].text, "use pnpm, never npm");
        assert_eq!(ev[0].cwd.as_deref(), Some("/work/app"));
        let bash = ev
            .iter()
            .find(|e| matches!(&e.kind, EventKind::ToolCall { name, .. } if name == "Bash"))
            .unwrap();
        assert!(bash.text.contains("pnpm test"));
        assert!(ev.iter().any(
            |e| matches!(&e.kind, EventKind::ToolResult { is_error: true, .. })
                && e.text.contains("not allowed")
        ));
        let edit = ev
            .iter()
            .find(|e| matches!(&e.kind, EventKind::ToolCall { name, .. } if name == "Edit"))
            .unwrap();
        assert!(
            edit.text.contains("\"file_path\":\"/work/app/a.ts\"")
                && edit.text.contains("new_string")
        );
        assert!(!ev.iter().any(|e| e.text == "hmm"), "reasoning is dropped");
    }

    #[test]
    fn export_is_idempotent_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(DB_FILE);
        opencode_fixture(&db);
        let out = dir.path().join("out");
        export(&db, &out).unwrap();
        assert_eq!(export(&db, &out).unwrap().written, 0, "unchanged: no write");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute(
            "INSERT INTO part VALUES ('prt_9','msg_2','ses_1',99,'{\"type\":\"text\",\"text\":\"later\"}')",
            [],
        )
        .unwrap();
        let file = out.join("ses_1.jsonl");
        let before = fs::read_to_string(&file).unwrap();
        assert_eq!(export(&db, &out).unwrap().written, 1);
        let after = fs::read_to_string(&file).unwrap();
        assert!(after.starts_with(&before) && after.contains("later"));
    }

    #[test]
    fn unfinished_tool_has_a_call_but_no_result() {
        let s = OpencodeSession {
            id: "s".into(),
            directory: "/w".into(),
            sidechain: false,
            parts: vec![],
        };
        let p = OpencodePart {
            message_id: "m".into(), role: "assistant".into(), time_ms: 1,
            data: r#"{"type":"tool","tool":"grep","callID":"c","state":{"status":"running","input":{"pattern":"x"}}}"#.into(),
        };
        let lines = part_lines(&s, &p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["name"], "Grep");
    }

    #[test]
    fn hostile_session_ids_cannot_escape_the_export_dir() {
        assert_eq!(safe_name("../../etc/x"), "______etc_x");
    }

    #[test]
    fn root_prefers_xdg_then_home() {
        let r =
            |x: Option<&str>, h: Option<&str>| root_from_env(x.map(Into::into), h.map(Into::into));
        assert_eq!(
            r(Some("/x"), Some("/h")),
            Some(PathBuf::from("/x/opencode"))
        );
        assert_eq!(
            r(Some(""), Some("/h")),
            Some(PathBuf::from("/h/.local/share/opencode"))
        );
        assert_eq!(r(None, None), None);
    }

    #[test]
    fn garbage_lines_are_counted_not_fatal() {
        let (ev, st, used) = parse_bytes(
            b"not json\n{\"kind\":\"user\",\"session\":\"s\",\"text\":\"hi\"}\n{\"x\":1}\npartial",
            0,
            0,
            Path::new("s.jsonl"),
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(st.malformed_lines, 1);
        assert!(st.partial_tail);
        assert_eq!(
            usize::try_from(used).unwrap(),
            "not json\n{\"kind\":\"user\",\"session\":\"s\",\"text\":\"hi\"}\n{\"x\":1}\n".len()
        );
    }
}
