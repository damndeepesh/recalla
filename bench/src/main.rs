//! `membench`: a neutral benchmark for coding-agent memory tools.

mod adapter;
mod git;
mod matching;
mod mcp;
mod mine;
mod score;
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

#[derive(Subcommand)]
enum Command {
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
                    std::fs::write(&p, json + "\n").map_err(|e| format!("{}: {e}", p.display()))?
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
