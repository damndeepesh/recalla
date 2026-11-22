//! Context packs: rank memories and fill a token budget. Pure, no I/O.
//!
//! The budget is enforced on [`est_tokens`], a deliberate overestimate
//! (D-016), so the real token count is always lower.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::model::{Kind, Memory, Status};

/// Longest memory text (in chars) shown on one pack line.
const MAX_LINE_CHARS: usize = 300;
/// Recency half-life-ish constant, in days.
const RECENCY_DAYS: f64 = 90.0;
const MS_PER_DAY: f64 = 86_400_000.0;
/// Section titles in output order; the last one holds `needs_reverify`.
const TITLES: [&str; 7] = [
    "Preferences",
    "Gotchas",
    "Decisions",
    "Procedures",
    "Tasks",
    "Facts",
    "Needs re-verify ⚠",
];
const REVERIFY: usize = 6;
/// Most items per section, in `TITLES` order. A wall of one-off commands or
/// tasks drowns the rules; ranking picks the best within each cap.
const SECTION_CAP: [usize; 7] = [15, 10, 10, 6, 8, 8, 8];

/// Conservative token estimate: `ceil(utf8_bytes / 3)` plus 2 per line.
#[must_use]
pub fn est_tokens(s: &str) -> usize {
    s.len().div_ceil(3) + 2 * (s.matches('\n').count() + 1)
}

/// A rendered pack. `markdown` is empty (and `est_tokens` 0) when nothing fit.
#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    pub markdown: String,
    pub json: Value,
    pub est_tokens: usize,
    /// Included memories, best ranked first.
    pub included_uids: Vec<String>,
}

const fn kind_weight(kind: Kind) -> f64 {
    match kind {
        Kind::Preference => 1.0,
        Kind::Gotcha => 0.9,
        Kind::Decision => 0.85,
        Kind::Procedure => 0.7,
        Kind::Task => 0.6,
        Kind::Fact => 0.5,
    }
}

/// `kind_weight x confidence x (1 + ln seen_count) x recency`, where recency
/// is `0.5 + 0.5 exp(-age_days / 90)` from `updated_at`. `query_boost` is the
/// memory's normalized search relevance in `0..=1` (`None`: not a hit),
/// which multiplies the score by `1.5 + boost`.
#[must_use]
pub fn score(m: &Memory, now_ms: i64, query_boost: Option<f64>) -> f64 {
    #[allow(clippy::cast_precision_loss)] // ms timestamps are far below 2^52
    let age_days = (now_ms - m.updated_at).max(0) as f64 / MS_PER_DAY;
    let recency = 0.5 + 0.5 * (-age_days / RECENCY_DAYS).exp();
    let seen = f64::from(m.seen_count.max(1)).ln() + 1.0;
    let base = kind_weight(m.kind) * f64::from(m.confidence) * seen * recency;
    base * query_boost.map_or(1.0, |q| 1.5 + q.clamp(0.0, 1.0))
}

/// Min-max normalizes search scores (higher is better) into `0..=1`. A single
/// hit, or all-equal scores, map to 1.
#[must_use]
pub fn query_boosts(hits: &[(String, f64)]) -> HashMap<String, f64> {
    let max = hits.iter().map(|h| h.1).fold(f64::NEG_INFINITY, f64::max);
    let min = hits.iter().map(|h| h.1).fold(f64::INFINITY, f64::min);
    let span = max - min;
    hits.iter()
        .map(|(uid, s)| {
            let norm = if span > 0.0 { (s - min) / span } else { 1.0 };
            (uid.clone(), norm)
        })
        .collect()
}

