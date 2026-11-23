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
        "recalla" | "recalla-backfill" => Box::new(Recalla::new(
            recalla.ok_or("--recalla PATH is required")?,
            scratch,
            name == "recalla-backfill",
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
                // Provenance is measured once per probe, at the middle budget with hints.
                let measure = budget == BUDGETS[1] && with_files;
                let mut score = if measure && adapter.has_provenance() {
                    let mut ask = |i: &crate::adapter::Item| adapter.source(i);
                    score::score_probe(suite, p, &r, budget, with_files, Some(&mut ask))
                } else {
                    score::score_probe(suite, p, &r, budget, with_files, None)
                };
                probes.push(score);
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

pub fn scratch(base: &Path, suite: &str, adapter: &str) -> std::result::Result<PathBuf, String> {
    if !crate::sessions::safe_name(suite) || !crate::sessions::safe_name(adapter) {
        return Err(format!(
            "unsafe suite or adapter name: {suite:?} / {adapter:?}"
        ));
    }
    let p = scratch_for(base, suite, adapter);
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
    Ok(p)
}

fn mean(xs: impl Iterator<Item = f64>) -> Option<f64> {
    let v: Vec<f64> = xs.collect();
    #[allow(clippy::cast_precision_loss)]
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Markdown table: mean per adapter at each budget (with file hints).
pub fn table(results: &[Result]) -> String {
    let mut out = String::from(
        "| adapter | budget | recall | noise | staleness | provenance | tokens | efficiency |\n|---|---|---|---|---|---|---|---|\n",
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
                "| {a} | {b} | {} | {} | {} | {} | {tok} | {} |\n",
                f(|p| p.recall),
                f(|p| p.noise),
                f(|p| p.staleness),
                f(|p| p.provenance),
                f(|p| p.efficiency),
            ));
        }
    }
    out.push_str(
        "\n| adapter | extraction recall | unlabeled share | memories |\n|---|---|---|---|\n",
    );
    let mut seen: Vec<&str> = Vec::new();
    for r in results {
        if seen.contains(&r.adapter.as_str()) {
            continue;
        }
        let ex: Vec<&score::Extraction> = results
            .iter()
            .filter(|x| x.adapter == r.adapter)
            .filter_map(|x| x.extraction.as_ref())
            .collect();
        if ex.is_empty() {
            continue;
        }
        seen.push(&r.adapter);
        #[allow(clippy::cast_precision_loss)]
        let m = |g: fn(&score::Extraction) -> f64| {
            ex.iter().map(|e| g(e)).sum::<f64>() / ex.len() as f64
        };
        out.push_str(&format!(
            "| {} | {:.2} | {:.2} | {} |\n",
            r.adapter,
            m(|e| e.recall),
            m(|e| e.unlabeled_share),
            ex.iter().map(|e| e.memories).sum::<usize>()
        ));
    }
    out
}
