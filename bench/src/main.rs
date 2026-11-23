//! `membench`: a neutral benchmark for coding-agent memory tools.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::cast_precision_loss))]

mod adapter;
mod fake;
mod git;
mod matching;
mod mcp;
mod mcp_adapter;
mod mine;
mod recalla;
mod report;
mod run;
mod score;
mod sessions;
mod suite;
mod tokens;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "membench", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct RunArgs {
    #[arg(long, num_args = 1.., required = true)]
    suite: Vec<PathBuf>,
    /// Directory of full-history clones named like the suites (see suites/build.sh).
    /// With it, tools get a real checkout at t0 that moves to t1.
    #[arg(long)]
    repos: Option<PathBuf>,
    #[command(flatten)]
    tool: ToolArgs,
}

/// Mines a suite from a local repository's own history and runs adapters on
/// it. Everything stays on this machine; nothing is fetched or uploaded.
#[derive(clap::Args)]
struct RealArgs {
    /// A local git repository with its history (not modified; a temporary clone is used).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Position of the feed cut-off in the history (0.0 oldest .. 1.0 newest).
    #[arg(long, default_value_t = 0.6)]
    t0: f64,
    /// Facts come from this many commits ending at the cut-off (default: all before it).
    #[arg(long)]
    window: Option<usize>,
    /// Commits between the cut-off and the code tasks run against (default: a tenth of the history).
    #[arg(long)]
    gap: Option<usize>,
    /// Commits after that where tasks come from (default: the rest).
    #[arg(long)]
    probe_window: Option<usize>,
    #[arg(long, default_value_t = 80)]
    max_probes: usize,
    #[command(flatten)]
    tool: ToolArgs,
}

#[derive(clap::Args)]
struct ToolArgs {
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "none,file,grep,dump,recalla"
    )]
    adapters: Vec<String>,
    #[arg(long)]
    recalla: Option<PathBuf>,
    /// Only the holdout (or tune) probes.
    #[arg(long)]
    split: Option<String>,
    /// Raw per-probe results as JSON.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Written report (markdown): totals, tune vs holdout, where each tool loses.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Command line of an MCP memory server, used by the `mcp` adapter.
    #[arg(long)]
    mcp_command: Option<String>,
    /// Name the `mcp` adapter reports under.
    #[arg(long, default_value = "mcp")]
    mcp_name: String,
    #[arg(long, default_value = "store")]
    mcp_store_tool: String,
    #[arg(long, default_value = "search")]
    mcp_recall_tool: String,
    #[arg(long, default_value = "text")]
    mcp_text_arg: String,
    #[arg(long, default_value = "query")]
    mcp_query_arg: String,
}

