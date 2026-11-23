//! Preferences: standing directives the user gives ("always X", "never Y").
//!
//! User text only. A directive marker counts only at the start of a clause
//! (sentence start, or after a comma/semicolon), which keeps narration such
//! as "I always forget" out.

use std::sync::LazyLock;

use regex::Regex;

use super::anchors_in;
use crate::extract::scope::preference_scope;
use crate::extract::text::{is_question, is_quoted, sentences, strip_code};
use crate::extract::{Candidate, EventCtx};
use crate::model::Kind;
use crate::store::StoredKind;

const START_CONFIDENCE: f32 = 0.85;
const MID_CONFIDENCE: f32 = 0.7;
const IMPORTANCE: u8 = 2;

/// Verbs that make "please X" a standing rule rather than a one-off request.
const PLEASE_VERBS: &str = "use|keep|write|add|run|make|ensure|avoid|stick|follow|prefer|put|name|\
    format|document|commit|store|return|log|handle|validate|install|pin|always|never|do|don'?t";

static FILLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:also|and|but|ok|okay|so|btw|then|yes|yeah)\b[,:]?\s+)+")
        .expect("valid regex")
});

/// A bare imperative ("Use X before any Y", "Keep the binary under 5 MB") is
/// only a standing rule when it carries a scope or limit word; "Use the
/// staging db" alone is a one-off instruction.
const BARE_RULE: &str = r"(?:use|keep|run|write|commit|pin|set|name|put)\s+.+?\b(?:every|all|any|each|before|after|under|below|at\s+least|at\s+most|no\s+more\s+than)\b";

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?is)^(?:always\b|never\b|don['’]?t\b|do not\b|make sure\b|please\s+(?:{PLEASE_VERBS})\b|(?:(?:i|we)\s+)?prefer\b|instead of\b|from now on\b|remember\b|stop\s+\w+ing\b|avoid\b|use\s+.+?,?\s+not\s+\S|{BARE_RULE})"
    ))
    .expect("valid regex")
});

/// Looks like a marker but is not a rule.
static NOT_A_RULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:never\s+mind\b|don['\u{2019}]?t\s+(?:worry|know|think|see|understand|care|mind|bother|get\s+it)\b|remember\s+(?:when|how|what|why|if|where|me)\b)",
    )
    .expect("valid regex")
});

/// First-person past narration ("I tried", "I always wondered").
static NARRATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:i|we)\s+(?:always\s+)?(?:tried|wondered|thought|asked|said|noticed|found|saw|ran|was|were|had|did|used\s+to|never\s+knew)\b|\bi['\u{2019}]ve\s+(?:tried|always|never|been)\b",
    )
    .expect("valid regex")
});

/// "If you ..." style conditionals describe a situation, not a rule.
static CONDITIONAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:if|when|unless)\s+(?:you|i|we|they|he|she)\b").expect("valid regex")
});

/// Preference candidates from one user-text event. Sidechain (subagent)
/// "user" turns are the orchestrator's instructions, not the human's.
#[must_use]
pub fn extract(ctx: &EventCtx<'_>, project: Option<&str>) -> Vec<Candidate> {
    if ctx.event.kind != StoredKind::UserText || ctx.event.sidechain {
        return Vec::new();
    }
    let clean = strip_code(&ctx.event.text);
    sentences(&clean)
        .into_iter()
        .filter_map(|s| {
            let confidence = directive_confidence(s)?;
            Some(Candidate {
                kind: Kind::Preference,
                text: s.to_owned(),
                confidence,
                importance: IMPORTANCE,
                scope: preference_scope(s, project),
                anchors: anchors_in(s),
            })
        })
        .collect()
}

/// True when `sentence` carries a directive marker (a preference).
pub(super) fn is_directive(sentence: &str) -> bool {
    directive_confidence(sentence).is_some()
}