/// Whitespace collapsed to single spaces, cut to `max` chars with `…`.
/// Collapse `text` to one line of at most `max` chars, for packs and terminal
/// output. Whitespace runs become one space and control chars (ESC, C0/C1) are
/// dropped, so memory text can't fake structure or emit terminal escapes.
#[must_use]
pub fn one_line(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .filter(|c| c.is_whitespace() || !c.is_control())
        .collect();
    let flat = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Running size of the markdown: exact, since every line's bytes and the
/// newlines between them are known as lines are added.
#[derive(Clone, Copy, Default)]
struct Cost {
    bytes: usize,
    lines: usize,
}

impl Cost {
    const fn add(&mut self, line_bytes: usize) {
        self.bytes += line_bytes + (self.lines > 0) as usize;
        self.lines += 1;
    }

    const fn est(self) -> usize {
        self.bytes.div_ceil(3) + 2 * self.lines
    }
}

fn header(label: &str, est: usize, budget: usize) -> String {
    format!("# Recalla memory · project {label} · {est}/{budget} tok")
}

const fn section(m: &Memory) -> usize {
    if matches!(m.status, Status::NeedsReverify) {
        return REVERIFY;
    }
    match m.kind {
        Kind::Preference => 0,
        Kind::Gotcha => 1,
        Kind::Decision => 2,
        Kind::Procedure => 3,
        Kind::Task => 4,
        Kind::Fact => 5,
    }
}

/// `- text [uid8]`, with ` (N×)` for procedures seen more than once.
/// `None` for a memory with no visible text.
fn item(m: &Memory) -> Option<(String, String)> {
    let text = one_line(&m.text, MAX_LINE_CHARS);
    if text.is_empty() {
        return None;
    }
    let seen = if m.kind == Kind::Procedure && m.seen_count > 1 {
        format!(" ({}×)", m.seen_count)
    } else {
        String::new()
    };
    let uid8: String = m.uid.chars().take(8).collect();
    Some((format!("- {text}{seen} [{uid8}]"), text))
}

/// Ranks `memories` and greedily fills `budget` estimated tokens, skipping
/// any memory that doesn't fit so smaller ones can still be added. The
/// header's cost counts toward the budget. `query_hits` maps uid to
/// normalized relevance (see [`query_boosts`]).
#[must_use]
#[allow(clippy::implicit_hasher)] // callers always use the default hasher
pub fn build(
    memories: &[Memory],
    budget: usize,
    project_label: &str,
    query_hits: &HashMap<String, f64>,
    now_ms: i64,
) -> Pack {
    let label = one_line(project_label, MAX_LINE_CHARS);
    let empty = || Pack {
        markdown: String::new(),
        json: pack_json(&label, budget, 0, &[]),
        est_tokens: 0,
        included_uids: Vec::new(),
    };

    // The header is sized with the largest number it can show, so filling in
    // the real estimate later can only make it shorter.
    let mut cost = Cost::default();
    cost.add(header(&label, budget, budget).len());
    if cost.est() > budget {
        return empty();
    }

    let mut ranked: Vec<(f64, &Memory)> = memories
        .iter()
        .map(|m| (score(m, now_ms, query_hits.get(&m.uid).copied()), m))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.uid.cmp(&b.1.uid)));

    let mut sections: [Vec<String>; TITLES.len()] = Default::default();
    let mut included: Vec<(&Memory, String)> = Vec::new();
    for (_, m) in ranked {
        let Some((line, text)) = item(m) else {
            continue;
        };
        let s = section(m);
        if sections[s].len() >= SECTION_CAP[s] {
            continue;
        }
        let mut next = cost;
        if sections[s].is_empty() {
            next.add("## ".len() + TITLES[s].len());
        }
        next.add(line.len());
        if next.est() <= budget {
            cost = next;
            sections[s].push(line);
            included.push((m, text));
        }
    }
    if included.is_empty() {
        return empty();
    }

    let render = |est: usize| {
        let mut lines = vec![header(&label, est, budget)];
        for (title, items) in TITLES.iter().zip(&sections).filter(|(_, i)| !i.is_empty()) {
            lines.push(format!("## {title}"));
            lines.extend(items.iter().cloned());
        }
        lines.join("\n")
    };
    // Show the true estimate: re-render until the header's number is the
    // estimate of the text containing it (digits only shrink, so this ends
    // quickly). The planned estimate is a safe upper bound if it doesn't.
    let (mut shown, mut markdown) = (cost.est(), String::new());
    for _ in 0..8 {
        markdown = render(shown);
        let actual = est_tokens(&markdown);
        if actual == shown {
            break;
        }
        shown = actual;
    }
    if est_tokens(&markdown) != shown {
        shown = cost.est();
        markdown = render(shown);
    }

    Pack {
        json: pack_json(&label, budget, shown, &included),
        markdown,
        est_tokens: shown,
        included_uids: included.iter().map(|(m, _)| m.uid.clone()).collect(),
    }
}

