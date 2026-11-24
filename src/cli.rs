//! Argument parsing and output formatting only; work happens in the library
//! modules.
//!
//! Exit codes: 0 ok, 1 some files failed, 2 usage or setup error.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use serde_json::{Value, json};

use crate::anchor;
use crate::doctor;
use crate::extract;
use crate::extract::scope::project_key;
use crate::harness::{claude, codex};
use crate::hook::{self, Ctx};
use crate::hooks_cfg;
use crate::ingest::{self, Options, Report};
use crate::mcp::{self, McpCtx};
use crate::mirror;
use crate::model::{Kind, Memory, Scope, Status};
use crate::ops::{
    self, PackArgs, Why, clean, open_existing, pack_for, prefix, truncate_chars, why,
};
use crate::pack;
use crate::paths;
use crate::select::{self, Selection};
use crate::store::{Filter, Hit, Store, Via};

#[derive(Debug, Parser)]
#[command(name = "recalla", version, about, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Which agents `backfill` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum HarnessSel {
    Claude,
    Codex,
    All,
}

impl HarnessSel {
    fn claude(self) -> bool {
        self != Self::Codex
    }

    fn codex(self) -> bool {
        self != Self::Claude
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Import Claude Code and Codex session history into the store (resumable).
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
            help = "Codex home [default: $CODEX_HOME or ~/.codex]"
        )]
        codex_root: Option<PathBuf>,
        /// Which agent's sessions to import.
        #[arg(long, value_enum, default_value_t = HarnessSel::All)]
        harness: HarnessSel,
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
        /// Don't update the markdown mirror afterwards.
        #[arg(long)]
        no_mirror: bool,
        #[arg(
            long = "dir",
            value_name = "PATH",
            help = "Only sessions run in this project directory or inside it (repeatable)"
        )]
        dirs: Vec<PathBuf>,
        #[arg(
            long = "session",
            value_name = "ID|FILE",
            help = "Only this session: its id (8+ characters is enough) or a .jsonl path (repeatable)"
        )]
        sessions: Vec<String>,
        /// List the sessions that would be imported, then exit (nothing is written).
        #[arg(long)]
        list: bool,
        /// Choose interactively: first Claude Code or Codex, then which sessions.
        #[arg(long, conflicts_with_all = ["dirs", "sessions", "list"])]
        pick: bool,
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
        /// Don't update the markdown mirror afterwards.
        #[arg(long)]
        no_mirror: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Sync the markdown mirror (one `.md` file per memory) with the database.
    Mirror {
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Re-check every memory and report stray files, not just recent changes.
        #[arg(long)]
        full: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Restore the memory layer from the markdown mirror (dry run unless --yes).
    Rebuild {
        #[arg(
            long,
            value_name = "DIR",
            num_args = 0..=1,
            required = true,
            help = "Rebuild from mirror files [default DIR: <data dir>/mirror]"
        )]
        #[allow(clippy::option_option)] // absent / flag only / flag with DIR
        from_mirror: Option<Option<PathBuf>>,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Apply the rebuild. Safe while Claude Code or `recalla mcp` run: a
        /// backup copy is kept and the live database is never swapped.
        #[arg(long)]
        yes: bool,
        /// Allow `--yes` to remove memories that are in the database but not
        /// in the mirror.
        #[arg(long)]
        allow_drop: bool,
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
        /// Show each candidate's score parts and why it was included or dropped.
        #[arg(long)]
        explain: bool,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
    },
    /// Check that Recalla is wired up and has fired (hooks, MCP, database, last capture/injection).
    Doctor {
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Claude Code settings file [default: `$CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json`].
        #[arg(long, value_name = "PATH")]
        settings: Option<PathBuf>,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
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
    /// Serve the local dashboard on 127.0.0.1 (Ctrl-C to stop).
    #[command(
        after_help = "Cookies are shared across ports on 127.0.0.1, so other local web servers could receive the session cookie: keep the dashboard closed when not in use."
    )]
    Ui {
        /// Port to listen on (0 = pick a free one).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Don't open the browser.
        #[arg(long)]
        no_open: bool,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
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
        /// With `install`: also recall memories anchored to a file before Read/Edit/Write.
        #[arg(long)]
        file_recall: bool,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
    },
    /// Check code anchors and flag memories whose code changed or vanished.
    Check {
        /// Project key [default: the git toplevel of the current directory].
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Confirm a memory is still right: re-anchor it to the code as it is now.
    Confirm {
        /// Memory id or a prefix of at least 4 hex characters.
        id: String,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
    },
    /// Drop a wrong memory: archive it (undo with `recalla confirm <id>`).
    Forget {
        /// Memory id or a prefix of at least 4 hex characters.
        id: String,
        #[arg(long, value_name = "PATH", help = DATA_DIR_HELP)]
        data_dir: Option<PathBuf>,
    },
    /// Move a memory to another scope: global (all projects) or one project.
    Promote {
        /// Memory id or a prefix of at least 4 hex characters.
        id: String,
        /// Make it apply to every project (the default).
        #[arg(long, conflicts_with = "project")]
        global: bool,
        /// Pin it to this project key instead.
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
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
    /// For `PreToolUse` on Read/Edit/Write (opt-in): memories anchored to the file.
    PreToolUse,
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
            codex_root,
            harness,
            data_dir,
            no_archive,
            dry_run,
            no_extract,
            no_mirror,
            dirs,
            sessions,
            list,
            pick,
            json,
        } => {
            let mut sel = Selection { dirs, sessions };
            let claude_root = harness.claude().then(|| root.or_else(claude::default_root));
            let codex_root = harness
                .codex()
                .then(|| codex_root.or_else(codex::default_root));
            let (claude_root, codex_root) = (claude_root.flatten(), codex_root.flatten());
            if claude_root.is_none() && codex_root.is_none() {
                return emit(err, "error: no root given and HOME is unset\n", USAGE);
            }
            let data_dir = match resolve_data_dir(data_dir) {
                Ok(d) => d,
                Err(msg) => return emit(err, &msg, USAGE),
            };
            let opts = Options {
                claude_root,
                codex_root,
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
            if pick {
                match pick_sessions(&opts, &mut io::stdin().lock(), out, err) {
                    Ok(Some(chosen)) => sel = chosen,
                    Ok(None) => return OK,
                    Err(code) => return code,
                }
            }
            if list {
                return list_sessions(&opts, &sel, json, out, err);
            }
            backfill(
                &opts,
                &sel,
                !(dry_run || no_extract),
                !(dry_run || no_mirror),
                json,
                out,
                err,
            )
        }
        Command::Extract {
            data_dir,
            rebuild,
            no_mirror,
            json,
        } => {
            let data_dir = match resolve_data_dir(data_dir) {
                Ok(d) => d,
                Err(msg) => return emit(err, &msg, USAGE),
            };
            match extract_into(&data_dir, rebuild) {
                Ok(r) => {
                    if !no_mirror {
                        sync_mirror(&data_dir, err);
                    }
                    emit(out, &extract_text(&r, json), OK)
                }
                Err(e) => emit(err, &format!("error: {e}\n"), USAGE),
            }
        }
        Command::Rebuild {
            from_mirror,
            data_dir,
            yes,
            allow_drop,
        } => run_rebuild(from_mirror.flatten(), data_dir, (yes, allow_drop), out, err),
        Command::Mirror {
            data_dir,
            full,
            json,
        } => run_mirror(data_dir, full, json, out, err),
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
            explain,
            data_dir,
        } => {
            let args = PackArgs {
                budget,
                project,
                query,
                no_log,
                via: Via::Cli,
                session_id: None,
            };
            run_pack(&args, format, explain, data_dir, out, err)
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
        Command::Ui {
            port,
            no_open,
            data_dir,
        } => run_ui(port, !no_open, data_dir, out, err),
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
                    HookEvent::PreToolUse => hook::Event::PreToolUse,
                };
                hook::run(event, std::io::stdin().lock(), out, &ctx);
            }
            OK
        }
        Command::Hooks {
            action,
            settings,
            dry_run,
            file_recall,
            data_dir,
        } => run_hooks(action, settings, dry_run, file_recall, data_dir, out, err),
        Command::Doctor {
            data_dir,
            settings,
            json,
        } => match resolve_data_dir(data_dir) {
            Ok(dir) => {
                let checks = doctor::run(&doctor::Inputs::from_env(dir, settings));
                let text = if json {
                    format!("{}\n", doctor::render_json(&checks))
                } else {
                    doctor::render_text(&checks)
                };
                emit(
                    out,
                    &text,
                    if doctor::failed(&checks) {
                        SOME_FAILED
                    } else {
                        OK
                    },
                )
            }
            Err(msg) => emit(err, &msg, USAGE),
        },
        Command::Stats { data_dir, json } => {
            read_only(data_dir, out, err, |s| stats_text(s, json).map(Why::Found))
        }
        Command::Check {
            project,
            data_dir,
            json,
        } => run_check(project, data_dir, json, out, err),
        Command::Confirm { id, data_dir } => run_confirm(&id, data_dir, out, err),
        Command::Forget { id, data_dir } => run_forget(&id, data_dir, out, err),
        Command::Promote {
            id,
            global: _,
            project,
            data_dir,
        } => run_promote(&id, project.as_deref(), data_dir, out, err),
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
    file_recall: bool,
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
        HooksAction::Install => hooks_cfg::install_with(&settings, &exe, dry_run, file_recall),
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
    let _ = writeln!(text, "mirror: {}", paths::mirror_dir(&data_dir).display());
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

