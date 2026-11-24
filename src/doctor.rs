//! `recalla doctor`: one command that answers "is Recalla wired up, and did it fire?".
//!
//! Every check is read-only. Inputs are explicit (paths, `PATH`, the clock) so the
//! checks can be tested without touching the real home directory.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::hook;
use crate::hooks_cfg;
use crate::paths;
use crate::store::{DEFAULT_BUSY, InjectionFilter, Store, StoreError, schema_version};

/// No captured session for this long, with hooks installed, is suspicious.
const STALE_CAPTURE_MS: i64 = 7 * 24 * 3600 * 1000;
const HOOK_LOG_LINES: usize = 3;
const DETAIL_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    /// Works, but something is off or unproven.
    Warn,
    /// Recalla cannot do its job until this is fixed.
    Fail,
    /// Not applicable (tool not installed, no database yet).
    Skip,
}

impl Level {
    const fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub level: Level,
    pub detail: String,
    /// What to run or change, when the level is not `Ok`.
    pub hint: Option<String>,
}

impl Check {
    fn new(name: &'static str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            name,
            level,
            detail: detail.into(),
            hint: None,
        }
    }

    fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Everything `doctor` looks at. Build with [`Inputs::from_env`] or by hand in tests.
#[derive(Debug, Clone)]
pub struct Inputs {
    pub data_dir: PathBuf,
    /// Claude Code `settings.json`.
    pub settings: Option<PathBuf>,
    /// Claude Code's `.claude.json` (user-scope MCP servers).
    pub claude_json: Option<PathBuf>,
    /// Codex `config.toml`.
    pub codex_config: Option<PathBuf>,
    /// The running binary, canonical.
    pub exe: PathBuf,
    pub path_var: Option<OsString>,
    /// Current time, Unix milliseconds.
    pub now_ms: i64,
}

impl Inputs {
    /// The real environment. `settings` overrides the default settings path.
    #[must_use]
    pub fn from_env(data_dir: PathBuf, settings: Option<PathBuf>) -> Self {
        let claude_root = crate::harness::claude::default_root();
        let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty());
        let claude_json = match (&config_dir, std::env::var_os("HOME")) {
            (Some(dir), _) => Some(PathBuf::from(dir).join(".claude.json")),
            (None, Some(home)) => Some(PathBuf::from(home).join(".claude.json")),
            (None, None) => None,
        };
        Self {
            data_dir,
            settings: settings.or_else(|| claude_root.map(|r| r.join("settings.json"))),
            claude_json,
            codex_config: crate::harness::codex::default_root().map(|r| r.join("config.toml")),
            exe: std::env::current_exe()
                .and_then(fs::canonicalize)
                .unwrap_or_default(),
            path_var: std::env::var_os("PATH"),
            now_ms: now_ms(),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Runs every check, in the order they are shown.
#[must_use]
pub fn run(inputs: &Inputs) -> Vec<Check> {
    let db = paths::db_path(&inputs.data_dir);
    let (db_check, store) = check_database(&db);
    let mut checks = vec![
        check_binary(inputs),
        check_hooks(inputs),
        check_mcp_claude(inputs),
        check_mcp_codex(inputs),
        db_check,
        check_data_dir_mode(&inputs.data_dir),
    ];
    checks.push(check_last_capture(inputs, store.as_ref()));
    checks.push(check_last_injection(inputs, store.as_ref()));
    checks.push(check_mirror(store.as_ref()));
    checks.push(check_hook_log(&inputs.data_dir));
    checks
}

/// True when any check is `Fail`.
#[must_use]
pub fn failed(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.level == Level::Fail)
}

#[must_use]
pub fn render_text(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        let _ = writeln!(out, "{:<5} {:<16} {}", c.level.name(), c.name, c.detail);
        if let Some(h) = &c.hint {
            let _ = writeln!(out, "      fix: {h}");
        }
    }
    let (fails, warns) = (
        checks.iter().filter(|c| c.level == Level::Fail).count(),
        checks.iter().filter(|c| c.level == Level::Warn).count(),
    );
    let _ = writeln!(out, "{fails} failed, {warns} warnings");
    out
}

