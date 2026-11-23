//! Scoring. Every definition is mechanical; the same rules apply to every
//! tool and are documented in `SCORING.md`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::adapter::{Item, Recall};
use crate::matching::{self, coverage, words};
use crate::suite::{Fact, Probe, Suite};

/// Outcome of one probe at one budget and query style.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeScore {
    pub probe: String,
    pub budget: usize,
    pub with_files: bool,
    /// Share of required facts returned.
    pub recall: f64,
    /// 1 minus the share of returned tokens spent on facts about other files.
    pub noise: f64,
    /// Stale facts: 1 flagged, 0.5 left out, 0 served as current. `None` when the probe has none.
    pub staleness: Option<f64>,
    /// Share of returned required facts whose source the tool can name. `None` when not measured.
    pub provenance: Option<f64>,
    /// Live facts the tool wrongly flagged as stale.
    pub false_flags: u32,
    pub tokens: usize,
    /// `max(0, 1 - tokens / dump) * recall`: tokens saved, but only counted if the fact arrived.
    pub efficiency: f64,
}

/// Tokens of the whole fact store, the no-budget reference.
#[must_use]
pub fn dump_tokens(suite: &Suite) -> usize {
    suite
        .facts
        .iter()
        .map(|f| crate::tokens::est(&f.feed_text()) + 1)
        .sum()
}

/// Scores one recall against a probe. `source` is asked for provenance only
/// when `measure_provenance` is set (it can be slow).
pub fn score_probe(
    suite: &Suite,
    probe: &Probe,
    r: &Recall,
    budget: usize,
    with_files: bool,
    source: Option<&mut dyn FnMut(&Item) -> Option<String>>,
) -> ProbeScore {
    let by_id: HashMap<&str, &Fact> = suite.facts.iter().map(|f| (f.id.as_str(), f)).collect();
    let item_words: Vec<HashSet<String>> = r.items.iter().map(|i| words(&i.text)).collect();
    // Which items contain a fact, and is any of them flagged.
    let find = |fact: &Fact| -> Vec<usize> {
        let fw = words(&fact.feed_text());
        (0..r.items.len())
            .filter(|&i| coverage(&fw, &item_words[i]) >= matching::THRESHOLD)
            .collect()
    };

    let required: Vec<&Fact> = probe
        .required
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .collect();
    let stale: Vec<&Fact> = probe
        .stale
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .collect();

    let hits: Vec<(&Fact, Vec<usize>)> = required.iter().map(|f| (*f, find(f))).collect();
    let found = hits.iter().filter(|(_, ix)| !ix.is_empty()).count();
    let recall = ratio(found, required.len());

    let tokens = r.tokens();
    let noise = if r.items.is_empty() {
        0.0
    } else {
        1.0 - noise_share(suite, probe, r, &item_words)
    };

    let staleness = (!stale.is_empty()).then(|| {
        if r.items.is_empty() {
            return 0.0;
        }
        let total: f64 = stale
            .iter()
            .map(|f| {
                let ix = find(f);
                if ix.is_empty() {
                    0.5
                } else if ix.iter().any(|&i| r.items[i].flagged_stale) {
                    1.0
                } else {
                    0.0
                }
            })
            .sum();
        #[allow(clippy::cast_precision_loss)]
        {
            total / stale.len() as f64
        }
    });

    let false_flags = u32::try_from(
        hits.iter()
            .filter(|(_, ix)| ix.iter().any(|&i| r.items[i].flagged_stale))
            .count(),
    )
    .unwrap_or(u32::MAX);

    let provenance = source.map(|ask| {
        let mut ok = 0;
        let mut n = 0;
        for (fact, ix) in &hits {
            let Some(&i) = ix.first() else { continue };
            n += 1;
            if ask(&r.items[i]).is_some_and(|s| names_source(&s, fact)) {
                ok += 1;
            }
        }
        ratio_or_zero(ok, n)
    });

    let dump = dump_tokens(suite).max(1);
    #[allow(clippy::cast_precision_loss)]
    let saved = (1.0 - tokens as f64 / dump as f64).max(0.0);

    ProbeScore {
        probe: probe.id.clone(),
        budget,
        with_files,
        recall,
        noise,
        staleness,
        provenance,
        false_flags,
        tokens,
        efficiency: saved * recall,
    }
}

