//! Context packs: rank memories and fill a token budget. Pure, no I/O.
//!
//! The budget is enforced on [`est_tokens`], a deliberate overestimate
//! (D-016), so the real token count is always lower.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::model::{Kind, Memory, Status};

/// Longest memory text (in chars) shown on one pack line.
const MAX_LINE_CHARS: usize = 300;
/// Longest stale reason (in chars) appended to a re-verify line.
const MAX_REASON_CHARS: usize = 80;
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
    /// Every ranked candidate with its score parts and outcome, best first.
    pub decisions: Vec<Decision>,
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
    ScoreParts::of(m, now_ms, query_boost).total()
}

/// The factors of [`score`], for `pack --explain`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreParts {
    pub kind_weight: f64,
    pub confidence: f64,
    /// `1 + ln(seen_count)`.
    pub seen: f64,
    pub recency: f64,
    /// `1.5 + boost` for a query hit, 1 otherwise.
    pub query: f64,
}

impl ScoreParts {
    fn of(m: &Memory, now_ms: i64, query_boost: Option<f64>) -> Self {
        #[allow(clippy::cast_precision_loss)] // ms timestamps are far below 2^52
        let age_days = (now_ms - m.updated_at).max(0) as f64 / MS_PER_DAY;
        Self {
            kind_weight: kind_weight(m.kind),
            confidence: f64::from(m.confidence),
            seen: f64::from(m.seen_count.max(1)).ln() + 1.0,
            recency: 0.5 + 0.5 * (-age_days / RECENCY_DAYS).exp(),
            query: query_boost.map_or(1.0, |q| 1.5 + q.clamp(0.0, 1.0)),
        }
    }

    fn total(self) -> f64 {
        self.kind_weight * self.confidence * self.seen * self.recency * self.query
    }
}

/// Why a candidate is not in the pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// No visible text after sanitizing.
    NoText,
    /// Its section already holds the maximum number of items.
    SectionCap,
    /// It would not fit the remaining token budget.
    Budget,
}

impl Dropped {
    const fn label(self) -> &'static str {
        match self {
            Self::NoText => "no visible text",
            Self::SectionCap => "section cap",
            Self::Budget => "over budget",
        }
    }
}

/// One ranked candidate and what happened to it (`pack --explain`).
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub uid: String,
    pub kind: Kind,
    pub section: &'static str,
    /// `active` or `needs_reverify` (the anchored code changed).
    pub status: Status,
    pub parts: ScoreParts,
    pub score: f64,
    /// `None`: included.
    pub dropped: Option<Dropped>,
}

impl Decision {
    fn json(&self) -> Value {
        let r = |x: f64| (x * 1000.0).round() / 1000.0;
        json!({
            "uid": self.uid, "kind": self.kind.to_string(), "section": self.section,
            "status": self.status.to_string(), "score": r(self.score),
            "parts": {
                "kind_weight": r(self.parts.kind_weight), "confidence": r(self.parts.confidence),
                "seen": r(self.parts.seen), "recency": r(self.parts.recency), "query": r(self.parts.query),
            },
            "included": self.dropped.is_none(),
            "dropped": self.dropped.map(Dropped::label),
        })
    }
}

/// Renders decisions as a plain table, best ranked first.
#[must_use]
pub fn explain_text(decisions: &[Decision]) -> String {
    let mut out = String::from(
        "uid       kind        status          score  kind×conf×seen×recency×query  outcome\n",
    );
    for d in decisions {
        let uid8: String = d.uid.chars().take(8).collect();
        let p = d.parts;
        let _ = writeln!(
            out,
            "{uid8:<9} {:<11} {:<14} {:>6.3}  {:.2}×{:.2}×{:.2}×{:.2}×{:.2}  {}",
            d.kind.to_string(),
            d.status.to_string(),
            d.score,
            p.kind_weight,
            p.confidence,
            p.seen,
            p.recency,
            p.query,
            d.dropped.map_or("included", Dropped::label),
        );
    }
    out
}

/// The explain table as JSON.
#[must_use]
pub fn explain_json(decisions: &[Decision]) -> Value {
    Value::Array(decisions.iter().map(Decision::json).collect())
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

/// Hits scoring under this share of the best hit are too weak to spend budget on.
const HIT_CUTOFF: f64 = 0.25;
/// Word overlap at which two memories count as the same statement.
const NEAR_DUPLICATE: f64 = 0.85;

fn word_set(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect()
}

fn overlap(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        a.intersection(b).count() as f64 / union as f64
    }
}

