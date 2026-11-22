//! Argument parsing and output formatting only; work happens in the library
//! modules.
//!
//! Exit codes: 0 ok, 1 some files failed, 2 usage or setup error.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use serde_json::{Value, json};

use crate::extract;
use crate::extract::scope::project_key;
use crate::harness::claude;
use crate::hook::{self, Ctx};
use crate::hooks_cfg;
use crate::ingest::{self, Options, Report};
use crate::mcp::{self, McpCtx};
use crate::model::{Kind, Memory, Scope};
use crate::ops::{PackArgs, Why, open_existing, pack_for, prefix, truncate_chars, why};
use crate::paths;
use crate::store::{Filter, Hit, Store, Via};

#[derive(Debug, Parser)]
#[command(name = "recalla", version, about, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Import Claude Code session history into the store (resumable).
    Backfill {
        #[arg(
            long,
            value_name = "PATH",
            help = "Claude config dir [default: $CLAUDE_CONFIG_DIR or ~/.claude]"
        )]
        root: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Recalla data dir [default: $RECALLA_HOME, $XDG_DATA_HOME/recalla or ~/.local/share/recalla]"
        )]
        data_dir: Option<PathBuf>,
        /// Don't keep a raw zstd copy of ingested session bytes.
        #[arg(long)]
        no_archive: bool,
        /// Parse, redact and count, but write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the memory extraction that normally follows the import.
        #[arg(long)]
        no_extract: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Extract memories from imported events (incremental).
    Extract {
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Archive backfill-only memories and re-derive everything.
        #[arg(long)]
        rebuild: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Full-text search over memories (inspection aid).
    Search {
        /// Words to look for (all must match).
        query: String,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Only this kind; repeat for several.
        #[arg(long = "kind", value_name = "KIND")]
        kinds: Vec<Kind>,
        /// Only memories scoped to exactly this project key.
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
        /// Maximum hits.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Print hits as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print a token-budgeted context pack for a project (empty if none).
    Pack {
        /// Token budget, measured with a conservative estimate.
        #[arg(long, default_value_t = 1500)]
        budget: usize,
        /// Project key [default: the git toplevel of the current directory].
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
        /// Favor memories matching these words.
        #[arg(long, value_name = "TEXT")]
        query: Option<String>,
        #[arg(long, value_enum, default_value_t = Format::Md)]
        format: Format,
        /// Don't record this injection in the ledger.
        #[arg(long)]
        no_log: bool,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
    },
    /// Show what memory has been injected and what it cost.
    Stats {
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Serve MCP tools (recall, remember, review, why) over stdio.
    Mcp {
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Project key [default: `$CLAUDE_PROJECT_DIR`, else the git toplevel of the current directory].
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
    },
    /// Claude Code hook entry point; reads the hook JSON on stdin. Always exits 0.
    Hook {
        #[arg(value_enum)]
        event: HookEvent,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Claude config dir [default: $CLAUDE_CONFIG_DIR or ~/.claude]"
        )]
        root: Option<PathBuf>,
    },
    /// Install, remove or inspect Recalla's Claude Code hooks.
    Hooks {
        #[arg(value_enum)]
        action: HooksAction,
        /// Settings file [default: `$CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json`].
        #[arg(long, value_name = "PATH")]
        settings: Option<PathBuf>,
        /// Show what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
    },
    /// Explain a memory: its provenance and the source text it came from.
    Why {
        /// Memory id or a prefix of at least 4 hex characters.
        id: String,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Which hook `recalla hook` serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum HookEvent {
    SessionStart,
    /// For `Stop` and `PreCompact`.
    Capture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum HooksAction {
    Install,
    Uninstall,
    Status,
}

/// Output format of `recalla pack`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Md,
    Json,
}

const DATA_DIR_HELP: &str =
    "Recalla data dir [default: $RECALLA_HOME, $XDG_DATA_HOME/recalla or ~/.local/share/recalla]";

const OK: u8 = 0;
const SOME_FAILED: u8 = 1;
const USAGE: u8 = 2;

/// Runs the CLI with `args` (including the program name) and returns the
/// process exit code. Normal output goes to `out`, diagnostics to `err`.
#[allow(clippy::too_many_lines)] // one flat dispatch over subcommands
pub fn run<I, T>(args: I, out: &mut impl Write, err: &mut impl Write) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let sink: &mut dyn Write = if e.use_stderr() { err } else { out };
            return emit(
                sink,
                &e.render().to_string(),
                u8::try_from(e.exit_code()).unwrap_or(USAGE),
            );
        }
    };
    match cli.command {
        Command::Backfill {
            root,
            data_dir,
            no_archive,
            dry_run,
            no_extract,
            json,
        } => {
            let Some(root) = root.or_else(claude::default_root) else {
                return emit(err, "error: no --root given and HOME is unset\n", USAGE);
            };
            let data_dir = match resolve_data_dir(data_dir) {
                Ok(d) => d,
                Err(msg) => return emit(err, &msg, USAGE),
            };
            let opts = Options {
                root,
                data_dir,
                archive: !no_archive,
                dry_run,
            };
            if !dry_run && let Some(mode) = paths::loose_dir_mode(&opts.data_dir) {
                // Not chmod-ed: the dir may be shared on purpose. The DB itself is 0600.
                let msg = format!(
                    "warning: {} is accessible to group/others (mode {mode:o}); consider chmod 700\n",
                    opts.data_dir.display()
                );
                if emit(err, &msg, OK) != OK {
                    return SOME_FAILED;
                }
            }
            backfill(&opts, !(dry_run || no_extract), json, out, err)
        }
        Command::Extract {
            data_dir,
            rebuild,
            json,
        } => {
            let data_dir = match resolve_data_dir(data_dir) {
                Ok(d) => d,
                Err(msg) => return emit(err, &msg, USAGE),
            };
            match extract_into(&data_dir, rebuild) {
                Ok(r) => emit(out, &extract_text(&r, json), OK),
                Err(e) => emit(err, &format!("error: {e}\n"), USAGE),
            }
        }
        Command::Search {
            query,
            data_dir,
            kinds,
            project,
            limit,
            json,
        } => {
            let data_dir = match resolve_data_dir(data_dir) {
                Ok(d) => d,
                Err(msg) => return emit(err, &msg, USAGE),
            };
            let filter = Filter {
                kinds,
                scope: project.map(Scope::Project),
                ..Filter::default()
            };
            let hits = open_existing(&data_dir)
                .and_then(|s| s.search(&query, &filter, limit).map_err(|e| e.to_string()));
            match hits {
                Ok(hits) => {
                    let text = if json {
                        format!("{}\n", Value::Array(hits.iter().map(hit_json).collect()))
                    } else {
                        human_hits(&hits)
                    };
                    emit(out, &text, OK)
                }
                Err(e) => emit(err, &format!("error: {e}\n"), USAGE),
            }
        }
        Command::Pack {
            budget,
            project,
            query,
            format,
            no_log,
            data_dir,
        } => {
            let args = PackArgs {
                budget,
                project,
                query,
                no_log,
                via: Via::Cli,
            };
            run_pack(&args, format, data_dir, out, err)
        }
        Command::Mcp { data_dir, project } => {
            let data_dir = match resolve_data_dir(data_dir) {
                Ok(d) => d,
                Err(msg) => return emit(err, &msg, USAGE),
            };
            let project = project.or_else(default_project);
            let ctx = McpCtx { data_dir, project };
            // stdout is protocol-only, so `out` is not used here.
            let (stdin, stdout) = (std::io::stdin(), std::io::stdout());
            match mcp::serve(stdin.lock(), stdout.lock(), &ctx) {
                Ok(()) => OK,
                Err(e) => emit(err, &format!("error: mcp: {e}\n"), SOME_FAILED),
            }
        }
        Command::Hook {
            event,
            data_dir,
            root,
        } => {
            // A hook never fails the session: no data dir means nothing to do.
            if let Some(data_dir) = data_dir.or_else(paths::data_dir) {
                hook::silence_panics();
                let ctx = Ctx {
                    data_dir,
                    root: root.or_else(claude::default_root),
                };
                let event = match event {
                    HookEvent::SessionStart => hook::Event::SessionStart,
                    HookEvent::Capture => hook::Event::Capture,
                };
                hook::run(event, std::io::stdin().lock(), out, &ctx);
            }
            OK
        }
        Command::Hooks {
            action,
            settings,
            dry_run,
            data_dir,
        } => run_hooks(action, settings, dry_run, data_dir, out, err),
        Command::Stats { data_dir, json } => {
            read_only(data_dir, out, err, |s| stats_text(s, json).map(Why::Found))
        }
        Command::Why { id, data_dir, json } => {
            read_only(data_dir, out, err, |s| why(s, &id, json, true))
        }
    }
}

