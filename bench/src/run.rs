//! Runs adapters over suites and reports.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::adapter::{Adapter, Ctx, Dump, File, Grep, NoMemory, Support, scratch_for};
use crate::recalla::Recalla;
use crate::score::{self, ProbeScore};
use crate::suite::{Split, Suite};

pub const BUDGETS: [usize; 3] = [500, 1500, 4000];

#[derive(Serialize)]
pub struct Result {
    pub suite: String,
    pub adapter: String,
    pub probes: Vec<ProbeScore>,
    pub extraction: Option<score::Extraction>,
}

pub fn make(
    name: &str,
    recalla: Option<&Path>,
    scratch: &Path,
) -> std::result::Result<Box<dyn Adapter>, String> {
    Ok(match name {
        "none" => Box::new(NoMemory),
        "dump" => Box::new(Dump::default()),
        "file" => Box::new(File::default()),
        "grep" => Box::new(Grep::default()),
        "recalla" => Box::new(Recalla::new(
            recalla.ok_or("--recalla PATH is required")?,
            scratch,
        )?),
        other => return Err(format!("unknown adapter {other}")),
    })
}

pub fn one(
    suite: &Suite,
    adapter: &mut dyn Adapter,
    split: Option<Split>,
    dir: &Path,
) -> std::result::Result<Result, String> {
    let ctx = Ctx {
        suite,
        repo: dir,
        worktree: dir,
        scratch: dir,
    };
    if adapter.feed_facts(&ctx)? == Support::Unsupported {
        return Err(format!("{} cannot be fed", adapter.name()));
    }
    adapter.advance(&ctx)?;
    let mut probes = Vec::new();
    for p in suite
        .probes
        .iter()
        .filter(|p| split.is_none_or(|s| s == p.split))
    {
        for budget in BUDGETS {
            for with_files in [false, true] {
                let r = adapter.recall(&p.query(with_files), budget)?;
                // Provenance is measured once per probe, at the middle budget.
                let measure = budget == BUDGETS[1] && with_files;
                let mut ask = |i: &crate::adapter::Item| adapter_source(i);
                let _ = &mut ask;
                probes.push(score::score_probe(suite, p, &r, budget, with_files, None));
                let _ = measure;
            }
        }
    }
    let extraction = adapter
        .exported()?
        .map(|m| score::score_extraction(suite, &m));
    Ok(Result {
        suite: suite.name.clone(),
        adapter: adapter.name(),
        probes,
        extraction,
    })
}

fn adapter_source(_: &crate::adapter::Item) -> Option<String> {
    None
}

pub fn scratch(base: &Path, suite: &str, adapter: &str) -> PathBuf {
    let p = scratch_for(base, suite, adapter);
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).ok();
    p
}

fn mean(xs: impl Iterator<Item = f64>) -> Option<f64> {
    let v: Vec<f64> = xs.collect();
    #[allow(clippy::cast_precision_loss)]
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Markdown table: mean per adapter at each budget (with file hints).
pub fn table(results: &[Result]) -> String {
    let mut out = String::from(
        "| adapter | budget | recall | noise | staleness | tokens | efficiency |\n|---|---|---|---|---|---|---|\n",
    );
    let mut names: Vec<&str> = results.iter().map(|r| r.adapter.as_str()).collect();
    names.dedup();
    names.sort_unstable();
    names.dedup();
    for a in names {
        for b in BUDGETS {
            let s: Vec<&ProbeScore> = results
                .iter()
                .filter(|r| r.adapter == a)
                .flat_map(|r| r.probes.iter())
                .filter(|p| p.budget == b && p.with_files)
                .collect();
            let f = |g: fn(&ProbeScore) -> Option<f64>| {
                mean(s.iter().filter_map(|p| g(p))).map_or("n/a".to_owned(), |v| format!("{v:.2}"))
            };
            #[allow(clippy::cast_precision_loss)]
            let tok =
                mean(s.iter().map(|p| p.tokens as f64)).map_or("n/a".into(), |v| format!("{v:.0}"));
            out.push_str(&format!(
                "| {a} | {b} | {} | {} | {} | {tok} | {} |\n",
                f(|p| p.recall),
                f(|p| p.noise),
                f(|p| p.staleness),
                f(|p| p.efficiency),
            ));
        }
    }
    out
}
