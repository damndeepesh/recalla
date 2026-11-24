//! Contradiction detection between two rule memories (preferences and
//! decisions) of the same scope. Pure and deliberately conservative (D-028):
//! a missed conflict costs nothing, a false one hides a good memory.
//!
//! Two signals: opposed alternatives of one curated group ("tabs" vs
//! "spaces"), and a polarity flip over near-identical wording ("always X" vs
//! "never X").

use std::collections::BTreeSet;

/// Mutually exclusive alternatives. Each alternative is a space-joined token
/// phrase; groups are matched on whole tokens.
const GROUPS: &[&[&str]] = &[
    &["tabs", "spaces"],
    &["wal", "rollback journal", "delete journal"],
    &["npm", "pnpm", "yarn", "bun"],
    &["single quotes", "double quotes"],
    &["camelcase", "snake_case"],
    &["jest", "vitest", "mocha"],
    &["unittest", "pytest"],
    &["postgres", "mysql", "sqlite"],
    &["rest", "graphql", "grpc"],
    &["squash", "rebase"],
];

/// Words that flip a statement's meaning.
const NEGATIONS: &[&str] = &[
    "never", "not", "dont", "avoid", "stop", "without", "no", "cannot", "cant",
];
/// Words that introduce a rejected alternative ("instead of tabs").
const REJECTORS: &[&str] = &[
    "not", "instead", "rather", "over", "than", "dont", "never", "no",
];
/// Wording that carries no subject.
const FILLER: &[&str] = &[
    "a",
    "an",
    "the",
    "to",
    "for",
    "in",
    "of",
    "and",
    "or",
    "we",
    "you",
    "i",
    "it",
    "is",
    "are",
    "be",
    "do",
    "does",
    "always",
    "must",
    "should",
    "please",
    "use",
    "using",
    "from",
    "now",
    "on",
    "longer",
    "going",
    "forward",
    "decision",
    "rule",
    "convention",
    "project",
    "this",
    "that",
    "with",
    "when",
    "all",
    "any",
];
/// Overlap of content words above which two statements are about one thing.
const SAME_SUBJECT: f64 = 0.7;
/// Fewest content words either statement needs for the polarity signal.
const MIN_POLARITY_WORDS: usize = 2;

/// A statement broken into what the detector compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analyzed {
    tokens: Vec<String>,
    negated: bool,
    content: BTreeSet<String>,
}

impl Analyzed {
    #[must_use]
    pub fn new(text: &str) -> Self {
        let tokens: Vec<String> = text
            .to_lowercase()
            .replace(['\'', '’'], "")
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect();
        let negated = tokens.iter().any(|t| NEGATIONS.contains(&t.as_str()));
        let content = tokens
            .iter()
            .filter(|t| !NEGATIONS.contains(&t.as_str()) && !FILLER.contains(&t.as_str()))
            .cloned()
            .collect();
        Self {
            tokens,
            negated,
            content,
        }
    }

    /// Positions where the phrase occurs as whole tokens.
    fn find(&self, phrase: &str) -> Vec<usize> {
        let words: Vec<&str> = phrase.split(' ').collect();
        (0..self.tokens.len())
            .filter(|&i| {
                self.tokens.len() >= i + words.len()
                    && self.tokens[i..i + words.len()]
                        .iter()
                        .map(String::as_str)
                        .eq(words.iter().copied())
            })
            .collect()
    }

    /// Alternatives of `group` that the statement picks and rejects.
    fn picks(&self, group: &[&'static str]) -> (BTreeSet<&'static str>, BTreeSet<&'static str>) {
        let (mut chosen, mut rejected) = (BTreeSet::new(), BTreeSet::new());
        for alt in group {
            for at in self.find(alt) {
                let before = &self.tokens[at.saturating_sub(2)..at];
                if before.iter().any(|t| REJECTORS.contains(&t.as_str())) {
                    rejected.insert(*alt);
                } else {
                    chosen.insert(*alt);
                }
            }
        }
        (chosen, rejected)
    }

    fn group_terms() -> BTreeSet<String> {
        GROUPS
            .iter()
            .flat_map(|g| g.iter())
            .flat_map(|alt| alt.split(' '))
            .map(str::to_owned)
            .collect()
    }
}

/// Why two statements conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// They pick different alternatives of one group.
    Opposed(&'static str, &'static str),
    /// Same wording, opposite polarity.
    Polarity,
}