/// Narrows a pack to what a task needs. `hits` are the memories matching the
/// task text (BM25 score, higher better) and `file_uids` those anchored to a
/// file the task names. Keeps file matches and hits scoring at least
/// `HIT_CUTOFF` of the best, drops near-duplicates, and always keeps
/// preferences (cheap and always relevant). File matches get the top boost.
/// With no usable hit the memories come back unchanged and unboosted, so a
/// vague query never empties a pack.
#[must_use]
#[allow(clippy::implicit_hasher)] // callers always use the default hasher
pub fn focus(
    memories: &[Memory],
    hits: &[(String, f64)],
    file_uids: &std::collections::HashSet<String>,
) -> (Vec<Memory>, HashMap<String, f64>) {
    let best = hits.iter().map(|h| h.1).fold(f64::NEG_INFINITY, f64::max);
    let strong: Vec<(String, f64)> = hits
        .iter()
        .filter(|h| h.1 >= best * HIT_CUTOFF)
        .cloned()
        .collect();
    if strong.is_empty() && file_uids.is_empty() {
        return (memories.to_vec(), HashMap::new());
    }
    let mut boosts = query_boosts(&strong);
    for uid in file_uids {
        boosts.insert(uid.clone(), 1.0);
    }
    let mut relevant: Vec<&Memory> = memories
        .iter()
        .filter(|m| boosts.contains_key(&m.uid))
        .collect();
    relevant.sort_by(|a, b| {
        boosts[&b.uid]
            .total_cmp(&boosts[&a.uid])
            .then_with(|| a.uid.cmp(&b.uid))
    });
    let mut kept: Vec<(&Memory, std::collections::HashSet<String>)> = Vec::new();
    for m in relevant {
        let w = word_set(&m.text);
        if kept.iter().all(|(_, k)| overlap(&w, k) < NEAR_DUPLICATE) {
            kept.push((m, w));
        }
    }
    let keep: std::collections::HashSet<&str> = kept.iter().map(|(m, _)| m.uid.as_str()).collect();
    let focused = memories
        .iter()
        .filter(|m| keep.contains(m.uid.as_str()) || m.kind == Kind::Preference)
        .cloned()
        .collect();
    (focused, boosts)
}

/// Whitespace collapsed to single spaces, cut to `max` chars with `…`.
/// Collapse `text` to one line of at most `max` chars, for packs and terminal
/// output. Whitespace runs become one space and control chars (ESC, C0/C1) are
/// dropped, so memory text can't fake structure or emit terminal escapes.
#[must_use]
pub fn one_line(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .filter(|c| c.is_whitespace() || !(c.is_control() || is_invisible(*c)))
        .collect();
    let flat = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Invisible or blank-rendering format characters that can hide text from a
/// human while a model still reads it: soft hyphen, combining grapheme joiner,
/// Arabic letter mark, Hangul and other fillers, Mongolian vowel separator,
/// zero-width and bidi marks/overrides/isolates (ZWJ included: packs favor
/// safety over emoji fidelity), word joiners, variation selectors, BOM,
/// interlinear annotation marks and the Unicode tag block (ASCII smuggling).
#[must_use]
pub fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{AD}' | '\u{34F}' | '\u{61C}' | '\u{115F}' | '\u{1160}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
        | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFB}' | '\u{E0000}'..='\u{E007F}'
        | '\u{E0100}'..='\u{E01EF}')
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

/// `- text [uid8]`, with ` (N×)` for procedures seen more than once and
/// ` (reason)` for a stale memory.
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
    let reason = m
        .stale_reason
        .as_deref()
        .map(|r| one_line(r, MAX_REASON_CHARS))
        .filter(|r| !r.is_empty() && m.status == Status::NeedsReverify)
        .map_or_else(String::new, |r| format!(" ({r})"));
    let uid8: String = m.uid.chars().take(8).collect();
    Some((format!("- {text}{seen}{reason} [{uid8}]"), text))
}

