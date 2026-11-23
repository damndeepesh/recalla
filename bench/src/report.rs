//! The written report: headline scores per adapter, tune vs holdout, and
//! where each tool loses. Everything is derived from the same per-probe
//! results that `--out` writes as JSON.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::run::{BUDGETS, Result, mean};
use crate::score::{Extraction, ProbeScore};
use crate::suite::{Split, Suite};

/// The budget the headline is read at.
const HEADLINE: usize = BUDGETS[1];

/// The three pillars, each 0..1, `None` when the tool has no such interface.
#[derive(Debug, Default, PartialEq)]
pub struct Pillars {
    /// Mean of recall, noise, staleness and provenance (those that apply).
    pub usefulness: Option<f64>,
    /// Share of suite facts found among the tool's own extracted memories.
    pub extraction: Option<f64>,
    /// Token savings versus dumping everything, counted only if the fact arrived.
    pub savings: Option<f64>,
}

impl Pillars {
    /// Equal-weight mean of the pillars that apply, and how many did.
    #[must_use]
    pub fn total(&self) -> (Option<f64>, usize) {
        let v: Vec<f64> = [self.usefulness, self.extraction, self.savings]
            .into_iter()
            .flatten()
            .collect();
        (mean(v.iter().copied()), v.len())
    }
}

#[must_use]
pub fn pillars(scores: &[&ProbeScore], extractions: &[&Extraction]) -> Pillars {
    let metric = |g: fn(&ProbeScore) -> Option<f64>| mean(scores.iter().filter_map(|p| g(p)));
    let parts = [
        metric(|p| p.recall),
        metric(|p| p.noise),
        metric(|p| p.staleness),
        metric(|p| p.provenance),
    ];
    Pillars {
        usefulness: mean(parts.into_iter().flatten()),
        extraction: mean(extractions.iter().map(|e| e.recall)),
        savings: metric(|p| p.efficiency),
    }
}

fn fmt(v: Option<f64>) -> String {
    v.map_or("n/a".to_owned(), |v| format!("{v:.2}"))
}

fn adapters(results: &[Result]) -> Vec<String> {
    let mut v: Vec<String> = results.iter().map(|r| r.adapter.clone()).collect();
    v.sort();
    v.dedup();
    v
}

/// Probes of one adapter, optionally limited to a split, at the headline
/// budget with file hints.
fn headline<'a>(
    results: &'a [Result],
    adapter: &str,
    split_of: &HashMap<(&str, &str), Split>,
    split: Option<Split>,
) -> Vec<&'a ProbeScore> {
    results
        .iter()
        .filter(|r| r.adapter == adapter)
        .flat_map(|r| r.probes.iter().map(move |p| (r, p)))
        .filter(|(_, p)| p.budget == HEADLINE && p.with_files)
        .filter(|(r, p)| {
            split.is_none_or(|s| split_of.get(&(r.suite.as_str(), p.probe.as_str())) == Some(&s))
        })
        .map(|(_, p)| p)
        .collect()
}