/// `recalla hooks install|uninstall|status`.
fn run_hooks(
    action: HooksAction,
    settings: Option<PathBuf>,
    dry_run: bool,
    data_dir: Option<PathBuf>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let Some(settings) =
        settings.or_else(|| claude::default_root().map(|r| r.join("settings.json")))
    else {
        return emit(err, "error: no --settings given and HOME is unset\n", USAGE);
    };
    let exe = match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(e) => e,
        Err(e) => {
            return emit(
                err,
                &format!("error: cannot locate recalla binary: {e}\n"),
                USAGE,
            );
        }
    };
    let result = match action {
        HooksAction::Install => hooks_cfg::install(&settings, &exe, dry_run),
        HooksAction::Uninstall => hooks_cfg::uninstall(&settings, dry_run),
        HooksAction::Status => return hooks_status(&settings, data_dir, out, err),
    };
    let o = match result {
        Ok(o) => o,
        Err(e) => return emit(err, &format!("error: {e}\n"), SOME_FAILED),
    };
    let mut text = format!("settings: {}\n", settings.display());
    for line in &o.changes {
        let _ = writeln!(text, "  {line}");
    }
    match (&o.backup, o.changed, dry_run) {
        (_, false, _) => text += "no changes\n",
        (_, true, true) => text += "dry run: nothing written\n",
        (Some(b), true, false) => {
            let _ = writeln!(text, "backup: {}", b.display());
        }
        (None, true, false) => text += "created new settings file\n",
    }
    if action == HooksAction::Install {
        text += &hooks_cfg::mcp_hints(&exe);
    }
    emit(out, &text, OK)
}