fn pack_json(label: &str, budget: usize, est: usize, included: &[(&Memory, String)]) -> Value {
    let memories: Vec<Value> = included
        .iter()
        .map(|(m, text)| {
            json!({
                "uid": m.uid,
                "kind": m.kind.to_string(),
                "text": text,
                "confidence": (f64::from(m.confidence) * 100.0).round() / 100.0,
                "seen_count": m.seen_count,
                "status": m.status.to_string(),
            })
        })
        .collect();
    json!({"project": label, "budget": budget, "est_tokens": est, "memories": memories})
}

#[cfg(test)]
mod tests {

    #[test]
    fn one_line_strips_control_chars() {
        assert_eq!(one_line("a\u{1b}[31mred\u{7}\nb", 50), "a[31mred b");
        assert_eq!(one_line("x\u{85}y", 50), "x y");
    }

    #[test]
    fn sections_are_capped() {
        let mems: Vec<Memory> = (0..20)
            .map(|i| {
                let mut m = mem(
                    &format!("{i:02x}"),
                    Kind::Procedure,
                    &format!("`cargo test case{i}`"),
                );
                m.seen_count = 20 - i;
                m
            })
            .collect();
        let p = build(&mems, 4000, "x", &HashMap::new(), NOW);
        assert_eq!(p.included_uids.len(), SECTION_CAP[3]);
        // The cap keeps the most-seen ones.
        assert!(p.markdown.contains("case0") && !p.markdown.contains("case19"));
    }

    use super::*;
    use crate::model::Scope;

    const NOW: i64 = 1_000_000_000_000;
    const DAY: i64 = 86_400_000;

    fn mem(uid: &str, kind: Kind, text: &str) -> Memory {
        Memory {
            uid: format!("{uid:0<32}"),
            kind,
            text: text.into(),
            scope: Scope::Global,
            confidence: 0.8,
            importance: 1,
            status: Status::Active,
            superseded_by: None,
            seen_count: 1,
            created_at: NOW,
            updated_at: NOW,
            valid_from: NOW,
            valid_to: None,
            provenance: Vec::new(),
            anchors: Vec::new(),
        }
    }

    fn pack(ms: &[Memory], budget: usize) -> Pack {
        build(ms, budget, "demo", &HashMap::new(), NOW)
    }

    #[test]
    fn estimator_counts_bytes_and_lines() {
        assert_eq!(est_tokens(""), 2);
        assert_eq!(est_tokens("abc"), 3);
        assert_eq!(est_tokens("abcd"), 4);
        assert_eq!(est_tokens("a\nb"), 1 + 4);
        assert_eq!(est_tokens("日本"), 2 + 2);
    }

    #[test]
    fn cost_matches_the_estimator_exactly() {
        let mut c = Cost::default();
        let lines = ["# head", "## Facts", "- 日本語 [abc]"];
        for l in lines {
            c.add(l.len());
        }
        assert_eq!(c.est(), est_tokens(&lines.join("\n")));
    }

    #[test]
    fn score_orders_by_kind_confidence_seen_recency_and_boost() {
        let pref = mem("a", Kind::Preference, "p");
        let fact = mem("b", Kind::Fact, "f");
        assert!(score(&pref, NOW, None) > score(&fact, NOW, None));

        let low = Memory {
            confidence: 0.2,
            ..pref.clone()
        };
        assert!(score(&low, NOW, None) < score(&fact, NOW, None));

        let seen = Memory {
            seen_count: 10,
            ..fact.clone()
        };
        assert!(score(&seen, NOW, None) > score(&fact, NOW, None));

        let old = Memory {
            updated_at: NOW - 900 * DAY,
            ..fact.clone()
        };
        let (new, aged) = (score(&fact, NOW, None), score(&old, NOW, None));
        assert!(aged < new && aged > new * 0.49);

        assert!(score(&fact, NOW, Some(0.0)) > score(&fact, NOW, None));
        assert!(score(&fact, NOW, Some(1.0)) > score(&fact, NOW, Some(0.0)));
        // A future updated_at doesn't boost beyond "fresh".
        let future = Memory {
            updated_at: NOW + DAY,
            ..fact.clone()
        };
        assert!((score(&future, NOW, None) - new).abs() < 1e-12);
    }

    #[test]
    fn boosts_normalize_to_unit_range() {
        let b = query_boosts(&[("a".into(), 3.0), ("b".into(), 1.0), ("c".into(), 2.0)]);
        assert_eq!((b["a"], b["b"], b["c"]), (1.0, 0.0, 0.5));
        assert_eq!(query_boosts(&[("x".into(), 7.0)])["x"], 1.0);
        assert!(query_boosts(&[]).is_empty());
    }