/// A provenance answer is right when it names the fact's commit.
fn names_source(answer: &str, fact: &Fact) -> bool {
    fact.commit
        .get(..7)
        .is_some_and(|short| answer.contains(short))
}

/// Share of returned tokens in items that contain a fact about other files.
fn noise_share(suite: &Suite, probe: &Probe, r: &Recall, item_words: &[HashSet<String>]) -> f64 {
    let unrelated: Vec<HashSet<String>> = suite
        .facts
        .iter()
        .filter(|f| !f.files.iter().any(|x| probe.files.contains(x)))
        .map(|f| words(&f.feed_text()))
        .collect();
    let mut noisy = 0;
    let mut total = 0;
    for (item, iw) in r.items.iter().zip(item_words) {
        let cost = crate::tokens::est(&item.text) + 1;
        total += cost;
        if unrelated
            .iter()
            .any(|fw| coverage(fw, iw) >= matching::THRESHOLD)
        {
            noisy += cost;
        }
    }
    ratio_or_zero(noisy, total)
}

#[allow(clippy::cast_precision_loss)]
fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 { 1.0 } else { n as f64 / d as f64 }
}

#[allow(clippy::cast_precision_loss)]
fn ratio_or_zero(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

/// Extraction result for a tool that ingests transcripts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extraction {
    /// Suite facts found among the tool's memories.
    pub recall: f64,
    /// Memories that match no suite fact: not an error (labels are
    /// incomplete), reported so the cost of noise is visible.
    pub unlabeled_share: f64,
    pub memories: usize,
}

