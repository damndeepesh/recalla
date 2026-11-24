//! Decisions: stated choices in conversation, and `## D-NNN` entries written
//! to `DESIGN_DECISIONS.md`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{anchors_in, decoded};
use crate::extract::scope::project_scope;
use crate::extract::text::{is_question, is_quoted, sentences, strip_code};
use crate::extract::{Candidate, EventCtx};
use crate::model::Kind;
use crate::store::StoredKind;

const USER_CONFIDENCE: f32 = 0.8;
const ASSISTANT_CONFIDENCE: f32 = 0.7;
const ADR_CONFIDENCE: f32 = 0.95;
const IMPORTANCE: u8 = 2;
const ADR_IMPORTANCE: u8 = 3;
const ADR_FILE: &str = "DESIGN_DECISIONS.md";
const MAX_ADR_CHARS: usize = 400;

static PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)\b(?:decided\b|decision:|decision\s+for\s+(?:this|the)\s+[\w\- ]{1,24}:\s+\S+\s+\S+\s+\S+|we['\u{2019}]ll use\b|we will use\b|going with\b|chose\b.+\bover\b|switch(?:ed)? to\b|^(?:we\s+)?switched\b(?:\s+[\w\-./`']+){1,6}?\s+to\b|settled on\b|picked\b.+\bbecause\b)",
    )
    .expect("valid regex")
});

/// Hedges and hypotheticals that contain a phrase without stating a choice.
static NOT_A_DECISION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:if|whether|unless|once|should)\b|\b(?:haven['\u{2019}]t|hasn['\u{2019}]t|isn['\u{2019}]t|wasn['\u{2019}]t|not|never|yet\s+to)\s+(?:yet\s+)?(?:decided|going\s+with|settled)\b",
    )
    .expect("valid regex")
});

static ADR_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^##\s+D-\d+\s+(.+?)\s*$").expect("valid regex"));
static TRAILING_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\(\d{4}-\d{2}-\d{2}\)\s*$").expect("valid regex"));
static DECISION_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*[-*]?\s*Decision:\s*(.+?)\s*$").expect("valid regex"));

/// Decision candidates from one event.
#[must_use]
pub fn extract(ctx: &EventCtx<'_>, project: Option<&str>) -> Vec<Candidate> {
    match ctx.event.kind {
        // Subagent "user" turns are orchestrator instructions, not the human's words.
        StoredKind::UserText if !ctx.event.sidechain => spoken(ctx, project, USER_CONFIDENCE),
        StoredKind::AssistantText => spoken(ctx, project, ASSISTANT_CONFIDENCE),
        StoredKind::ToolCall => adr_blocks(ctx, project),
        _ => Vec::new(),
    }
}

fn spoken(ctx: &EventCtx<'_>, project: Option<&str>, confidence: f32) -> Vec<Candidate> {
    let clean = strip_code(&ctx.event.text);
    sentences(&clean)
        .into_iter()
        .filter(|s| states_decision(s))
        .map(|s| Candidate {
            kind: Kind::Decision,
            text: s.to_owned(),
            confidence,
            importance: IMPORTANCE,
            scope: project_scope(project),
            anchors: anchors_in(s),
        })
        .collect()
}

/// A decision needs substance: at least this many words.
const MIN_WORDS: usize = 5;

/// Adverbs that can follow "decided" without naming what was decided.
const BARE_ADVERBS: &[&str] = &[
    "upfront",
    "early",
    "late",
    "later",
    "earlier",
    "already",
    "finally",
    "together",
    "again",
    "then",
    "now",
    "today",
    "here",
    "there",
    "otherwise",
    "wisely",
    "quickly",
];

/// True when the word right after the marker is a stray adverb or ends a
/// clause ("Decided upfront, not bolted on later."), so no object is named.
fn lacks_object(after_marker: &str) -> bool {
    let Some(first) = after_marker.split_whitespace().next() else {
        return true;
    };
    let word = first.trim_end_matches(|c: char| !c.is_alphanumeric());
    first.ends_with([',', ';', '.', '!']) && !word.is_empty()
        || BARE_ADVERBS.contains(&word.to_lowercase().as_str())
        || (word.len() > 3 && word.to_lowercase().ends_with("ly"))
}