#[derive(Subcommand)]
enum Command {
    /// Run adapters over suites and print a markdown table.
    Run(RunArgs),
    /// Benchmark tools on your own repository's history, locally.
    Real(RealArgs),
    /// A tiny in-memory MCP memory server, for testing the `mcp` adapter.
    FakeMcp,
    /// Write a suite's facts as Claude Code session logs (for transcript-ingesting tools).
    Sessions {
        #[arg(long)]
        suite: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Mine a suite (facts and probes) from a pinned public repo checkout.
    Mine {
        /// Checkout with full history (see `fetch`).
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        license: String,
        /// Pinned commit the suite ends at.
        #[arg(long)]
        head: String,
        /// Position of the feed cut-off in the history (0.0 oldest .. 1.0 head).
        #[arg(long, default_value_t = 0.75)]
        t0: f64,
        /// Facts come from this many commits ending at t0.
        #[arg(long, default_value_t = 600)]
        window: usize,
        /// Commits between the cut-off and the working tree tasks run against.
        #[arg(long, default_value_t = 150)]
        gap: usize,
        /// Commits after that point that tasks are taken from.
        #[arg(long, default_value_t = 400)]
        probe_window: usize,
        #[arg(long, default_value_t = 80)]
        max_probes: usize,
        /// Output file (stdout when omitted).
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("membench: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Run(a) => cmd_run(&a),
        Command::Real(a) => cmd_real(&a),
        Command::FakeMcp => fake::serve(),
        Command::Sessions { suite, out } => {
            let text = std::fs::read_to_string(&suite).map_err(|e| e.to_string())?;
            let s: suite::Suite = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            sessions::write(&s, &out, &sessions::cwd(&s, std::path::Path::new("")))
        }
        Command::Mine {
            repo,
            name,
            url,
            license,
            head,
            t0,
            window,
            gap,
            probe_window,
            max_probes,
            out,
        } => {
            let params = mine::Params {
                name,
                url,
                license,
                head,
                t0_frac: t0,
                window,
                gap,
                probe_window,
                max_probes,
            };
            let suite = mine::mine(&repo, &params)?;
            let json = serde_json::to_string_pretty(&suite).map_err(|e| e.to_string())?;
            match out {
                Some(p) => {
                    std::fs::write(&p, json + "\n").map_err(|e| format!("{}: {e}", p.display()))?;
                }
                None => println!("{json}"),
            }
            eprintln!(
                "{}: {} facts, {} probes",
                suite.name,
                suite.facts.len(),
                suite.probes.len()
            );
            Ok(())
        }
    }
}

fn cmd_run(a: &RunArgs) -> Result<(), String> {
    let mut jobs = Vec::new();
    for path in &a.suite {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let s: suite::Suite =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        // `ripgrep-e2` is cloned as `ripgrep`.
        let base = s.name.rsplit_once("-e").map_or(s.name.as_str(), |(b, _)| b);
        let repo = a.repos.as_ref().map(|r| r.join(base));
        jobs.push((s, repo));
    }
    execute(&a.tool, &jobs, Some(suite::Split::Holdout))
}

fn cmd_real(a: &RealArgs) -> Result<(), String> {
    let tmp = tempfile::TempDir::new().map_err(|e| e.to_string())?;
    let repo = a
        .repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", a.repo.display()))?;
    let name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| sessions::safe_name(n))
        .unwrap_or("local");
    // A private clone, so the user's repository is never touched.
    let clone = tmp.path().join("clone");
    git::git(
        tmp.path(),
        &[
            "clone",
            "-q",
            "--no-checkout",
            &repo.display().to_string(),
            &clone.display().to_string(),
        ],
    )?;
    let head = git::git(&clone, &["rev-parse", "HEAD"])?.trim().to_owned();
    let n: usize = git::git(&clone, &["rev-list", "--count", &head])?
        .trim()
        .parse()
        .map_err(|_| "cannot count commits".to_owned())?;
    let params = mine::Params {
        name: format!("{name}-local"),
        url: "local".into(),
        license: "local".into(),
        head,
        t0_frac: a.t0,
        window: a.window.unwrap_or(n),
        gap: a.gap.unwrap_or((n / 10).max(5)),
        probe_window: a.probe_window.unwrap_or(n),
        max_probes: a.max_probes,
    };
    let suite = mine::mine(&clone, &params)?;
    eprintln!(
        "{}: {} facts, {} probes mined from {} (kept local)",
        suite.name,
        suite.facts.len(),
        suite.probes.len(),
        repo.display()
    );
    if suite.probes.is_empty() || suite.facts.is_empty() {
        return Err(
            "too few causal commit messages in this history to build probes; try a larger --window or a repo with explanatory commit messages"
                .into(),
        );
    }
    execute(&a.tool, &[(suite, Some(clone))], None)
}

fn execute(
    tool: &ToolArgs,
    jobs: &[(suite::Suite, Option<PathBuf>)],
    focus: Option<suite::Split>,
) -> Result<(), String> {
    let mcp = tool.mcp_command.as_ref().map(|c| mcp_adapter::Spec {
        name: tool.mcp_name.clone(),
        command: c.split_whitespace().map(str::to_owned).collect(),
        store_tool: tool.mcp_store_tool.clone(),
        recall_tool: tool.mcp_recall_tool.clone(),
        text_arg: tool.mcp_text_arg.clone(),
        query_arg: tool.mcp_query_arg.clone(),
    });
    let split = match tool.split.as_deref().map(str::to_owned).or_else(|| {
        focus.map(|f| {
            if f == suite::Split::Holdout {
                "holdout"
            } else {
                "tune"
            }
            .to_owned()
        })
    }) {
        None => None,
        Some(s) if s == "holdout" => Some(suite::Split::Holdout),
        Some(s) if s == "tune" => Some(suite::Split::Tune),
        Some(o) => return Err(format!("unknown split {o}")),
    };
    let tmp = tempfile::TempDir::new().map_err(|e| e.to_string())?;
    let base = tmp.path().to_path_buf();
    let mut results = Vec::new();
    for (s, repo) in jobs {
        for name in &tool.adapters {
            let dir = run::scratch(&base, &s.name, name)?;
            let mut a = run::make(name, tool.recalla.as_deref(), mcp.as_ref(), &dir)?;
            results.push(run::one(s, a.as_mut(), split, &dir, repo.as_deref())?);
        }
    }
    println!("{}", run::table(&results));
    if let Some(p) = &tool.report {
        let suites: Vec<suite::Suite> = jobs.iter().map(|j| j.0.clone()).collect();
        std::fs::write(p, report::render(&results, &suites, split))
            .map_err(|e| format!("{}: {e}", p.display()))?;
    }
    if let Some(p) = &tool.out {
        let json = serde_json::to_string_pretty(&results).map_err(|e| e.to_string())?;
        std::fs::write(p, json).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}