/// `recalla check`: a full, unbudgeted staleness check of one project.
fn run_check(
    project: Option<String>,
    data_dir: Option<PathBuf>,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let Some(project) = project.or_else(default_project) else {
        return emit(err, "error: no project; pass --project\n", USAGE);
    };
    if anchor::project_dir(&project).is_none() {
        let msg = format!("error: {project} is not an existing absolute directory\n");
        return emit(err, &msg, USAGE);
    }
    let report = open_existing(&data_dir).and_then(|mut s| {
        let r = anchor::check(&mut s, &project, None).map_err(|e| e.to_string())?;
        warn_mirror(&mut s, &data_dir, err);
        Ok(r)
    });
    match report {
        Ok(r) if json => emit(out, &format!("{}\n", r.to_json()), OK),
        Ok(r) => emit(
            out,
            &format!(
                "checked {} anchors: {} newly stale, {} stale but matching again (confirm to clear)\n",
                r.checked, r.stale_new, r.recovered
            ),
            OK,
        ),
        Err(e) => emit(err, &format!("error: {e}\n"), SOME_FAILED),
    }
}

/// `recalla confirm <id>`.
fn run_confirm(
    id: &str,
    data_dir: Option<PathBuf>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let result = open_existing(&data_dir).and_then(|mut store| {
        let found = store
            .memories_by_uid_prefix(id)
            .map_err(|e| e.to_string())?;
        match found.as_slice() {
            [] => Err(format!("no memory matches {id}")),
            [m] => ops::confirm(&mut store, &m.uid).map(|n| {
                warn_mirror(&mut store, &data_dir, err);
                (m.uid.clone(), n)
            }),
            _ => Err(format!(
                "{id} matches {} memories; use a longer prefix",
                found.len()
            )),
        }
    });
    match result {
        Ok((uid, n)) => emit(
            out,
            &format!("Confirmed {} ({n} anchors re-resolved)\n", prefix(&uid, 8)),
            OK,
        ),
        Err(e) => emit(err, &format!("error: {e}\n"), SOME_FAILED),
    }
}