/// The "dump everything" baseline: estimated tokens if every candidate were
/// injected. It is the sum of [`est_tokens`] of each candidate's rendered
/// item line (the same line [`build`] emits, so uncapped by section caps and
/// budget; memories with no visible text are skipped, as in a pack) plus the
/// cost of a header with an empty project label. Section titles are not
/// counted, so the baseline slightly understates the dump.
#[must_use]
pub fn unbounded_tokens(memories: &[Memory]) -> usize {
    let lines: usize = memories
        .iter()
        .filter_map(item)
        .map(|(line, _)| est_tokens(&line))
        .sum();
    lines + est_tokens(&header("", 0, 0))
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
    let empty = |decisions: Vec<Decision>| Pack {
        markdown: String::new(),
        json: pack_json(&label, budget, 0, &[]),
        est_tokens: 0,
        included_uids: Vec::new(),
        decisions,
    };

    // The header is sized with the largest number it can show, so filling in
    // the real estimate later can only make it shorter.
    let mut cost = Cost::default();
    cost.add(header(&label, budget, budget).len());
    if cost.est() > budget {
        return empty(Vec::new());
    }

    let mut ranked: Vec<(ScoreParts, f64, &Memory)> = memories
        .iter()
        .map(|m| {
            let parts = ScoreParts::of(m, now_ms, query_hits.get(&m.uid).copied());
            (parts, parts.total(), m)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.2.uid.cmp(&b.2.uid)));
    let mut decisions: Vec<Decision> = Vec::with_capacity(ranked.len());

    let mut sections: [Vec<String>; TITLES.len()] = Default::default();
    let mut included: Vec<(&Memory, String)> = Vec::new();
    for (parts, score, m) in ranked {
        let s = section(m);
        let mut decision = Decision {
            uid: m.uid.clone(),
            kind: m.kind,
            section: TITLES[s],
            status: m.status,
            parts,
            score,
            dropped: None,
        };
        let Some((line, text)) = item(m) else {
            decision.dropped = Some(Dropped::NoText);
            decisions.push(decision);
            continue;
        };
        if sections[s].len() >= SECTION_CAP[s] {
            decision.dropped = Some(Dropped::SectionCap);
            decisions.push(decision);
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
        } else {
            decision.dropped = Some(Dropped::Budget);
        }
        decisions.push(decision);
    }
    if included.is_empty() {
        return empty(decisions);
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
        decisions,
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
    fn unbounded_tokens_counts_every_item_line_beyond_caps() {
        let mems: Vec<Memory> = (0..20)
            .map(|i| mem(&format!("{i:02x}"), Kind::Fact, &format!("fact number {i}")))
            .collect();
        let built = build(&mems, 4000, "p", &HashMap::new(), NOW);
        assert_eq!(built.included_uids.len(), SECTION_CAP[5]);
        let all = unbounded_tokens(&mems);
        assert!(all > built.est_tokens, "{all} vs {}", built.est_tokens);
        let one = unbounded_tokens(&mems[..1]);
        let line = item(&mems[0]).unwrap().0;
        assert_eq!(one, est_tokens(&line) + est_tokens(&header("", 0, 0)));
        assert_eq!(unbounded_tokens(&[]), est_tokens(&header("", 0, 0)));
    }

    #[test]
    fn one_line_strips_control_chars() {
        assert_eq!(one_line("a\u{1b}[31mred\u{7}\nb", 50), "a[31mred b");
        assert_eq!(one_line("x\u{85}y", 50), "x y");
    }

    #[test]
    fn one_line_strips_tag_block_and_fillers_but_keeps_real_text() {
        let tagged: String = "ignore"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        assert_eq!(one_line(&tagged, 50), "");
        assert_eq!(one_line(&format!("vis{tagged}ible"), 50), "visible");
        let fillers =
            "a\u{61C}\u{180E}\u{FFF9}\u{FE0F}\u{E0100}\u{34F}\u{115F}\u{1160}\u{3164}\u{FFA0}b";
        assert_eq!(one_line(fillers, 50), "ab");
        let plain = "日本語 café naïve 😀 ✓";
        assert_eq!(one_line(plain, 50), plain);
    }

    #[test]
    fn one_line_strips_invisible_format_chars() {
        let dirty =
            "a\u{AD}b\u{200B}c\u{200F}d\u{202E}e\u{2060}f\u{2064}g\u{2066}h\u{2069}i\u{FEFF}j";
        assert_eq!(one_line(dirty, 50), "abcdefghij");
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
            stale_reason: None,
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
    fn stale_reason_is_shown_and_counted_in_the_budget() {
        let mut stale = mem("9", Kind::Fact, "insert is cheap");
        stale.status = Status::NeedsReverify;
        stale.stale_reason = Some("symbol changed: Store::insert\nx".into());
        let p = pack(&[stale.clone()], 4000);
        assert!(
            p.markdown
                .ends_with("- insert is cheap (symbol changed: Store::insert x) [90000000]"),
            "{}",
            p.markdown
        );
        assert_eq!(p.est_tokens, est_tokens(&p.markdown));
        // A reason on an active memory is ignored.
        stale.status = Status::Active;
        assert!(!pack(&[stale], 4000).markdown.contains("symbol changed"));
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

    #[test]
    fn focus_drops_weak_hits_duplicates_and_non_hits_but_keeps_preferences() {
        use std::collections::HashSet;
        let ms = vec![
            mem(
                "a",
                Kind::Gotcha,
                "glob builder drops separators when empty",
            ),
            mem(
                "b",
                Kind::Gotcha,
                "glob builder drops separators when empty!",
            ),
            mem("c", Kind::Gotcha, "weak partial match about glob"),
            mem("d", Kind::Gotcha, "tokio runtime shutdown ordering"),
            mem("e", Kind::Preference, "use tabs"),
            mem("f", Kind::Fact, "anchored to the named file only"),
        ];
        let id = |c: &str| format!("{c:0<32}");
        let hits = vec![(id("a"), 10.0), (id("b"), 9.0), (id("c"), 1.0)];
        let files: HashSet<String> = [id("f")].into();
        let (kept, boosts) = focus(&ms, &hits, &files);
        let uids: Vec<String> = kept.iter().map(|m| m.uid.clone()).collect();
        // c is under the cutoff, b duplicates a, d never matched.
        assert_eq!(uids, vec![id("a"), id("e"), id("f")]);
        assert!((boosts[&id("f")] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn focus_with_no_signal_returns_everything_unboosted() {
        let ms = vec![mem("a", Kind::Fact, "one"), mem("d", Kind::Fact, "two")];
        let (kept, boosts) = focus(&ms, &[], &std::collections::HashSet::new());
        assert_eq!(kept.len(), 2);
        assert!(boosts.is_empty());
    }

    #[test]
    fn decisions_cover_every_candidate_with_outcome_and_matching_score() {
        let mut ms: Vec<Memory> = (0..12)
            .map(|i| mem(&format!("{i:02x}"), Kind::Task, &format!("task number {i}")))
            .collect();
        ms.push(mem("f0", Kind::Fact, "\u{200B}"));
        ms.push(mem("f1", Kind::Preference, "use tabs"));
        let p = build(&ms, 1500, "x", &HashMap::new(), NOW);
        assert_eq!(p.decisions.len(), ms.len());
        let dropped = |d: Dropped| p.decisions.iter().filter(|x| x.dropped == Some(d)).count();
        assert_eq!(dropped(Dropped::NoText), 1);
        assert_eq!(dropped(Dropped::SectionCap), 4); // 12 tasks, cap 8
        let included = p.decisions.iter().filter(|d| d.dropped.is_none()).count();
        assert_eq!(included, p.included_uids.len());
        // Sorted best first; the stored score equals the product of the parts.
        let scores: Vec<f64> = p.decisions.iter().map(|d| d.score).collect();
        assert!(scores.windows(2).all(|w| w[0] >= w[1]));
        let d = &p.decisions[0];
        let prod = d.parts.kind_weight
            * d.parts.confidence
            * d.parts.seen
            * d.parts.recency
            * d.parts.query;
        assert!((prod - d.score).abs() < 1e-12);
    }

    #[test]
    fn tight_budget_marks_the_rest_over_budget_and_query_part_shows() {
        let ms = vec![
            mem("a", Kind::Fact, "alpha fact that is fairly long text here"),
            mem("b", Kind::Fact, "beta fact that is fairly long text here"),
        ];
        let hits: HashMap<String, f64> = [(format!("{:0<32}", "b"), 1.0)].into();
        let p = build(&ms, 45, "x", &hits, NOW);
        let by = |c: &str| {
            p.decisions
                .iter()
                .find(|d| d.uid.starts_with(c))
                .unwrap()
                .clone()
        };
        assert!((by("b").parts.query - 2.5).abs() < 1e-9);
        assert!((by("a").parts.query - 1.0).abs() < 1e-9);
        assert_eq!(by("b").dropped, None);
        assert_eq!(by("a").dropped, Some(Dropped::Budget));
        let text = explain_text(&p.decisions);
        assert!(
            text.contains("over budget") && text.contains("included"),
            "{text}"
        );
    }
}
