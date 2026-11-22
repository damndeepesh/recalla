//! Facts: very narrow statements of where something lives or what we use.
//!
//! User text only, precision over recall. Two shapes:
//! - `[the] <noun phrase, <=4 words> is|are on|in|at <version or path>`
//! - `we use <X> for <Y>`
//!
//! Sentences a preference rule would take (directive markers) are skipped.

use std::sync::LazyLock;

use regex::Regex;

use super::anchors_in;
use super::preference::is_directive;
use crate::extract::scope::project_scope;
use crate::extract::text::{is_question, is_quoted, path_tokens, sentences, strip_code};
use crate::extract::{Candidate, EventCtx};
use crate::model::Kind;
use crate::store::StoredKind;

const CONFIDENCE: f32 = 0.5;
const IMPORTANCE: u8 = 0;

static LOCATED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:the\s+)?(?P<noun>[\w.\-]+(?:\s+[\w.\-]+){0,3})\s+(?:is|are)\s+(?:on|in|at)\s+(?P<rest>\S.*)$",
    )
    .expect("valid regex")
});

static USES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^we use\s+(?P<what>[^,;]+?)\s+for\s+(?P<why>\S.*)$").expect("valid regex")
});

/// A name followed by a number (`Node 20`, `python 3.12`) or a dotted version.
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[a-z][\w+-]*\s+v?\d+(?:\.\d+)*\b|\bv?\d+\.\d+(?:\.\d+)*\b")
        .expect("valid regex")
});