#[must_use]
pub fn render(results: &[Result], suites: &[Suite]) -> String {
    let split_of: HashMap<(&str, &str), Split> = suites
        .iter()
        .flat_map(|s| {
            s.probes
                .iter()
                .map(move |p| ((s.name.as_str(), p.id.as_str()), p.split))
        })
        .collect();
    let names = adapters(results);
    let mut out = String::new();

    let _ = writeln!(
        out,
        "# membench report\n\nHeadline: holdout probes, {HEADLINE}-token budget, file hints given.\n"
    );
    out.push_str("## Total (equal weight across the pillars that apply)\n\n");
    out.push_str(
        "| adapter | usefulness | extraction | token savings | total | pillars |\n|---|---|---|---|---|---|\n",
    );
    for a in &names {
        let scores = headline(results, a, &split_of, Some(Split::Holdout));
        let ex: Vec<&Extraction> = results
            .iter()
            .filter(|r| &r.adapter == a)
            .filter_map(|r| r.extraction.as_ref())
            .collect();
        let p = pillars(&scores, &ex);
        let (total, n) = p.total();
        let _ = writeln!(
            out,
            "| {a} | {} | {} | {} | {} | {n}/3 |",
            fmt(p.usefulness),
            fmt(p.extraction),
            fmt(p.savings),
            fmt(total),
        );
    }
    out.push_str(
        "\nA pillar a tool has no interface for is n/a and left out of its total, never scored as zero. Compare totals only between tools with the same pillar count.\n",
    );

    out.push_str("\n## Tune vs holdout\n\nA large gap means the tool or miner was fitted to the tune split.\n\n");
    out.push_str(
        "| adapter | split | probes | recall | noise | staleness |\n|---|---|---|---|---|---|\n",
    );
    for a in &names {
        for (label, split) in [("tune", Split::Tune), ("holdout", Split::Holdout)] {
            let s = headline(results, a, &split_of, Some(split));
            let f = |g: fn(&ProbeScore) -> Option<f64>| fmt(mean(s.iter().filter_map(|p| g(p))));
            let _ = writeln!(
                out,
                "| {a} | {label} | {} | {} | {} | {} |",
                s.len(),
                f(|p| p.recall),
                f(|p| p.noise),
                f(|p| p.staleness),
            );
        }
    }

    out.push_str("\n## Where each tool loses\n\nLowest-recall holdout probes per tool (ties broken by probe id).\n");
    let task_of: HashMap<(&str, &str), &str> = suites
        .iter()
        .flat_map(|s| {
            s.probes
                .iter()
                .map(move |p| ((s.name.as_str(), p.id.as_str()), p.task.as_str()))
        })
        .collect();
    for a in &names {
        let mut worst: Vec<(f64, &str, &str)> = results
            .iter()
            .filter(|r| &r.adapter == a)
            .flat_map(|r| r.probes.iter().map(move |p| (r, p)))
            .filter(|(r, p)| {
                p.budget == HEADLINE
                    && p.with_files
                    && split_of.get(&(r.suite.as_str(), p.probe.as_str())) == Some(&Split::Holdout)
            })
            .filter_map(|(r, p)| Some((p.recall?, r.suite.as_str(), p.probe.as_str())))
            .collect();
        worst.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(y.1)).then(x.2.cmp(y.2)));
        let _ = writeln!(out, "\n### {a}\n");
        let misses = worst.iter().filter(|w| w.0 < 1.0).count();
        let _ = writeln!(
            out,
            "{misses} of {} holdout probes with required facts missed at least one.\n",
            worst.len()
        );
        for (rec, suite, probe) in worst.iter().take(3).filter(|w| w.0 < 1.0) {
            let task = task_of.get(&(*suite, *probe)).copied().unwrap_or("");
            let _ = writeln!(out, "- {suite} `{probe}` recall {rec:.2}: {task}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(recall: Option<f64>, noise: Option<f64>, eff: Option<f64>) -> ProbeScore {
        ProbeScore {
            recall,
            noise,
            efficiency: eff,
            ..ProbeScore::default()
        }
    }

    #[test]
    fn total_averages_only_pillars_that_apply() {
        let a = ps(Some(1.0), Some(0.5), Some(0.5));
        let p = pillars(&[&a], &[]);
        assert_eq!(p.usefulness, Some(0.75));
        assert_eq!(p.extraction, None);
        let (t, n) = p.total();
        assert_eq!(n, 2);
        assert!((t.unwrap() - 0.625).abs() < 1e-9);
    }

    #[test]
    fn nothing_measured_is_n_a_not_zero() {
        let p = pillars(&[], &[]);
        assert_eq!(p, Pillars::default());
        assert_eq!(p.total(), (None, 0));
    }

    #[test]
    fn extraction_pillar_is_mean_recall() {
        let e = |r| Extraction {
            recall: r,
            unlabeled_share: 0.0,
            memories: 1,
        };
        let (a, b) = (e(0.2), e(0.4));
        let p = pillars(&[], &[&a, &b]);
        assert!((p.extraction.unwrap() - 0.3).abs() < 1e-9);
    }
}
