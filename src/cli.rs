//! Argument parsing and output formatting only; work happens in the library
//! modules.
//!
//! Exit codes: 0 ok, 1 some files failed, 2 usage or setup error.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use serde_json::{Value, json};

use crate::extract;
use crate::harness::claude;
use crate::ingest::{self, Options, Report};
use crate::model::{Kind, Scope};
use crate::paths;
use crate::store::{Filter, Hit, Store};

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
}

const DATA_DIR_HELP: &str =
    "Recalla data dir [default: $RECALLA_HOME, $XDG_DATA_HOME/recalla or ~/.local/share/recalla]";

const OK: u8 = 0;
const SOME_FAILED: u8 = 1;
const USAGE: u8 = 2;

/// Runs the CLI with `args` (including the program name) and returns the
/// process exit code. Normal output goes to `out`, diagnostics to `err`.
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
    }
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

fn resolve_data_dir(given: Option<PathBuf>) -> Result<PathBuf, String> {
    given
        .or_else(paths::data_dir)
        .ok_or_else(|| "error: no --data-dir given and HOME is unset\n".to_owned())
}

/// Opens the existing store; never creates one.
fn open_existing(data_dir: &std::path::Path) -> Result<Store, String> {
    let db = paths::db_path(data_dir);
    if !db.is_file() {
        return Err(format!(
            "no database at {}; run `recalla backfill` first",
            db.display()
        ));
    }
    Store::open(&db).map_err(|e| format!("cannot open store: {e}"))
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

fn truncate_chars(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let mut cut: String = one_line.chars().take(max.saturating_sub(3)).collect();
    cut.push_str("...");
    cut
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

/// First `n` chars of `s`.
fn prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
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