/// Absolute, home or dot-relative path, or a multi-segment path.
static SLASH_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s`(])(?:~|\.{0,2})/[\w.\-]+|\b[\w.\-]+(?:/[\w.\-]+){2,}")
        .expect("valid regex")
});

/// First words that make the "noun phrase" a pronoun or clause, not a thing.
const NOT_NOUNS: &[&str] = &[
    "i",
    "you",
    "we",
    "they",
    "he",
    "she",
    "it",
    "this",
    "that",
    "these",
    "those",
    "there",
    "here",
    "what",
    "who",
    "which",
    "where",
    "when",
    "how",
    "why",
    "everything",
    "something",
    "nothing",
];
const CLAUSE_WORDS: &[&str] = &["that", "which", "who", "not", "and", "or", "but", "if"];
const PRONOUN_OBJECTS: &[&str] = &["it", "this", "that", "them", "these", "those", "everything"];

/// Fact candidates from one human user-text event (not a subagent prompt).
#[must_use]
pub fn extract(ctx: &EventCtx<'_>, project: Option<&str>) -> Vec<Candidate> {
    if ctx.event.kind != StoredKind::UserText || ctx.event.sidechain {
        return Vec::new();
    }
    let clean = strip_code(&ctx.event.text);
    sentences(&clean)
        .into_iter()
        .filter(|s| is_fact(s))
        .map(|s| Candidate {
            kind: Kind::Fact,
            text: s.to_owned(),
            confidence: CONFIDENCE,
            importance: IMPORTANCE,
            scope: project_scope(project),
            anchors: anchors_in(s),
        })
        .collect()
}

fn is_fact(s: &str) -> bool {
    if is_quoted(s, 0) || is_question(s) || is_directive(s) {
        return false;
    }
    located(s) || uses(s)
}

fn words(phrase: &str) -> impl Iterator<Item = String> + '_ {
    phrase.split_whitespace().map(str::to_ascii_lowercase)
}

fn located(s: &str) -> bool {
    let Some(c) = LOCATED.captures(s) else {
        return false;
    };
    let mut noun = words(&c["noun"]);
    let first = noun.next().unwrap_or_default();
    if NOT_NOUNS.contains(&first.as_str()) || noun.any(|w| CLAUSE_WORDS.contains(&w.as_str())) {
        return false;
    }
    let rest = &c["rest"];
    VERSION.is_match(rest) || SLASH_PATH.is_match(rest) || !path_tokens(rest).is_empty()
}

fn uses(s: &str) -> bool {
    let Some(c) = USES.captures(s) else {
        return false;
    };
    let what = &c["what"];
    !PRONOUN_OBJECTS.contains(&what.to_ascii_lowercase().as_str())
        && what.split_whitespace().count() <= 4
        && !c["why"].trim_end_matches(['.', '!']).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::testutil::event;
    use crate::model::Scope;

    fn run(kind: StoredKind, text: &str) -> Vec<Candidate> {
        let e = event(kind, text);
        extract(
            &EventCtx {
                event: &e,
                prev: &[],
            },
            Some("/work/app"),
        )
    }

    fn user(text: &str) -> Vec<Candidate> {
        run(StoredKind::UserText, text)
    }

    #[test]
    fn positives() {
        for s in [
            "The API is on Node 20 in production.",
            "Configs are in /etc/app for every host.",
            "The build image is on python 3.12 for now.",
            "Our database server is on Postgres 16 today.",
            "The deploy scripts are in ./scripts/deploy and nowhere else.",
            "Fixtures are in tests/fixtures/extract for the eval.",
            "The frontend is at v2.3.1 right now.",
            "The main entrypoint is in src/main.rs for the cli.",
            "The cache dir is in ~/.cache/recalla by default.",
            "We use SQLite for local storage of every memory.",
            "We use Redis for the job queue in staging.",
            "We use gitleaks for secret scanning in CI.",
        ] {
            let got = user(s);
            assert_eq!(got.len(), 1, "{s}");
            assert_eq!(got[0].kind, Kind::Fact);
            assert_eq!(got[0].importance, 0);
            assert!((got[0].confidence - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn negatives() {
        for s in [
            "The weather is in a bad mood again today.",
            "My manager is in a meeting until later today.",
            "It is in Node 20 for some reason I think.",
            "I think the API is on Node 20 but not sure.",
            "That is on version 3 of the old plan, right.",
            "Always keep the API on Node 20 in production.",
            "Never put configs in /etc/app for every host.",
            "Remember that the API is on Node 20 in production.",
            "We use it for testing things around the office.",
            "We use tabs.",
            "The problem that we have is in src/main.rs somewhere.",
            "Tests are in a good place right now overall.",
            "\"The API is on Node 20\" was in the old readme.",
            "> The API is on Node 20 in production today",
            "Prefer that we use pnpm for the whole workspace.",
            "Is the API on Node 20 in production right now",
            "Where are the configs in /etc/app for every host",
            "The bug is in the parser module and needs a fix.",
            "We used Redis for the queue until last year.",
            "The fix is on its way and should land soon, I hope.",
        ] {
            assert!(user(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn only_user_text_counts() {
        let s = "The API is on Node 20 in production.";
        for k in [
            StoredKind::AssistantText,
            StoredKind::ToolCall,
            StoredKind::ToolResult,
            StoredKind::Summary,
        ] {
            assert!(run(k, s).is_empty());
        }
    }

    #[test]
    fn sidechain_user_text_is_skipped() {
        let mut e = event(StoredKind::UserText, "The API is on Node 20 in production.");
        let ctx = |e: &_| {
            extract(
                &EventCtx {
                    event: e,
                    prev: &[],
                },
                None,
            )
        };
        assert_eq!(ctx(&e).len(), 1);
        e.sidechain = true;
        assert!(ctx(&e).is_empty());
    }

    #[test]
    fn code_blocks_and_questions_are_ignored() {
        assert!(user("```\nThe API is on Node 20 in production.\n```").is_empty());
        assert!(user("Is the API on Node 20 in production?").is_empty());
    }

    #[test]
    fn scope_and_anchors() {
        let g = &user("The entrypoint is in src/main.rs for the cli.")[0];
        assert_eq!(g.scope, Scope::Project("/work/app".into()));
        assert_eq!(g.anchors[0].path, "src/main.rs");
    }
}
