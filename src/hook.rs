//! Claude Code hook entry points (`recalla hook session-start|capture`).
//!
//! A hook must never disturb the session: every failure, even a panic, ends
//! in a single log line under `<data>/log/hook.log` and the process exits 0.
//! `session-start` prints one JSON object (or nothing); `capture` prints
//! nothing, ever. See ADR D-018.
//!
//! Privacy: log lines carry the event, an error kind and a short message
//! (paths and SQLite messages), never transcript or memory content.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use thiserror::Error;

use crate::anchor;
use crate::extract::{self, scope::project_key};
use crate::ingest::{self, Options};
use crate::mirror;
use crate::ops::{self, PackArgs};
use crate::paths;
use crate::store::{Store, StoreError, Via};

/// Longest wait on a locked database before a hook gives up.
pub const BUSY: Duration = Duration::from_secs(2);
/// Largest stdin a hook reads.
const MAX_STDIN: u64 = 1 << 20;
/// Time the staleness check may take after a capture.
const CAPTURE_CHECK_BUDGET: Duration = Duration::from_millis(200);
const DEFAULT_BUDGET: usize = 1500;
const BUDGET_RANGE: std::ops::RangeInclusive<usize> = 100..=8000;
/// The log is cut to its last `LOG_KEEP` bytes once it passes `LOG_MAX`.
const LOG_MAX: u64 = 1 << 20;
const LOG_KEEP: u64 = 512 << 10;
const LOG_DIR: &str = "log";
/// File name of the hook log inside `<data>/log`.
pub const LOG_FILE: &str = "hook.log";

/// Which hook is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    SessionStart,
    /// Used for both `Stop` and `PreCompact`.
    Capture,
}

impl Event {
    const fn name(self) -> &'static str {
        match self {
            Self::SessionStart => "session-start",
            Self::Capture => "capture",
        }
    }
}

/// Where a hook reads and writes.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub data_dir: PathBuf,
    /// Claude config dir; `capture` only reads transcripts under its `projects/`.
    pub root: Option<PathBuf>,
}

#[derive(Debug, Error)]
enum HookError {
    #[error("{0}")]
    Input(&'static str),
    #[error("{0}")]
    Refused(&'static str),
    #[error("sqlite busy for over {}s", BUSY.as_secs())]
    Busy,
    #[error("{0}")]
    Store(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Mirror(String),
    #[error("panic")]
    Panic,
}

impl HookError {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Input(_) => "input",
            Self::Refused(_) => "refused",
            Self::Busy => "busy",
            Self::Store(_) => "store",
            Self::Io(_) => "io",
            Self::Mirror(_) => "mirror",
            Self::Panic => "panic",
        }
    }

    /// Classifies a SQLite failure message, mapping lock timeouts to `Busy`.
    fn store(msg: &str) -> Self {
        if msg.contains("locked") || msg.contains("busy") {
            Self::Busy
        } else {
            Self::Store(msg.to_owned())
        }
    }
}

impl From<StoreError> for HookError {
    fn from(e: StoreError) -> Self {
        Self::store(&e.to_string())
    }
}

impl From<ops::OpenError> for HookError {
    fn from(e: ops::OpenError) -> Self {
        Self::store(&e.to_string())
    }
}

/// Silences the default panic message so nothing reaches stderr.
pub fn silence_panics() {
    std::panic::set_hook(Box::new(|_| {}));
}

/// Runs one hook. Never fails and never panics: problems are logged.
/// `out` receives the `session-start` JSON; `capture` never writes to it.
pub fn run(event: Event, input: impl Read, out: &mut impl Write, ctx: &Ctx) {
    let result = catch_unwind(AssertUnwindSafe(|| handle(event, input, out, ctx)));
    let err = match result {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e,
        Err(_) => HookError::Panic,
    };
    log_line(&ctx.data_dir, event, &err);
}

fn handle(
    event: Event,
    input: impl Read,
    out: &mut impl Write,
    ctx: &Ctx,
) -> Result<(), HookError> {
    let payload = read_payload(input)?;
    match event {
        Event::SessionStart => session_start(&payload, out, &ctx.data_dir),
        Event::Capture => capture(&payload, ctx),
    }
}

/// The stdin JSON object. Parse errors are classified, never echoed: serde's
/// messages can quote the offending value.
fn read_payload(input: impl Read) -> Result<Value, HookError> {
    let mut buf = Vec::new();
    input
        .take(MAX_STDIN)
        .read_to_end(&mut buf)
        .map_err(|e| HookError::Io(format!("reading stdin: {:?}", e.kind())))?;
    let value: Value = serde_json::from_slice(&buf).map_err(|e| {
        HookError::Input(match e.classify() {
            serde_json::error::Category::Io => "stdin unreadable",
            serde_json::error::Category::Syntax => "stdin is not valid JSON",
            serde_json::error::Category::Data => "stdin JSON has the wrong shape",
            serde_json::error::Category::Eof => "stdin JSON is truncated or empty",
        })
    })?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(HookError::Input("stdin JSON is not an object"))
    }
}