fn hooks_status(
    settings: &std::path::Path,
    data_dir: Option<PathBuf>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let events = match hooks_cfg::installed(settings) {
        Ok(e) => e,
        Err(e) => return emit(err, &format!("error: {e}\n"), SOME_FAILED),
    };
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let mut text = format!("settings: {}\n", settings.display());
    for (event, yes) in events {
        let _ = writeln!(
            text,
            "  {event}: {}",
            if yes { "installed" } else { "not installed" }
        );
    }
    let _ = writeln!(text, "data dir: {}", data_dir.display());
    match open_existing(&data_dir).and_then(|s| s.memory_counts().map_err(|e| e.to_string())) {
        Ok(counts) => {
            let mut by_status = std::collections::BTreeMap::<String, u64>::new();
            for (_, status, n) in counts {
                *by_status.entry(status.to_string()).or_default() += n;
            }
            let _ = writeln!(
                text,
                "database: present, memories {}",
                kind_counts(&by_status)
            );
        }
        Err(_) if !paths::db_path(&data_dir).is_file() => text += "database: missing\n",
        Err(e) => {
            let _ = writeln!(text, "database: unreadable ({e})");
        }
    }
    let tail = hook::log_tail(&data_dir, 5);
    if tail.is_empty() {
        text += "hook log: empty\n";
    } else {
        text += "hook log (last 5):\n";
        for l in &tail {
            let _ = writeln!(text, "  {l}");
        }
    }
    emit(out, &text, OK)
}