#[must_use]
pub fn render_json(checks: &[Check]) -> Value {
    json!({
        "ok": !failed(checks),
        "checks": checks.iter().map(|c| json!({
            "name": c.name,
            "level": c.level.name(),
            "detail": c.detail,
            "hint": c.hint,
        })).collect::<Vec<_>>(),
    })
}

fn clip(s: &str) -> String {
    let one_line: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    crate::ops::truncate_chars(&one_line, DETAIL_CHARS)
}

fn age(now_ms: i64, then_ms: i64) -> String {
    let secs = (now_ms - then_ms).max(0) / 1000;
    match secs {
        0..=89 => format!("{secs} s ago"),
        90..=5399 => format!("{} min ago", secs / 60),
        5400..=172_799 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

// ------------------------------------------------------------------ checks

fn check_binary(inputs: &Inputs) -> Check {
    let name = "binary";
    let on_path: Vec<PathBuf> = inputs
        .path_var
        .as_ref()
        .map(|p| {
            std::env::split_paths(p)
                .map(|d| d.join("recalla"))
                .filter(|c| c.is_file())
                .collect()
        })
        .unwrap_or_default();
    let Some(first) = on_path.first() else {
        return Check::new(
            name,
            Level::Warn,
            format!(
                "{} is not on PATH (hooks use this absolute path, so they still work)",
                inputs.exe.display()
            ),
        )
        .hint("add its directory to PATH, e.g. `cargo install --path .` puts it in ~/.cargo/bin");
    };
    let same = fs::canonicalize(first).is_ok_and(|c| c == inputs.exe);
    if same {
        Check::new(
            name,
            Level::Ok,
            format!("{} (on PATH)", inputs.exe.display()),
        )
    } else {
        Check::new(
            name,
            Level::Warn,
            format!(
                "PATH resolves recalla to {}, but this is {}",
                first.display(),
                inputs.exe.display()
            ),
        )
        .hint("two different installs: remove one, or reinstall so hooks and CLI agree")
    }
}

fn check_hooks(inputs: &Inputs) -> Check {
    let name = "hooks";
    let Some(settings) = inputs.settings.as_deref() else {
        return Check::new(name, Level::Fail, "no settings path (HOME is unset)");
    };
    match hooks_cfg::installed(settings) {
        Err(e) => Check::new(name, Level::Fail, clip(&e.to_string())),
        Ok(states) => {
            let missing: Vec<&str> = states.iter().filter(|s| !s.1).map(|s| s.0).collect();
            if missing.len() == states.len() {
                Check::new(
                    name,
                    Level::Fail,
                    "no Recalla hooks installed in Claude Code",
                )
                .hint("recalla hooks install")
            } else if missing.is_empty() {
                Check::new(
                    name,
                    Level::Ok,
                    format!("{} events installed", states.len()),
                )
            } else {
                Check::new(
                    name,
                    Level::Warn,
                    format!("missing: {}", missing.join(", ")),
                )
                .hint("recalla hooks install")
            }
        }
    }
}

fn check_mcp_claude(inputs: &Inputs) -> Check {
    let name = "mcp (claude)";
    let Some(path) = inputs.claude_json.as_deref().filter(|p| p.is_file()) else {
        return Check::new(name, Level::Skip, "Claude Code config not found");
    };
    let hint = format!(
        "claude mcp add --scope user recalla -- {} mcp",
        inputs.exe.display()
    );
    let Ok(Ok(v)) = fs::read_to_string(path).map(|t| serde_json::from_str::<Value>(&t)) else {
        return Check::new(name, Level::Warn, "cannot parse Claude Code config").hint(hint);
    };
    let user = v.pointer("/mcpServers/recalla").is_some();
    let project = v
        .get("projects")
        .and_then(Value::as_object)
        .is_some_and(|p| {
            p.values()
                .any(|x| x.pointer("/mcpServers/recalla").is_some())
        });
    match (user, project) {
        (true, _) => Check::new(name, Level::Ok, "registered (user scope)"),
        (false, true) => Check::new(name, Level::Ok, "registered (project scope)"),
        _ => Check::new(name, Level::Warn, "recalla MCP server is not registered").hint(hint),
    }
}

fn check_mcp_codex(inputs: &Inputs) -> Check {
    let name = "mcp (codex)";
    let Some(path) = inputs.codex_config.as_deref().filter(|p| p.is_file()) else {
        return Check::new(name, Level::Skip, "Codex config not found");
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Check::new(name, Level::Warn, "cannot read Codex config");
    };
    let has = |table: &str| text.lines().any(|l| l.trim() == table);
    if !has("[mcp_servers.recalla]") {
        return Check::new(name, Level::Warn, "recalla MCP server is not registered").hint(
            format!(
                "add the [mcp_servers.recalla] snippet to {} (printed by `recalla hooks install`, also in `recalla ui` > Connect)",
                path.display()
            ),
        );
    }
    if !has("[mcp_servers.recalla.tools.recall]") {
        return Check::new(
            name,
            Level::Warn,
            "registered, but `recall` is not auto-approved: non-interactive Codex blocks it",
        )
        .hint("add the [mcp_servers.recalla.tools.recall] and [...tools.why] approval_mode = \"approve\" tables from `recalla ui` > Connect");
    }
    Check::new(name, Level::Ok, "registered, read-only tools auto-approved")
}

/// The database check, plus the open store the later checks reuse.
fn check_database(db: &Path) -> (Check, Option<Store>) {
    let name = "database";
    if !db.is_file() {
        return (
            Check::new(
                name,
                Level::Fail,
                format!("{} does not exist", db.display()),
            )
            .hint("recalla backfill (or finish one Claude Code session with hooks installed)"),
            None,
        );
    }
    match Store::open_read_only(db, DEFAULT_BUSY) {
        Ok(store) => {
            let detail = match store.ui_overview() {
                Ok(o) => format!(
                    "schema v{}, {} memories",
                    schema_version(),
                    o.memories.iter().map(|m| m.2).sum::<u64>()
                ),
                Err(_) => format!("schema v{}", schema_version()),
            };
            (Check::new(name, Level::Ok, detail), Some(store))
        }
        Err(StoreError::Schema { found, .. }) => (
            Check::new(
                name,
                Level::Fail,
                format!("schema v{found}, this build expects v{}", schema_version()),
            )
            .hint(
                "run any writing command (e.g. `recalla extract`) with the newer binary to migrate",
            ),
            None,
        ),
        Err(e) => (Check::new(name, Level::Fail, clip(&e.to_string())), None),
    }
}

#[cfg(unix)]
fn check_data_dir_mode(dir: &Path) -> Check {
    use std::os::unix::fs::PermissionsExt;
    let name = "data dir mode";
    match fs::metadata(dir) {
        Err(_) => Check::new(name, Level::Skip, "no data dir yet"),
        Ok(m) if m.permissions().mode().trailing_zeros() >= 6 => {
            Check::new(name, Level::Ok, "private (no group/other access)")
        }
        Ok(m) => Check::new(
            name,
            Level::Warn,
            format!(
                "mode {:o}: other users can read your memories",
                m.permissions().mode() & 0o777
            ),
        )
        .hint(format!("chmod 700 {}", dir.display())),
    }
}

#[cfg(not(unix))]
fn check_data_dir_mode(_: &Path) -> Check {
    Check::new("data dir mode", Level::Skip, "not checked on this platform")
}

fn check_last_capture(inputs: &Inputs, store: Option<&Store>) -> Check {
    let name = "last capture";
    let Some(store) = store else {
        return Check::new(name, Level::Skip, "no database");
    };
    match store.ui_overview().ok().and_then(|o| o.last_event_ts) {
        None => Check::new(name, Level::Warn, "no session has been captured yet")
            .hint("finish a Claude Code session with hooks installed, or run `recalla backfill`"),
        Some(ts) if inputs.now_ms - ts > STALE_CAPTURE_MS => Check::new(
            name,
            Level::Warn,
            format!("newest captured event is {}", age(inputs.now_ms, ts)),
        )
        .hint("hooks may not be firing: check `hook log` below, then `recalla hooks status`"),
        Some(ts) => Check::new(
            name,
            Level::Ok,
            format!("newest event {}", age(inputs.now_ms, ts)),
        ),
    }
}

fn check_last_injection(inputs: &Inputs, store: Option<&Store>) -> Check {
    let name = "last injection";
    let Some(store) = store else {
        return Check::new(name, Level::Skip, "no database");
    };
    let newest = store
        .ui_injections(&InjectionFilter::default(), None, 1)
        .ok()
        .and_then(|p| p.rows.into_iter().next());
    match newest {
        None => Check::new(name, Level::Warn, "no pack has ever been injected")
            .hint("start a new Claude Code session in a project that has memories"),
        Some(i) => Check::new(
            name,
            Level::Ok,
            format!(
                "via {}, {}, ~{} tokens",
                i.via,
                age(inputs.now_ms, i.at),
                i.est_tokens
            ),
        ),
    }
}

fn check_mirror(store: Option<&Store>) -> Check {
    let name = "mirror";
    let Some(store) = store else {
        return Check::new(name, Level::Skip, "no database");
    };
    match store.mirror_dirty_count() {
        Ok(0) => Check::new(name, Level::Ok, "in sync"),
        Ok(n) => Check::new(
            name,
            Level::Warn,
            format!("{n} memories not yet written to the mirror"),
        )
        .hint("recalla mirror"),
        Err(e) => Check::new(name, Level::Warn, clip(&e.to_string())),
    }
}

fn check_hook_log(data_dir: &Path) -> Check {
    let name = "hook log";
    let lines = hook::log_tail(data_dir, HOOK_LOG_LINES);
    match lines.last() {
        None => Check::new(name, Level::Ok, "no hook errors logged"),
        Some(last) => Check::new(
            name,
            Level::Warn,
            format!("{} recent error(s), latest: {}", lines.len(), clip(last)),
        )
        .hint(format!("{}", data_dir.join("log/hook.log").display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_buckets() {
        assert_eq!(age(10_000, 0), "10 s ago");
        assert_eq!(age(10 * 60_000, 0), "10 min ago");
        assert_eq!(age(3 * 3_600_000, 0), "3 h ago");
        assert_eq!(age(5 * 86_400_000, 0), "5 days ago");
        assert_eq!(
            age(0, 5000),
            "0 s ago",
            "a future timestamp never goes negative"
        );
    }

    #[test]
    fn clip_is_one_line_and_bounded() {
        let long = format!("a\nb{}", "x".repeat(500));
        let c = clip(&long);
        assert!(!c.contains('\n'));
        assert!(c.chars().count() <= DETAIL_CHARS);
    }

    #[test]
    fn render_counts_and_failure_flag() {
        let checks = vec![
            Check::new("a", Level::Ok, "fine"),
            Check::new("b", Level::Warn, "meh").hint("do x"),
            Check::new("c", Level::Fail, "bad"),
        ];
        assert!(failed(&checks));
        let text = render_text(&checks);
        assert!(text.contains("fix: do x"));
        assert!(text.ends_with("1 failed, 1 warnings\n"));
        let json = render_json(&checks);
        assert_eq!(json["ok"], false);
        assert_eq!(json["checks"][1]["hint"], "do x");
        assert!(!failed(&checks[..2]));
    }
}