fn str_field<'a>(payload: &'a Value, key: &'static str) -> Result<&'a str, HookError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(HookError::Input(match key {
            "cwd" => "missing cwd",
            _ => "missing transcript_path",
        }))
}

/// `RECALLA_BUDGET` when it is an integer in 100..=8000, else 1500.
#[must_use]
pub fn budget_from(var: Option<&str>) -> usize {
    var.and_then(|v| v.trim().parse().ok())
        .filter(|b| BUDGET_RANGE.contains(b))
        .unwrap_or(DEFAULT_BUDGET)
}

/// Longest `session_id` stored with an injection, in chars.
const MAX_SESSION_ID: usize = 200;

fn session_start(payload: &Value, out: &mut impl Write, data_dir: &Path) -> Result<(), HookError> {
    let project = project_key(str_field(payload, "cwd")?);
    let db = paths::db_path(data_dir);
    if !db.is_file() {
        return Ok(());
    }
    let mut store = Store::open_with_busy(&db, BUSY)?;
    let args = PackArgs {
        budget: budget_from(std::env::var("RECALLA_BUDGET").ok().as_deref()),
        project: Some(project),
        query: None,
        no_log: false,
        via: Via::Hook,
        session_id: payload
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(MAX_SESSION_ID).collect()),
    };
    let pack = ops::pack_with(&mut store, &args).map_err(|e| HookError::store(&e))?;
    if pack.markdown.is_empty() {
        return Ok(());
    }
    let reply = json!({"hookSpecificOutput": {
        "hookEventName": "SessionStart",
        "additionalContext": pack.markdown,
    }});
    writeln!(out, "{reply}").map_err(|e| HookError::Io(format!("writing stdout: {:?}", e.kind())))
}

/// The canonical path of a transcript, if it is a `.jsonl` file under
/// `<root>/projects` after resolving symlinks on both sides.
fn trusted_transcript(root: &Path, raw: &str) -> Result<PathBuf, HookError> {
    let io_err = |what: &str, e: io::Error| HookError::Io(format!("{what}: {:?}", e.kind()));
    let root = fs::canonicalize(root).map_err(|e| io_err("claude root", e))?;
    let path = fs::canonicalize(raw).map_err(|e| io_err("transcript_path", e))?;
    if !path.starts_with(root.join("projects")) {
        return Err(HookError::Refused(
            "transcript_path is outside the Claude root",
        ));
    }
    if !path.is_file() || path.extension().is_none_or(|e| e != "jsonl") {
        return Err(HookError::Refused("transcript_path is not a .jsonl file"));
    }
    Ok(path)
}