#[must_use]
pub fn score_extraction(suite: &Suite, memories: &[String]) -> Extraction {
    let mem_words: Vec<HashSet<String>> = memories.iter().map(|m| words(m)).collect();
    let found = suite
        .facts
        .iter()
        .filter(|f| matching::contains(&f.text, &mem_words))
        .count();
    let fact_words: Vec<HashSet<String>> = suite.facts.iter().map(|f| words(&f.text)).collect();
    let matched = mem_words
        .iter()
        .filter(|m| {
            fact_words
                .iter()
                .any(|f| coverage(f, m) >= matching::THRESHOLD)
        })
        .count();
    Extraction {
        recall: ratio(found, suite.facts.len()),
        unlabeled_share: if memories.is_empty() {
            0.0
        } else {
            1.0 - ratio(matched, memories.len())
        },
        memories: memories.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::{RepoRef, Split, State};

    fn fact(id: &str, text: &str, files: &[&str], state: State) -> Fact {
        Fact {
            id: id.into(),
            kind: "gotcha".into(),
            text: text.into(),
            files: files.iter().map(|s| (*s).into()).collect(),
            commit: format!("{id}abcdef0123456789"),
            split: Split::Tune,
            state,
            survival: 1.0,
        }
    }

    fn suite() -> Suite {
        Suite {
            name: "t".into(),
            repo: RepoRef {
                url: String::new(),
                license: String::new(),
                t0: String::new(),
                t1: String::new(),
                head: String::new(),
            },
            facts: vec![
                fact(
                    "live1",
                    "The parser drops trailing newline because the lexer eats it early",
                    &["a.rs"],
                    State::Live,
                ),
                fact(
                    "stale1",
                    "The glob builder panics when the pattern list is empty at startup",
                    &["a.rs"],
                    State::Stale,
                ),
                fact(
                    "other1",
                    "Windows console output needs the vt flag enabled before printing",
                    &["w.rs"],
                    State::Live,
                ),
            ],
            probes: vec![],
        }
    }

    fn probe() -> Probe {
        Probe {
            id: "p1".into(),
            task: "fix parser".into(),
            files: vec!["a.rs".into()],
            commit: "c".into(),
            required: vec!["live1".into()],
            stale: vec!["stale1".into()],
            split: Split::Tune,
        }
    }

    fn it(s: &Suite, id: &str, flagged: bool) -> Item {
        let f = s.facts.iter().find(|f| f.id == id).unwrap();
        Item {
            text: f.feed_text(),
            id: Some(id.into()),
            flagged_stale: flagged,
        }
    }

    #[test]
    fn perfect_recall_flagged_stale_and_no_noise() {
        let s = suite();
        let r = Recall {
            items: vec![it(&s, "live1", false), it(&s, "stale1", true)],
        };
        let sc = score_probe(&s, &probe(), &r, 1500, true, None);
        assert!((sc.recall - 1.0).abs() < 1e-9);
        assert!((sc.noise - 1.0).abs() < 1e-9);
        assert_eq!(sc.staleness, Some(1.0));
        assert_eq!(sc.false_flags, 0);
    }

    #[test]
    fn stale_served_as_current_scores_zero_and_absent_half() {
        let s = suite();
        let served = Recall {
            items: vec![it(&s, "live1", false), it(&s, "stale1", false)],
        };
        assert_eq!(
            score_probe(&s, &probe(), &served, 1500, true, None).staleness,
            Some(0.0)
        );
        let absent = Recall {
            items: vec![it(&s, "live1", false)],
        };
        assert_eq!(
            score_probe(&s, &probe(), &absent, 1500, true, None).staleness,
            Some(0.5)
        );
    }

    #[test]
    fn empty_return_gets_no_credit_for_noise_or_staleness() {
        let s = suite();
        let sc = score_probe(&s, &probe(), &Recall::default(), 1500, true, None);
        assert!(sc.recall.abs() < 1e-9);
        assert!(sc.noise.abs() < 1e-9);
        assert_eq!(sc.staleness, Some(0.0));
    }

    #[test]
    fn facts_about_other_files_are_noise_and_flagged_live_is_a_false_flag() {
        let s = suite();
        let r = Recall {
            items: vec![it(&s, "live1", true), it(&s, "other1", false)],
        };
        let sc = score_probe(&s, &probe(), &r, 1500, true, None);
        assert!(sc.noise < 1.0 && sc.noise > 0.0);
        assert_eq!(sc.false_flags, 1);
    }

    #[test]
    fn provenance_needs_the_source_to_name_the_commit() {
        let s = suite();
        let r = Recall {
            items: vec![it(&s, "live1", false)],
        };
        let mut good = |_: &Item| Some("from commit live1ab details".to_owned());
        let sc = score_probe(&s, &probe(), &r, 1500, true, Some(&mut good));
        assert_eq!(sc.provenance, Some(1.0));
        let mut bad = |_: &Item| Some("somewhere".to_owned());
        let sc = score_probe(&s, &probe(), &r, 1500, true, Some(&mut bad));
        assert_eq!(sc.provenance, Some(0.0));
        let mut none = |_: &Item| None;
        assert_eq!(
            score_probe(&s, &probe(), &r, 1500, true, Some(&mut none)).provenance,
            Some(0.0)
        );
    }

    #[test]
    fn efficiency_is_savings_times_recall() {
        let s = suite();
        let r = Recall {
            items: vec![it(&s, "live1", false)],
        };
        let sc = score_probe(&s, &probe(), &r, 1500, true, None);
        let dump = dump_tokens(&s) as f64;
        let expect = (1.0 - sc.tokens as f64 / dump) * 1.0;
        assert!((sc.efficiency - expect).abs() < 1e-9);
        let all = Recall {
            items: s.facts.iter().map(|f| it(&s, &f.id, false)).collect(),
        };
        assert!(
            score_probe(&s, &probe(), &all, 1500, true, None)
                .efficiency
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn extraction_recall_and_unlabeled_share() {
        let s = suite();
        let mems = vec![
            "The parser drops trailing newline because the lexer eats it early".to_owned(),
            "Something nobody labeled about the build system".to_owned(),
        ];
        let e = score_extraction(&s, &mems);
        assert!((e.recall - 1.0 / 3.0).abs() < 1e-9);
        assert!((e.unlabeled_share - 0.5).abs() < 1e-9);
        assert_eq!(score_extraction(&s, &[]).memories, 0);
    }
}