/// `recalla forget <id>`: archive one memory (human path, so Global memories
/// are allowed). An ambiguous or malformed id is a usage error listing the
/// candidates; an unknown id is a failure.
fn run_forget(
    id: &str,
    data_dir: Option<PathBuf>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let mut store = match open_existing(&data_dir) {
        Ok(s) => s,
        Err(e) => return emit(err, &format!("error: {e}\n"), SOME_FAILED),
    };
    let found = match store.memories_by_uid_prefix(id) {
        Ok(f) => f,
        Err(e) => return emit(err, &format!("error: {e}\n"), USAGE),
    };
    let m = match found.as_slice() {
        [] => {
            return emit(
                err,
                &format!("error: no memory matches {id}\n"),
                SOME_FAILED,
            );
        }
        [m] => m,
        many => {
            let mut t = format!(
                "error: {id} matches {} memories; use a longer prefix:\n",
                many.len()
            );
            for m in many {
                let _ = writeln!(
                    t,
                    "  {}  {}",
                    prefix(&m.uid, 12),
                    truncate_chars(&clean(&m.text), 70)
                );
            }
            return emit(err, &t, USAGE);
        }
    };
    let short = prefix(&m.uid, 8);
    if m.status == Status::Archived {
        return emit(out, &format!("{short} is already archived\n"), OK);
    }
    if let Err(e) = store.archive(&m.uid) {
        return emit(err, &format!("error: {e}\n"), SOME_FAILED);
    }
    warn_mirror(&mut store, &data_dir, err);
    emit(
        out,
        &format!(
            "Archived {short}: {}\nUndo with: recalla confirm {short}\n",
            truncate_chars(&clean(&m.text), 80)
        ),
        OK,
    )
}

