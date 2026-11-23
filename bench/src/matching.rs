//! Deciding whether a tool's returned text contains a given fact. Tools may
//! reformat or lightly reword what they store, so matching is by word
//! coverage, not exact text.

use std::collections::HashSet;

/// A fact counts as returned when this share of its words appear in one item.
pub const THRESHOLD: f64 = 0.7;

const STOP: &[&str] = &[
    "the", "and", "that", "this", "with", "for", "are", "was", "were", "not", "but", "has", "have",
    "had", "its", "from", "can", "will", "would", "when", "which", "than", "then", "into", "files",
];

/// Lowercase alphanumeric words of 3+ chars without stop words.
#[must_use]
pub fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .collect()
}

/// Share of `fact`'s words found in `item` (0 when the fact has none).
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn coverage(fact: &HashSet<String>, item: &HashSet<String>) -> f64 {
    if fact.is_empty() {
        return 0.0;
    }
    fact.intersection(item).count() as f64 / fact.len() as f64
}

/// True when some item covers the fact.
#[must_use]
pub fn contains(fact_text: &str, items: &[HashSet<String>]) -> bool {
    let f = words(fact_text);
    items.iter().any(|i| coverage(&f, i) >= THRESHOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_reformatted_text_match() {
        let fact = "The cache was keyed by path because the old key collided.";
        let items = [words(
            "- The cache was keyed by path because the old key collided. [ab12]",
        )];
        assert!(contains(fact, &items));
        let reworded = [words("cache keyed by path: old key collided")];
        assert!(contains(fact, &reworded));
    }

    #[test]
    fn unrelated_or_partial_text_does_not_match() {
        let fact = "The cache was keyed by path because the old key collided.";
        assert!(!contains(fact, &[words("We use tokio for the runtime")]));
        assert!(!contains(fact, &[words("cache path")]));
        assert!(!contains("", &[words("anything")]));
    }
}