impl Conflict {
    /// One line for `why` and the review queue.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Opposed(a, b) => format!("opposed choices: {a} vs {b}"),
            Self::Polarity => "same wording, opposite polarity".to_owned(),
        }
    }
}

/// Whether `a` and `b` contradict each other.
#[must_use]
pub fn detect(a: &Analyzed, b: &Analyzed) -> Option<Conflict> {
    opposed(a, b).or_else(|| polarity(a, b))
}

fn rest<'a>(x: &'a Analyzed, terms: &'a BTreeSet<String>) -> BTreeSet<&'a String> {
    x.content.difference(terms).collect()
}

fn opposed(a: &Analyzed, b: &Analyzed) -> Option<Conflict> {
    let terms = Analyzed::group_terms();
    let (ra, rb) = (rest(a, &terms), rest(b, &terms));
    // Same subject unless one side names no subject beyond the choice.
    let same_subject = ra.is_empty() || rb.is_empty() || ra.intersection(&rb).next().is_some();
    if !same_subject {
        return None;
    }
    for group in GROUPS {
        let ((ca, xa), (cb, xb)) = (a.picks(group), b.picks(group));
        let explicit = ca
            .iter()
            .find(|x| xb.contains(*x))
            .or_else(|| cb.iter().find(|x| xa.contains(*x)));
        if let Some(rejected) = explicit {
            let other = ca
                .iter()
                .chain(&cb)
                .find(|x| *x != rejected)
                .unwrap_or(rejected);
            return Some(Conflict::Opposed(rejected, other));
        }
        if let (Some(x), Some(y)) = (ca.iter().next(), cb.iter().next())
            && ca.is_disjoint(&cb)
        {
            return Some(Conflict::Opposed(x, y));
        }
    }
    None
}

fn polarity(a: &Analyzed, b: &Analyzed) -> Option<Conflict> {
    if a.negated == b.negated
        || a.content.len() < MIN_POLARITY_WORDS
        || b.content.len() < MIN_POLARITY_WORDS
    {
        return None;
    }
    let shared = a.content.intersection(&b.content).count();
    let union = a.content.union(&b.content).count();
    #[allow(clippy::cast_precision_loss)] // word counts are tiny
    let jaccard = shared as f64 / union as f64;
    (jaccard >= SAME_SUBJECT).then_some(Conflict::Polarity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(a: &str, b: &str) -> Option<Conflict> {
        detect(&Analyzed::new(a), &Analyzed::new(b))
    }

    #[test]
    fn opposed_alternatives_conflict() {
        assert!(d("Use tabs for indentation", "Use spaces for indentation").is_some());
        assert!(
            d(
                "Use WAL mode for SQLite",
                "Use rollback journal mode for SQLite"
            )
            .is_some()
        );
        assert!(d("Use pnpm in this repo", "Use yarn in this repo").is_some());
    }

    #[test]
    fn an_explicit_rejection_conflicts() {
        assert!(
            d(
                "Use spaces, not tabs, for indentation",
                "Use tabs for indentation"
            )
            .is_some()
        );
        assert!(d("Use pnpm instead of npm", "Always use npm").is_some());
    }

    #[test]
    fn same_choice_or_different_subjects_do_not_conflict() {
        assert!(d("Use tabs for indentation", "Use tabs in Makefiles").is_none());
        assert!(d("Use pnpm for the frontend", "Use npm for the backend").is_none());
        assert!(d("We use pnpm; npm is slow", "Use pnpm").is_none());
        assert!(d("Use spaces, not tabs", "Use spaces for indentation").is_none());
    }

    #[test]
    fn polarity_flip_over_same_wording_conflicts() {
        assert!(
            d(
                "Always squash commits before merging",
                "Never squash commits before merging"
            )
            .is_some()
        );
        assert!(d("Log request bodies", "Don't log request bodies").is_some());
    }

    #[test]
    fn negation_with_different_subject_does_not_conflict() {
        assert!(d("Never log request bodies", "Always log request ids").is_none());
        assert!(d("Don't commit generated files", "Commit lockfiles").is_none());
        assert!(d("Never push", "Always push").is_none()); // too little to judge
    }
}