/// `recalla promote`.
fn run_promote(
    id: &str,
    project: Option<&str>,
    data_dir: Option<PathBuf>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let mut store = match open_existing(&data_dir) {
        Ok(s) => s,
        Err(e) => return emit(err, &format!("error: {e}\n"), SOME_FAILED),
    };
    let found = match store.memories_by_uid_prefix(id) {
        Ok(f) => f,
        Err(e) => return emit(err, &format!("error: {e}\n"), USAGE),
    };
    let [m] = found.as_slice() else {
        let msg = if found.is_empty() {
            format!("error: no memory matches {id}\n")
        } else {
            format!(
                "error: {id} matches {} memories; use a longer prefix\n",
                found.len()
            )
        };
        return emit(
            err,
            &msg,
            if found.is_empty() { SOME_FAILED } else { USAGE },
        );
    };
    let target = match project.map(str::trim) {
        None => Scope::Global,
        Some("") => return emit(err, "error: --project needs a project key\n", USAGE),
        Some(key) => Scope::Project(key.to_owned()),
    };
    let new_uid = match ops::promote(&mut store, &m.uid, &target) {
        Ok(u) => u,
        Err(e) => return emit(err, &format!("error: {e}\n"), SOME_FAILED),
    };
    warn_mirror(&mut store, &data_dir, err);
    emit(
        out,
        &format!(
            "Moved {} to {}: {}\nNew id {}. Undo with: recalla forget {} && recalla confirm {}\n",
            prefix(&m.uid, 8),
            clean(&target.to_string()),
            truncate_chars(&clean(&m.text), 80),
            prefix(&new_uid, 8),
            prefix(&new_uid, 8),
            prefix(&m.uid, 8),
        ),
        OK,
    )
}