fn states_decision(sentence: &str) -> bool {
    if is_question(sentence)
        || sentence.split_whitespace().count() < MIN_WORDS
        || NOT_A_DECISION.is_match(sentence)
    {
        return false;
    }
    PHRASE.find(sentence).is_some_and(|m| {
        // Only a bare "decided" can be followed by nothing decided; the other
        // phrases ("chose X over Y", "going with X") carry their object.
        let bare = m.as_str().eq_ignore_ascii_case("decided");
        !(is_quoted(sentence, m.start()) || bare && lacks_object(&sentence[m.end()..]))
    })
}

/// `## D-NNN Title` blocks inside a Write/Edit to `DESIGN_DECISIONS.md`.
fn adr_blocks(ctx: &EventCtx<'_>, project: Option<&str>) -> Vec<Candidate> {
    let e = ctx.event;
    let field = match e.tool_name.as_deref() {
        Some("Write") => "content",
        Some("Edit") => "new_string",
        _ => return Vec::new(),
    };
    // Tool input is compact JSON; truncated or odd input is simply skipped.
    let Ok(input) = serde_json::from_str::<Value>(&e.text) else {
        return Vec::new();
    };
    let targets_adr_file = input
        .get("file_path")
        .and_then(Value::as_str)
        .is_some_and(|p| p.rsplit(['/', '\\']).next() == Some(ADR_FILE));
    let Some(body) = input.get(field).and_then(Value::as_str) else {
        return Vec::new();
    };
    if !targets_adr_file {
        return Vec::new();
    }
    parse_adr(&decoded(body))
        .into_iter()
        .map(|text| Candidate {
            kind: Kind::Decision,
            anchors: anchors_in(&text),
            text,
            confidence: ADR_CONFIDENCE,
            importance: ADR_IMPORTANCE,
            scope: project_scope(project),
        })
        .collect()
}