fn capture(payload: &Value, ctx: &Ctx) -> Result<(), HookError> {
    let raw = str_field(payload, "transcript_path")?;
    let root = ctx.root.as_deref().ok_or(HookError::Input(
        "no Claude root (set CLAUDE_CONFIG_DIR or HOME)",
    ))?;
    let file = trusted_transcript(root, raw)?;
    let mut store = ops::open_or_create_with(&ctx.data_dir, BUSY)?;
    let opts = Options {
        claude_root: Some(root.to_owned()),
        codex_root: None,
        data_dir: ctx.data_dir.clone(),
        archive: true,
        dry_run: false,
    };
    let report = ingest::run_files(&mut store, &opts, &[file]);
    if let Some(f) = report.failed.first() {
        return Err(HookError::store(&format!("ingest failed: {}", f.error)));
    }
    extract::run(&mut store, &extract::Options::default())?;
    // Code edited during the session may have outdated memories.
    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
        let project = project_key(cwd);
        if anchor::project_dir(&project).is_some() {
            anchor::check(&mut store, &project, Some(CAPTURE_CHECK_BUDGET))?;
        }
    }
    mirror::sync_best_effort(&mut store, &ctx.data_dir)
        .map_or(Ok(()), |w| Err(HookError::Mirror(w)))
}

/// Appends one line to the hook log, trimming it first when it is too big.
/// Logging is best effort: a hook has nowhere else to report a failure.
fn log_line(data_dir: &Path, event: Event, err: &HookError) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let msg: String = err
        .to_string()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = format!("{ts} {} {}: {msg}\n", event.name(), err.kind());
    let dir = data_dir.join(LOG_DIR);
    let write = || -> io::Result<()> {
        paths::ensure_dir(&dir)?;
        let path = dir.join(LOG_FILE);
        paths::ensure_private_file(&path)?;
        trim_log(&path)?;
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())
    };
    let _ = write();
}

/// Cuts `path` to its last `LOG_KEEP` bytes (from a line start) once it
/// exceeds `LOG_MAX`.
fn trim_log(path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len <= LOG_MAX {
        return Ok(());
    }
    file.seek(SeekFrom::Start(len - LOG_KEEP))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let start = tail.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
    fs::write(path, &tail[start..])
}

/// The last `n` lines of the hook log (empty when there is none).
#[must_use]
pub fn log_tail(data_dir: &Path, n: usize) -> Vec<String> {
    let text = fs::read_to_string(data_dir.join(LOG_DIR).join(LOG_FILE)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|&l| l.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_env_is_validated() {
        assert_eq!(budget_from(None), 1500);
        assert_eq!(budget_from(Some("2000")), 2000);
        assert_eq!(budget_from(Some(" 100 ")), 100);
        assert_eq!(budget_from(Some("8000")), 8000);
        for bad in ["99", "8001", "abc", "", "-5", "1e3"] {
            assert_eq!(budget_from(Some(bad)), 1500, "{bad}");
        }
    }

    #[test]
    fn log_is_trimmed_to_whole_recent_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.log");
        let line = format!("{}\n", "x".repeat(99));
        let n = usize::try_from(LOG_MAX).unwrap() / line.len() + 10;
        fs::write(&path, line.repeat(n)).unwrap();
        trim_log(&path).unwrap();
        let kept = fs::read_to_string(&path).unwrap();
        assert!(kept.len() as u64 <= LOG_KEEP);
        assert!(kept.len() as u64 > LOG_KEEP - 200);
        assert!(kept.lines().all(|l| l.len() == 99));
        fs::write(&path, "short\n").unwrap();
        trim_log(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "short\n");
    }

    #[test]
    fn log_lines_are_private_single_lines() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        log_line(&data, Event::Capture, &HookError::Io("a\nb".into()));
        log_line(&data, Event::SessionStart, &HookError::Panic);
        let tail = log_tail(&data, 5);
        assert_eq!(tail.len(), 2);
        assert!(tail[0].ends_with("capture io: a b"), "{}", tail[0]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(data.join(LOG_DIR)), 0o700);
            assert_eq!(mode(data.join(LOG_DIR).join(LOG_FILE)), 0o600);
        }
    }

    #[test]
    fn payload_errors_do_not_echo_input() {
        let err = read_payload(&b"{\"secret\": nope"[..])
            .unwrap_err()
            .to_string();
        assert!(!err.contains("secret") && !err.contains("nope"), "{err}");
        assert!(read_payload(&b"[1]"[..]).is_err());
        assert!(read_payload(&b""[..]).is_err());
    }
}