/// Opens the existing store and prints what `f` produces: exit 0 for a
/// found result, 2 for an ambiguous one or any error, 1 for a missing one.
fn read_only(
    data_dir: Option<PathBuf>,
    out: &mut impl Write,
    err: &mut impl Write,
    f: impl FnOnce(&Store) -> Result<Why, String>,
) -> u8 {
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    match open_existing(&data_dir).and_then(|s| f(&s)) {
        Ok(Why::Found(text)) => emit(out, &text, OK),
        Ok(Why::Ambiguous(text)) => emit(err, &text, USAGE),
        Ok(Why::Missing(what)) => emit(
            err,
            &format!("error: no memory matches {what}\n"),
            SOME_FAILED,
        ),
        Err(e) => emit(err, &format!("error: {e}\n"), USAGE),
    }
}

/// `recalla pack`. Hooks call this, so a missing database is a hint on
/// stderr with exit 0, never a failure.
fn run_pack(
    args: &PackArgs,
    format: Format,
    data_dir: Option<PathBuf>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let db = paths::db_path(&data_dir);
    if !db.is_file() {
        let hint = format!(
            "recalla: no database at {}; run `recalla backfill` first\n",
            db.display()
        );
        return emit(err, &hint, OK);
    }
    let project = args.project.clone().or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        Some(project_key(&cwd.to_string_lossy()))
    });
    let args = PackArgs {
        project,
        ..args.clone()
    };
    match pack_for(&data_dir, &args) {
        Ok(p) => {
            let text = match format {
                Format::Md if p.markdown.is_empty() => String::new(),
                Format::Md => format!("{}\n", p.markdown),
                Format::Json => format!("{}\n", p.json),
            };
            emit(out, &text, OK)
        }
        Err(e) => emit(err, &format!("error: {e}\n"), SOME_FAILED),
    }
}

fn stats_text(store: &Store, json: bool) -> Result<String, String> {
    let st = store.injection_stats().map_err(|e| e.to_string())?;
    let counts = store.memory_counts().map_err(|e| e.to_string())?;
    let top: Vec<(Option<Memory>, &String, u64)> = st
        .top_memories
        .iter()
        .map(|(uid, n)| Ok((store.get(uid).map_err(|e| e.to_string())?, uid, *n)))
        .collect::<Result<_, String>>()?;
    let avg = st.est_tokens.checked_div(st.injections).unwrap_or(0);
    if json {
        return Ok(format!(
            "{}\n",
            json!({
                "injections": st.injections,
                "est_tokens": st.est_tokens,
                "avg_est_tokens": avg,
                "per_project": st.per_project.iter().map(|p| json!({
                    "project": p.project, "injections": p.injections, "est_tokens": p.est_tokens,
                })).collect::<Vec<_>>(),
                "top_memories": top.iter().map(|(m, uid, n)| json!({
                    "uid": uid, "count": n,
                    "kind": m.as_ref().map(|m| m.kind.to_string()),
                    "text": m.as_ref().map(|m| truncate_chars(&m.text, 120)),
                })).collect::<Vec<_>>(),
                "memories": counts.iter().map(|(k, s, n)| json!({
                    "kind": k.to_string(), "status": s.to_string(), "count": n,
                })).collect::<Vec<_>>(),
            })
        ));
    }
    let mut lines = vec![format!(
        "injections  {} (avg {avg} tok, total {} tok est.)",
        st.injections, st.est_tokens
    )];
    for p in &st.per_project {
        lines.push(format!(
            "  {}  {} injections, {} tok",
            p.project.as_deref().unwrap_or("(none)"),
            p.injections,
            p.est_tokens
        ));
    }
    if !top.is_empty() {
        lines.push("most injected".to_owned());
    }
    for (m, uid, n) in &top {
        let what = m.as_ref().map_or_else(String::new, |m| {
            format!("{}  {}", m.kind, truncate_chars(&m.text, 80))
        });
        lines.push(format!("  {n:>3}x  {}  {what}", prefix(uid, 8)));
    }
    let by_status = counts
        .iter()
        .map(|(k, s, n)| format!("{k}/{s} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(format!(
        "memories    {}",
        if by_status.is_empty() {
            "none"
        } else {
            &by_status
        }
    ));
    Ok(lines.join("\n") + "\n")
}