    #[test]
    fn sections_are_in_fixed_order_and_empty_ones_omitted() {
        let mut stale = mem("9", Kind::Preference, "stale pref");
        stale.status = Status::NeedsReverify;
        let mut proc = mem("5", Kind::Procedure, "`make check`");
        proc.seen_count = 15;
        let ms = [
            mem("1", Kind::Fact, "a fact"),
            stale,
            mem("2", Kind::Gotcha, "a gotcha"),
            proc,
            mem("3", Kind::Preference, "a pref"),
            mem("4", Kind::Task, "a task"),
            mem("6", Kind::Decision, "a decision"),
        ];
        let p = pack(&ms, 4000);
        let heads: Vec<&str> = p
            .markdown
            .lines()
            .filter(|l| l.starts_with("## "))
            .collect();
        assert_eq!(
            heads,
            [
                "## Preferences",
                "## Gotchas",
                "## Decisions",
                "## Procedures",
                "## Tasks",
                "## Facts",
                "## Needs re-verify ⚠"
            ]
        );
        assert!(p.markdown.contains("- `make check` (15×) [50000000]"));
        assert!(p.markdown.ends_with("- stale pref [90000000]"));
        let only_fact = pack(&ms[..1], 4000);
        assert_eq!(only_fact.markdown.matches("## ").count(), 1);
        assert_eq!(p.json["memories"].as_array().unwrap().len(), 7);
    }

    #[test]
    fn header_reports_the_true_estimate() {
        let p = pack(&[mem("1", Kind::Fact, "one")], 1500);
        assert_eq!(p.est_tokens, est_tokens(&p.markdown));
        let first = p.markdown.lines().next().unwrap();
        assert_eq!(
            first,
            format!(
                "# Recalla memory · project demo · {}/1500 tok",
                p.est_tokens
            )
        );
        assert_eq!(p.json["est_tokens"], p.est_tokens);
        assert_eq!(p.json["memories"][0]["confidence"], 0.8);
    }

    #[test]
    fn long_multiline_text_becomes_one_capped_line() {
        let text = format!("first\nsecond\t{}", "x".repeat(500));
        let p = pack(&[mem("1", Kind::Fact, &text)], 4000);
        let line = p.markdown.lines().nth(2).unwrap();
        assert!(line.starts_with("- first second xxx"));
        assert!(line.contains("x… ["), "{line}");
        assert_eq!(p.markdown.lines().count(), 3);
    }

    #[test]
    fn greedy_skips_what_does_not_fit_but_keeps_going() {
        let big = mem("1", Kind::Preference, &"word ".repeat(100));
        let small = mem("2", Kind::Fact, "tiny");
        let p = pack(&[big, small.clone()], 70);
        assert_eq!(p.included_uids, [small.uid]);
        assert!(p.est_tokens <= 70);
    }

    #[test]
    fn nothing_fits_or_nothing_stored_gives_empty_output() {
        let m = [mem("1", Kind::Fact, "something")];
        for p in [pack(&m, 5), pack(&m, 0), pack(&[], 1500), pack(&m, 30)] {
            assert_eq!((p.markdown.as_str(), p.est_tokens), ("", 0));
            assert!(p.included_uids.is_empty());
            assert_eq!(p.json["memories"].as_array().unwrap().len(), 0);
        }
        // Whitespace-only text has nothing to show.
        assert_eq!(pack(&[mem("1", Kind::Fact, " \n ")], 1500).markdown, "");
    }

    #[test]
    fn ties_break_on_uid_and_query_boost_lifts_a_hit() {
        let (a, b) = (mem("a", Kind::Fact, "alpha"), mem("b", Kind::Fact, "beta"));
        let p = pack(&[b.clone(), a.clone()], 4000);
        assert_eq!(p.included_uids, [a.uid.clone(), b.uid.clone()]);
        let hits = HashMap::from([(b.uid.clone(), 0.0)]);
        let p = build(&[a.clone(), b.clone()], 4000, "demo", &hits, NOW);
        assert_eq!(p.included_uids, [b.uid, a.uid]);
    }

    #[test]
    fn label_newlines_cannot_break_the_header() {
        let p = build(
            &[mem("1", Kind::Fact, "x")],
            4000,
            "a\nb",
            &HashMap::new(),
            NOW,
        );
        assert!(p.markdown.starts_with("# Recalla memory · project a b · "));
    }
}