/// `recalla pack`. Hooks call this, so a missing database is a hint on
/// stderr with exit 0, never a failure.
fn run_pack(
    args: &PackArgs,
    format: Format,
    explain: bool,
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
            let text = match (format, explain) {
                (Format::Md, false) if p.markdown.is_empty() => String::new(),
                (Format::Md, false) => format!("{}\n", p.markdown),
                (Format::Md, true) => {
                    format!("{}\n\n{}", p.markdown, pack::explain_text(&p.decisions))
                }
                (Format::Json, false) => format!("{}\n", p.json),
                (Format::Json, true) => {
                    let mut j = p.json.clone();
                    j["explain"] = pack::explain_json(&p.decisions);
                    format!("{j}\n")
                }
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
                    "project": p.project.as_deref().map(ops::clean), "injections": p.injections, "est_tokens": p.est_tokens,
                })).collect::<Vec<_>>(),
                "top_memories": top.iter().map(|(m, uid, n)| json!({
                    "uid": ops::clean(uid), "count": n,
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
            ops::clean(p.project.as_deref().unwrap_or("(none)")),
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
/// Reads one trimmed line; `None` at end of input.
fn read_answer(input: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_owned()),
    }
}

/// Asks until `parse` accepts the answer. `None` on empty answer, `q` or end of input.
fn ask<T>(
    input: &mut impl BufRead,
    out: &mut impl Write,
    prompt: &str,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Option<T> {
    loop {
        let _ = write!(out, "{prompt}");
        let _ = out.flush();
        let a = read_answer(input)?;
        if a.is_empty() || a.eq_ignore_ascii_case("q") {
            return None;
        }
        match parse(&a) {
            Ok(v) => return Some(v),
            Err(m) => {
                let _ = writeln!(out, "  {m}");
            }
        }
    }
}

/// Interactive menu: pick a harness, then sessions. `Ok(None)` when cancelled,
/// `Err(code)` when there is nothing to choose from.
fn pick_sessions(
    opts: &Options,
    input: &mut impl BufRead,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<Option<Selection>, u8> {
    let all: Vec<_> = match ingest::list(opts, &Selection::default()) {
        Ok(f) => f
            .into_iter()
            .filter(|c| !select::is_subagent(&c.path))
            .collect(),
        Err(e) => return Err(emit(err, &format!("error: {e}\n"), USAGE)),
    };
    let mut harnesses: Vec<&str> = Vec::new();
    for c in &all {
        if !harnesses.contains(&c.agent) {
            harnesses.push(c.agent);
        }
    }
    if harnesses.is_empty() {
        return Err(emit(
            err,
            "error: no sessions found to choose from\n",
            USAGE,
        ));
    }
    let _ = writeln!(out, "Which tool's sessions?");
    for (i, h) in harnesses.iter().enumerate() {
        let n = all.iter().filter(|c| c.agent == *h).count();
        let _ = writeln!(out, "  {}) {h} ({n} sessions)", i + 1);
    }
    let Some(h) = ask(input, out, "Choose [number, q to cancel]: ", |a| {
        select::parse_choice(a, harnesses.len()).and_then(|v| {
            (v.len() == 1)
                .then(|| harnesses[v[0]])
                .ok_or_else(|| "choose one".to_owned())
        })
    }) else {
        return Ok(None);
    };
    let mut rows: Vec<_> = all.iter().filter(|c| c.agent == h).collect();
    rows.sort_by(|a, b| a.cwd.cmp(&b.cwd).then_with(|| a.path.cmp(&b.path)));
    let _ = writeln!(out, "\n{h} sessions:");
    for (i, c) in rows.iter().enumerate() {
        let id = select::session_ids(&c.path)
            .into_iter()
            .next()
            .unwrap_or_default();
        let id: String = id
            .chars()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let bytes = fs::metadata(&c.path).map_or(0, |m| m.len());
        let cwd = c
            .cwd
            .as_ref()
            .map_or_else(|| "?".to_owned(), |p| p.display().to_string());
        let _ = writeln!(out, "  {:>3}) {cwd}  …{id}  {}", i + 1, size(bytes));
    }
    let Some(chosen) = ask(
        input,
        out,
        "Import which? [e.g. 1,3-5 or a for all, q to cancel]: ",
        |a| select::parse_choice(a, rows.len()),
    ) else {
        return Ok(None);
    };
    let sessions = chosen
        .into_iter()
        .filter_map(|i| select::session_ids(&rows[i].path).into_iter().next())
        .collect();
    Ok(Some(Selection {
        dirs: Vec::new(),
        sessions,
    }))
}

fn list_sessions(
    opts: &Options,
    sel: &Selection,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let found = match ingest::list(opts, sel) {
        Ok(f) => f,
        Err(e) => return emit(err, &format!("error: {e}\n"), USAGE),
    };
    let text = if json {
        let rows: Vec<_> = found
            .iter()
            .map(|c| {
                json!({"agent": c.agent, "path": c.path.display().to_string(),
                       "cwd": c.cwd.as_ref().map(|p| p.display().to_string()),
                       "id": select::session_ids(&c.path).first()})
            })
            .collect();
        format!("{}\n", json!({"sessions": rows}))
    } else {
        let mut t = String::new();
        for c in &found {
            let id = select::session_ids(&c.path)
                .into_iter()
                .next()
                .unwrap_or_default();
            let cwd = c
                .cwd
                .as_ref()
                .map_or_else(|| "?".to_owned(), |p| p.display().to_string());
            let _ = writeln!(t, "{:<7} {id}  {cwd}", c.agent);
        }
        let _ = writeln!(t, "{} session(s)", found.len());
        t
    };
    emit(out, &text, OK)
}

fn backfill(
    opts: &Options,
    sel: &Selection,
    extract: bool,
    mirror: bool,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let report = match ingest::run_selected(opts, sel) {
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
    let mirrored = mirror.then(|| sync_mirror(&opts.data_dir, err)).flatten();
    let text = match (json, extracted) {
        (true, Some(x)) => format!(
            "{}\n",
            json!({"ingest": report.to_json(), "extract": x.to_json(),
                   "mirror": mirrored.as_ref().map(|p| p.display().to_string())})
        ),
        (true, None) => format!("{}\n", report.to_json()),
        (false, x) => {
            let mut text = human(&report, opts.dry_run);
            text += &x.map(|x| human_extract(&x)).unwrap_or_default();
            if let Some(p) = &mirrored {
                let _ = writeln!(text, "mirror     {}", p.display());
            }
            text
        }
    };
    emit(out, &text, code)
}

/// Incremental mirror sync that only warns on stderr; returns the mirror dir
/// when it worked.
fn sync_mirror(data_dir: &std::path::Path, err: &mut impl Write) -> Option<PathBuf> {
    match open_existing(data_dir) {
        Ok(mut store) => warn_mirror(&mut store, data_dir, err),
        Err(e) => {
            let _ = writeln!(err, "warning: mirror not updated: {e}");
            None
        }
    }
}

/// As [`sync_mirror`] for an already-open store.
fn warn_mirror(
    store: &mut Store,
    data_dir: &std::path::Path,
    err: &mut impl Write,
) -> Option<PathBuf> {
    match mirror::sync_best_effort(store, data_dir) {
        None => Some(paths::mirror_dir(data_dir)),
        Some(w) => {
            let _ = writeln!(err, "warning: {w}");
            None
        }
    }
}

/// `recalla rebuild --from-mirror [DIR]`.
fn run_rebuild(
    from: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    (yes, allow_drop): (bool, bool),
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let dir = from.unwrap_or_else(|| paths::mirror_dir(&data_dir));
    let r = match mirror::rebuild(&data_dir, &dir, yes, allow_drop) {
        Ok(r) => r,
        Err(e) => return emit(err, &format!("error: {e}\n"), SOME_FAILED),
    };
    let mut text = if r.applied {
        format!("rebuilt {} memories from {}\n", r.memories, dir.display())
    } else {
        format!(
            "plan: replace the memory layer in {} with {} memories from {}\n",
            paths::db_path(&data_dir).display(),
            r.memories,
            dir.display()
        )
    };
    let _ = writeln!(text, "  {} global", r.global);
    for (project, n) in &r.per_project {
        let _ = writeln!(text, "  {n:>5}  {project}");
    }
    if r.dropped_total > 0 {
        let _ = writeln!(
            text,
            "{} memories exist in the DB but not in the mirror (they will be removed from the memory layer; a backup is kept)",
            r.dropped_total
        );
        for d in &r.dropped {
            let _ = writeln!(text, "  {}  {}  {}", d.uid8, d.kind, d.text);
        }
    }
    if let Some(b) = &r.backup {
        let _ = writeln!(text, "previous database copied to {}", b.display());
    }
    if !r.applied {
        text.push_str(
            "events, sources and the injection ledger are kept; safe while Claude Code or recalla mcp are running\n\
             nothing changed; re-run with --yes to apply (add --allow-drop to remove the memories above)\n",
        );
    }
    emit(out, &text, OK)
}

/// `recalla mirror [--full]`.
fn run_mirror(
    data_dir: Option<PathBuf>,
    full: bool,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let dir = paths::mirror_dir(&data_dir);
    let report = open_existing(&data_dir)
        .and_then(|mut s| mirror::sync(&mut s, &dir, full).map_err(|e| e.to_string()));
    let r = match report {
        Ok(r) => r,
        Err(e) => return emit(err, &format!("error: {e}\n"), SOME_FAILED),
    };
    let strays: Vec<String> = r.strays.iter().map(|p| p.display().to_string()).collect();
    let text = if json {
        format!(
            "{}\n",
            json!({"dir": dir.display().to_string(), "written": r.written,
                   "unchanged": r.unchanged, "strays": strays})
        )
    } else {
        let mut t = format!(
            "mirror     {}\nwritten    {}\nunchanged  {}\n",
            dir.display(),
            r.written,
            r.unchanged
        );
        if !strays.is_empty() {
            let _ = writeln!(
                t,
                "strays     {} files not in the database (left in place):",
                strays.len()
            );
            for p in &strays {
                let _ = writeln!(t, "  {p}");
            }
        }
        t
    };
    emit(out, &text, OK)
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

/// `recalla ui`: serves the dashboard until Ctrl-C (default signal handling
/// ends the process; the server holds nothing that needs flushing).
fn run_ui(
    port: u16,
    open_browser: bool,
    data_dir: Option<PathBuf>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    use crate::ui::UiHandler;
    use crate::ui::security::{LOGIN_TTL, LoginCode, Token};
    use crate::ui::server::{ServeOptions, Server};

    let data_dir = match resolve_data_dir(data_dir) {
        Ok(d) => d,
        Err(msg) => return emit(err, &msg, USAGE),
    };
    let upgrade = |v| {
        let _ = writeln!(err, "Upgrading database to schema v{v}");
    };
    if let Err(e) = crate::ui::prepare(&data_dir, upgrade) {
        return emit(err, &format!("error: {e}\n"), SOME_FAILED);
    }
    let secrets = Token::generate().and_then(|t| {
        Token::generate().and_then(|c| LoginCode::generate(LOGIN_TTL).map(|l| (t, c, l)))
    });
    let (token, csrf, code) = match secrets {
        Ok(s) => s,
        Err(e) => return emit(err, &format!("error: no OS randomness: {e}\n"), SOME_FAILED),
    };
    let mut opts = ServeOptions::new(port, data_dir.clone());
    opts.open_browser = open_browser;
    let idle = opts.session_idle;
    let server = match Server::bind(opts) {
        Ok(s) => s,
        Err(e) => {
            return emit(
                err,
                &format!("error: cannot bind 127.0.0.1:{port}: {e}\n"),
                USAGE,
            );
        }
    };
    let bound = match server.local_addr() {
        Ok(a) => a.port(),
        Err(e) => return emit(err, &format!("error: {e}\n"), SOME_FAILED),
    };
    let url = crate::ui::login_url(bound, code.as_str());
    let text = format!(
        "Recalla is running at {url}\nThis link works once, for {} seconds. Run recalla ui again for a new one.\nPress Ctrl-C to stop\n",
        LOGIN_TTL.as_secs()
    );
    if emit(out, &text, OK) != OK {
        return SOME_FAILED;
    }
    let code = std::sync::Arc::new(code);
    let run_dir = crate::ui::login::run_dir(&data_dir);
    crate::ui::login::sweep_stale(&run_dir);
    let cleanup = if open_browser {
        open_login_page(&run_dir, bound, &code, err)
    } else {
        None
    };
    let handler = UiHandler::new(token, csrf, code, bound, data_dir).with_session_idle(idle);
    let result = server.run(handler);
    if let Some(c) = cleanup {
        c.finish();
    }
    match result {
        Ok(()) => OK,
        Err(e) => emit(err, &format!("error: ui: {e}\n"), SOME_FAILED),
    }
}

/// Writes the private redirect page and opens it (the login code never
/// reaches the opener's argv). On failure the user still has the printed URL.
fn open_login_page(
    run_dir: &std::path::Path,
    port: u16,
    code: &std::sync::Arc<crate::ui::security::LoginCode>,
    err: &mut impl Write,
) -> Option<crate::ui::login::PageCleanup> {
    use crate::ui::login;
    let page = match login::write_page(run_dir, port, code.as_str()) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(
                err,
                "note: cannot prepare the login page ({e}); open the URL above"
            );
            return None;
        }
    };
    let cleanup = login::PageCleanup::spawn(page.clone(), std::sync::Arc::clone(code));
    if !open_url(&page) {
        let _ = writeln!(err, "note: could not open a browser; open the URL above");
    }
    Some(cleanup)
}

/// Opens `target` with the platform opener (no shell; one argv entry).
fn open_url(target: &std::path::Path) -> bool {
    let (opener, args) = crate::ui::opener_command(target);
    let child = std::process::Command::new(opener)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match child {
        Ok(mut c) => {
            std::thread::spawn(move || c.wait());
            true
        }
        Err(_) => false,
    }
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
    let by_harness = if r.files_by_harness.is_empty() {
        "none".to_owned()
    } else {
        r.files_by_harness
            .iter()
            .map(|(h, n)| format!("{h} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut s = format!(
        "files      {} seen: {} new, {} resumed, {} restarted, {} unchanged, {} failed\n\
         harnesses  {}\n\
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
        by_harness,
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