/// Ingest, then (when `extract`) extract; one combined report.
fn backfill(
    opts: &Options,
    extract: bool,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let report = match ingest::run(opts) {
        Ok(report) => report,
        Err(e) => return emit(err, &format!("error: {e}\n"), USAGE),
    };
    let code = if report.failed.is_empty() {
        OK
    } else {
        SOME_FAILED
    };
    let extracted = if extract {
        match extract_into(&opts.data_dir, false) {
            Ok(r) => Some(r),
            Err(e) => return emit(err, &format!("error: {e}\n"), USAGE),
        }
    } else {
        None
    };
    let text = match (json, extracted) {
        (true, Some(x)) => format!(
            "{}\n",
            json!({"ingest": report.to_json(), "extract": x.to_json()})
        ),
        (true, None) => format!("{}\n", report.to_json()),
        (false, x) => {
            human(&report, opts.dry_run) + &x.map(|x| human_extract(&x)).unwrap_or_default()
        }
    };
    emit(out, &text, code)
}

fn extract_text(r: &extract::Report, json: bool) -> String {
    if json {
        format!("{}\n", r.to_json())
    } else {
        human_extract(r)
    }
}

/// `$CLAUDE_PROJECT_DIR` (set by Claude Code), else the current directory,
/// resolved to its git toplevel.
fn default_project() -> Option<String> {
    let dir = std::env::var("CLAUDE_PROJECT_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    Some(project_key(&dir.to_string_lossy()))
}

fn resolve_data_dir(given: Option<PathBuf>) -> Result<PathBuf, String> {
    given
        .or_else(paths::data_dir)
        .ok_or_else(|| "error: no --data-dir given and HOME is unset\n".to_owned())
}

fn extract_into(data_dir: &std::path::Path, rebuild: bool) -> Result<extract::Report, String> {
    let mut store = open_existing(data_dir)?;
    extract::run(&mut store, &extract::Options { rebuild }).map_err(|e| e.to_string())
}

fn kind_counts(m: &std::collections::BTreeMap<String, u64>) -> String {
    if m.is_empty() {
        return "none".to_owned();
    }
    m.iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Short aligned summary of an extraction run. Counts only.
fn human_extract(r: &extract::Report) -> String {
    let mut lines = vec![format!(
        "extracted  {} events -> {} new memories ({}), {} duplicates",
        r.events_processed,
        r.new_total(),
        kind_counts(&r.memories_new),
        r.duplicate_total(),
    )];
    if r.restarted && r.memories_archived_for_rebuild > 0 {
        lines.push(format!(
            "rebuild    {} backfill-only memories archived first",
            r.memories_archived_for_rebuild
        ));
    }
    if r.tasks_archived > 0 {
        lines.push(format!("tasks      {} archived as done", r.tasks_archived));
    }
    lines.push(format!("time       {} ms", r.duration_ms));
    lines.join("\n") + "\n"
}

fn human_hits(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return "no matches\n".to_owned();
    }
    let mut lines = Vec::new();
    for h in hits {
        let m = &h.memory;
        lines.push(format!(
            "{}  {}  conf {:.2}  seen {}  {}",
            prefix(&m.uid, 8),
            m.kind,
            m.confidence,
            m.seen_count,
            truncate_chars(&m.text, 160),
        ));
        if let Some(p) = m.provenance.first() {
            let line = p.line.map_or_else(|| "?".to_owned(), |l| l.to_string());
            lines.push(format!(
                "          from {} line {line}",
                prefix(p.session_id.as_deref().unwrap_or("?"), 8)
            ));
        }
    }
    lines.join("\n") + "\n"
}

fn hit_json(h: &Hit) -> Value {
    let m = &h.memory;
    json!({
        "uid": m.uid,
        "kind": m.kind.to_string(),
        "text": m.text,
        "scope": m.scope.to_string(),
        "confidence": m.confidence,
        "importance": m.importance,
        "seen_count": m.seen_count,
        "score": h.score,
        "provenance": m.provenance.iter().map(|p| json!({
            "session_id": p.session_id,
            "source_path": p.source_path,
            "line": p.line,
        })).collect::<Vec<_>>(),
    })
}

/// Writes `text` and returns `code`; if even writing fails, the exit code is
/// all that's left to report it with.
fn emit(sink: &mut dyn Write, text: &str, code: u8) -> u8 {
    match sink.write_all(text.as_bytes()).and_then(|()| sink.flush()) {
        Ok(()) => code,
        Err(_) => code.max(SOME_FAILED),
    }
}

/// Bytes as a short human size (base 1024, one decimal).
fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    #[allow(clippy::cast_precision_loss)] // display only
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Short aligned summary. Counts only, never content.
fn human(r: &Report, dry_run: bool) -> String {
    let rules: Vec<String> = r
        .redactions_by_rule
        .iter()
        .map(|(rule, n)| format!("{rule} {n}"))
        .collect();
    let mut s = format!(
        "files      {} seen: {} new, {} resumed, {} restarted, {} unchanged, {} failed\n\
         events     {} added, {} malformed lines skipped\n\
         redacted   {}{}\n\
         archive    {} -> {}\n\
         time       {} ms\n",
        r.files_seen,
        r.files_new,
        r.files_resumed,
        r.files_restarted,
        r.files_unchanged,
        r.failed.len(),
        r.events_added,
        r.malformed_lines,
        r.redactions(),
        if rules.is_empty() {
            String::new()
        } else {
            format!(" ({})", rules.join(", "))
        },
        size(r.archive_raw_bytes),
        size(r.archive_compressed_bytes),
        r.duration_ms,
    );
    for f in &r.failed {
        s.extend(["failed     ", &f.path, " (", &f.error, ")\n"]);
    }
    if dry_run {
        s.push_str("dry run: nothing was written\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_args(args: &[&str]) -> (u8, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(args.iter().copied(), &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn version_and_usage_errors() {
        let (code, out, _) = run_args(&["recalla", "--version"]);
        assert_eq!((code, out.trim()), (0, crate::version_line().as_str()));
        assert_eq!(run_args(&["recalla"]).0, 2);
        let (code, _, err) = run_args(&["recalla", "backfill", "--bogus"]);
        assert_eq!(code, 2);
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(10 * 1024 * 1024), "10.0 MB");
    }

    #[test]
    fn human_summary_lists_failures_and_rules() {
        let mut r = Report {
            files_seen: 3,
            events_added: 7,
            ..Report::default()
        };
        r.redactions_by_rule.insert("github-token".into(), 2);
        r.failed.push(ingest::Failure {
            path: "/a.jsonl".into(),
            error: "PermissionDenied".into(),
        });
        let s = human(&r, true);
        assert!(s.contains("3 seen"));
        assert!(s.contains("7 added, 0 malformed"));
        assert!(s.contains("2 (github-token 2)"));
        assert!(s.contains("/a.jsonl (PermissionDenied)"));
        assert!(s.ends_with("dry run: nothing was written\n"));
    }
}