/// Confidence of the first directive marker in `sentence`, or `None`.
fn directive_confidence(sentence: &str) -> Option<f32> {
    if sentence.starts_with(['"', '>', '\u{201c}'])
        || CONDITIONAL.is_match(sentence)
        || is_question(sentence)
    {
        return None;
    }
    if NARRATION.is_match(sentence) {
        return None;
    }
    let body_at = FILLER.find(sentence).map_or(0, |m| m.end());
    let mut clause_start = body_at;
    loop {
        let rest = &sentence[clause_start..];
        let trimmed = rest.trim_start();
        let trimmed = &trimmed[FILLER.find(trimmed).map_or(0, |m| m.end())..];
        let at = clause_start + (rest.len() - trimmed.len());
        if MARKER.is_match(trimmed) && !NOT_A_RULE.is_match(trimmed) && !is_quoted(sentence, at) {
            return Some(if clause_start == body_at {
                START_CONFIDENCE
            } else {
                MID_CONFIDENCE
            });
        }
        let next = trimmed.find([',', ';'])?;
        clause_start = at + next + 1;
    }
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
    fn bare_imperatives_count_only_with_a_scope_or_limit_word() {
        for s in [
            "Use terraform plan before any apply, no exceptions.",
            "Use context deadlines on every outbound HTTP call.",
            "Keep the binary under 5 MB.",
            "Run the linter after all edits.",
        ] {
            assert_eq!(user(s).len(), 1, "should match: {s}");
        }
        for s in [
            "Use the staging database for this task.",
            "Keep going.",
            "Run the tests.",
            "Should we use caching before every request?",
            "If you use the cache before any write it breaks.",
        ] {
            assert!(user(s).is_empty(), "should not match: {s}");
        }
    }

    #[test]
    fn positives_at_sentence_start() {
        for s in [
            "Always run the formatter before committing.",
            "Never commit directly to the main branch.",
            "Don't add comments that restate the code.",
            "Do not mock the database in integration tests.",
            "Make sure the tests pass before you finish.",
            "Please use tabs instead of spaces here.",
            "Prefer small functions over long ones.",
            "Instead of unwrap, return a proper error type.",
            "From now on, keep replies short and plain.",
            "Remember that releases need a changelog entry.",
            "Stop adding trailing summaries to every reply.",
            "Avoid adding new dependencies without a reason.",
            "Use pnpm, not npm, for every install.",
            "I prefer explicit error handling everywhere.",
            "Also, never leave debug prints in committed code.",
        ] {
            let got = user(s);
            assert_eq!(got.len(), 1, "{s}");
            assert_eq!(got[0].kind, Kind::Preference);
            assert_eq!(got[0].importance, 2);
            assert!((got[0].confidence - 0.85).abs() < 1e-6, "{s}");
        }
    }

    #[test]
    fn positives_mid_sentence_get_lower_confidence() {
        for s in [
            "Thanks for that, always run the linter first.",
            "Looks good; never push without a green build.",
            "That works, but don't change the public API.",
            "Fine for now, make sure the docs are updated too.",
        ] {
            let got = user(s);
            assert_eq!(got.len(), 1, "{s}");
            assert!((got[0].confidence - 0.7).abs() < 1e-6, "{s}");
        }
    }

    #[test]
    fn negatives() {
        for s in [
            "Should we always run the formatter before committing?",
            "I tried to never use unwrap but it got messy.",
            "I always wondered why the parser works like that.",
            "\"Never do that\" was written in the old readme.",
            "> Always run the formatter first in this repo",
            "If you don't pass the flag it will fail loudly.",
            "When you always forget the flag it breaks things.",
            "Never mind, that approach works fine for us now.",
            "Don't worry about the failing flaky test for now.",
            "I always forget to push my branch after commit.",
            "The build always fails on the old laptop machine.",
            "We never found the cause of that crash earlier.",
            "Please can you take a look at this failure soon.",
            "Remember when the cache broke every single night?",
            "Looks fine to me, thanks for the quick update.",
            "What i need to understand is why we never use redis",
            "Should we always run the formatter before committing",
            "Tell me why you never use the token bucket here.",
            "He said \"never ship on friday\" in the meeting.",
        ] {
            assert!(user(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn only_user_text_counts() {
        let s = "Always run the formatter before committing.";
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
        let mut e = event(
            StoredKind::UserText,
            "Always run the formatter before committing.",
        );
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
        let t =
            "Here is the log:\n```\nAlways do this thing in code\n```\nShould we never do that?";
        assert!(user(t).is_empty());
    }

    #[test]
    fn scope_and_anchors() {
        // Project-scoped by default; explicit cues make it global.
        let d = &user("Always write small commits for every change.")[0];
        assert_eq!(d.scope, Scope::Project("/work/app".into()));
        let g = &user("Always write small commits in all projects.")[0];
        assert_eq!(g.scope, Scope::Global);
        assert!(g.anchors.is_empty());
        let p = &user("Never edit src/store/mod.rs and main.rs by hand.")[0];
        assert_eq!(p.scope, Scope::Project("/work/app".into()));
        let paths: Vec<_> = p.anchors.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths, ["src/store/mod.rs", "main.rs"]);
        let q = &user("Prefer `anyhow` for application errors please.")[0];
        assert_eq!(q.scope, Scope::Project("/work/app".into()));
    }

    #[test]
    fn multiple_sentences_yield_multiple_candidates() {
        let got = user("Always lint. Looks fine otherwise. Never skip the tests though.");
        assert_eq!(got.len(), 1, "first sentence is under 15 chars: {got:?}");
        let got = user("Always lint before every commit. Never skip the tests though.");
        assert_eq!(got.len(), 2);
    }
}