/// One `"<Title>: <Decision>"` (or just the title) per ADR block.
fn parse_adr(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    // (title, decision line) of the block being read.
    let mut current: Option<(String, Option<String>)> = None;
    let mut flush = |c: Option<(String, Option<String>)>| {
        if let Some((title, decision)) = c {
            let text = decision.map_or(title.clone(), |d| format!("{title}: {d}"));
            out.push(text.chars().take(MAX_ADR_CHARS).collect());
        }
    };
    for line in body.lines() {
        if let Some(c) = ADR_HEADING.captures(line) {
            flush(current.take());
            let title = TRAILING_DATE.replace(&c[1], "").trim().to_owned();
            current = Some((title, None));
        } else if line.starts_with("## ") {
            flush(current.take());
        } else if let Some((_, decision @ None)) = current.as_mut() {
            if let Some(c) = DECISION_LINE.captures(line) {
                *decision = Some(c[1].to_owned());
            }
        }
    }
    flush(current);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::testutil::event;
    use crate::model::Scope;
    use serde_json::json;

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

    #[test]
    fn labeled_project_decisions_count() {
        let hit = run(
            StoredKind::UserText,
            "Decision for this project: all timestamps are stored as UTC epoch milliseconds.",
        );
        assert_eq!(hit.len(), 1);
        for s in [
            "Decision for this project: tbd.",
            "What is the decision for this project: A or B?",
            "The decision for the weekend trip is still open for now.",
        ] {
            assert!(
                run(StoredKind::UserText, s).is_empty(),
                "should not match: {s}"
            );
        }
    }

    #[test]
    fn a_decision_needs_an_object_and_five_words() {
        for s in [
            "We decided that the cache must be keyed by tenant.",
            "We chose Postgres over MySQL.",
            "Decided to keep the archive format as it is.",
            "Going with sqlite for the local store.",
        ] {
            assert_eq!(run(StoredKind::UserText, s).len(), 1, "{s}");
        }
        for s in [
            "Decided upfront, not bolted on later.",
            "We decided.",
            "Decided quickly and moved on.",
            "Going with it.",
        ] {
            assert!(run(StoredKind::UserText, s).is_empty(), "{s}");
        }
    }

    #[test]
    fn switched_with_an_object_phrase_before_to() {
        for s in [
            "Switched the default tags to the new cost-center key in providers.tf.",
            "We switched the retry policy to exponential backoff.",
        ] {
            assert_eq!(run(StoredKind::AssistantText, s).len(), 1, "{s}");
        }
        for s in [
            // First-person narration of an edit, not a stated choice.
            "I switched the button and modal components to named exports.",
            // Too far from "to" to be one clause.
            "Switched one two three four five six seven to eight.",
            "Have we switched the tags to the new key?",
        ] {
            assert!(run(StoredKind::AssistantText, s).is_empty(), "{s}");
        }
    }

    fn tool(name: &str, input: &Value) -> Vec<Candidate> {
        let mut e = event(StoredKind::ToolCall, &input.to_string());
        e.tool_name = Some(name.into());
        extract(
            &EventCtx {
                event: &e,
                prev: &[],
            },
            Some("/work/app"),
        )
    }

    #[test]
    fn positives_from_both_speakers() {
        for s in [
            "We decided to keep the store behind a single module.",
            "Decision: use blake3 for content hashes everywhere.",
            "We'll use rusqlite with the bundled feature enabled.",
            "We will use a single writer for the database file.",
            "Going with plain regexes instead of a parser crate.",
            "I chose SQLite over a server database for simplicity.",
            "We switched to a content-addressed archive layout.",
            "Let's switch to the new layout for the archive files.",
            "Settled on rule-based extraction for the first release.",
            "I picked zstd because it compresses fast and well.",
            "After some thought we decided against a daemon process.",
            "That is why we're going with a stdio server only.",
        ] {
            for (kind, conf) in [
                (StoredKind::UserText, 0.8),
                (StoredKind::AssistantText, 0.7),
            ] {
                let got = run(kind, s);
                assert_eq!(got.len(), 1, "{s}");
                assert_eq!(got[0].kind, Kind::Decision);
                assert!((got[0].confidence - conf).abs() < 1e-6);
                assert_eq!(got[0].importance, 2);
                assert_eq!(got[0].scope, Scope::Project("/work/app".into()));
            }
        }
    }

    #[test]
    fn negatives() {
        for s in [
            "Which database should we pick for the project?",
            "We haven't decided on the storage format yet.",
            "This question is still undecided at the moment.",
            "If we decided to switch to Postgres we would migrate.",
            "Whether we go with plain regexes is still open.",
            "\"We decided to rewrite it\" was in the old notes.",
            "Switching tabs in the editor is slow today.",
            "The tests passed and the build looks green now.",
            "Nothing was chosen and nobody picked a winner.",
            "Should we decide soon on the final directory layout?",
            "We should decide soon about the directory layout.",
            "Let me read the file and report what it contains.",
            "Not yet decided: the name of the new subcommand.",
            "What i need to understand is why we decided to use redis",
            "How did we decide to switch to the new layout",
        ] {
            for kind in [StoredKind::UserText, StoredKind::AssistantText] {
                assert!(run(kind, s).is_empty(), "{s}");
            }
        }
    }

    #[test]
    fn sidechain_skips_user_but_not_assistant_or_adr() {
        let s = "We decided to keep the store behind a single module.";
        let run_side = |kind| {
            let mut e = event(kind, s);
            e.sidechain = true;
            extract(
                &EventCtx {
                    event: &e,
                    prev: &[],
                },
                None,
            )
        };
        assert!(run_side(StoredKind::UserText).is_empty());
        assert_eq!(run_side(StoredKind::AssistantText).len(), 1);
        let mut e = event(
            StoredKind::ToolCall,
            &json!({"file_path": "DESIGN_DECISIONS.md", "content": ADR}).to_string(),
        );
        e.tool_name = Some("Write".into());
        e.sidechain = true;
        assert_eq!(
            extract(
                &EventCtx {
                    event: &e,
                    prev: &[]
                },
                None
            )
            .len(),
            3
        );
    }

    #[test]
    fn spoken_decisions_ignore_other_kinds_and_code() {
        let s = "We decided to keep the store behind a single module.";
        assert!(run(StoredKind::ToolResult, s).is_empty(), "tool result");
        assert!(run(StoredKind::Summary, s).is_empty(), "summary");
        assert!(
            run(StoredKind::UserText, &format!("```\n{s}\n```")).is_empty(),
            "fenced"
        );
    }

    #[test]
    fn spoken_decision_anchors_and_scope() {
        let got = run(
            StoredKind::AssistantText,
            "We decided to move the parser into src/harness/claude.rs for now.",
        );
        assert_eq!(got[0].anchors.len(), 1);
        assert_eq!(got[0].anchors[0].path, "src/harness/claude.rs");
        let e = event(
            StoredKind::UserText,
            "We decided to keep one module for it.",
        );
        let none = extract(
            &EventCtx {
                event: &e,
                prev: &[],
            },
            None,
        );
        assert_eq!(none[0].scope, Scope::Global);
    }

    const ADR: &str = "# Decisions\n\n\
        ## D-001 Use SQLite (2026-01-02)\n\
        Context: need storage.\n\
        Decision: SQLite with FTS5 in src/store/mod.rs.\n\
        Why: single file.\n\n\
        ## D-002 Rule-based extraction (2026-01-03)\n\
        - Context: no LLM.\n\
        - Decision: regex rules only.\n\n\
        ## D-003 Title only\n\
        Context: nothing else here.\n\n\
        ## Notes\n\
        Decision: ignored because it is outside an ADR block.\n";

    #[test]
    fn write_parses_every_block() {
        let got = tool(
            "Write",
            &json!({"file_path": "/work/app/DESIGN_DECISIONS.md", "content": ADR}),
        );
        let texts: Vec<_> = got.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Use SQLite: SQLite with FTS5 in src/store/mod.rs.",
                "Rule-based extraction: regex rules only.",
                "Title only",
            ]
        );
        for c in &got {
            assert_eq!(c.kind, Kind::Decision);
            assert!((c.confidence - 0.95).abs() < 1e-6);
            assert_eq!(c.importance, 3);
        }
        assert_eq!(got[0].anchors[0].path, "src/store/mod.rs");
    }

    #[test]
    fn edit_uses_new_string_only() {
        let input = json!({
            "file_path": "DESIGN_DECISIONS.md",
            "old_string": "## D-009 Old thing\nDecision: old.",
            "new_string": "## D-010 New thing\nDecision: new.\n",
            "content": "## D-011 Wrong field\nDecision: wrong."
        });
        let got = tool("Edit", &input);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "New thing: new.");
        // Write reads `content`, not `new_string`.
        let got = tool("Write", &input);
        assert_eq!(got[0].text, "Wrong field: wrong.");
    }

    #[test]
    fn adr_ignores_other_files_tools_and_bad_input() {
        let adr = |file: &str| json!({"file_path": file, "content": ADR, "new_string": ADR});
        assert!(tool("Write", &adr("/work/app/NOTES.md")).is_empty());
        assert!(tool("Write", &adr("/work/app/MY_DESIGN_DECISIONS.md")).is_empty());
        assert!(tool("Read", &adr("/work/app/DESIGN_DECISIONS.md")).is_empty());
        assert!(tool("Write", &json!({"content": ADR})).is_empty());
        assert!(tool("Write", &json!({"file_path": "DESIGN_DECISIONS.md"})).is_empty());
        let mut e = event(
            StoredKind::ToolCall,
            "{\"file_path\": \"DESIGN_DECISIONS.md\", trunc",
        );
        e.tool_name = Some("Write".into());
        assert!(
            extract(
                &EventCtx {
                    event: &e,
                    prev: &[]
                },
                None
            )
            .is_empty()
        );
    }

    #[test]
    fn adr_keeps_redaction_markers_and_caps_length() {
        let body = format!(
            "## D-001 Key handling\nDecision: store {} and {}\n",
            "[REDACTED:key]",
            "x".repeat(600)
        );
        let got = tool(
            "Write",
            &json!({"file_path": "DESIGN_DECISIONS.md", "content": body}),
        );
        assert!(got[0].text.contains("[REDACTED:key]"));
        assert_eq!(got[0].text.chars().count(), 400);
    }
}
