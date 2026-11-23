//! Gotchas: the explanatory sentence an assistant gives after a tool error.
//!
//! Assistant text only, and only when a failed tool result sits in the
//! current exchange (after the last user message in the session window).

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use super::anchors_in;
use crate::extract::scope::project_scope;
use crate::extract::text::{sentences, strip_code};
use crate::extract::{Candidate, EventCtx};
use crate::model::Kind;
use crate::store::{EventRow, StoredKind};

const CONFIDENCE: f32 = 0.65;
const IMPORTANCE: u8 = 1;
const MAX_PER_EVENT: usize = 2;

/// Cause/fix phrases. A bare `because` is deliberately absent.
static CAUSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:root cause|the issue (?:was|is)|turns out|the fix is|fixed by|(?:happens|failed|fails|is|was) because)\b",
    )
    .expect("valid regex")
});

/// Hypotheticals and advice are not findings.
static NOT_A_FINDING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:if|whether|should)\b").expect("valid regex"));

/// Words too generic to tie a sentence to a failure.
const STOP: &[&str] = &[
    "error", "errors", "failed", "failure", "fails", "command", "output", "result", "with", "that",
    "this", "from", "have", "been", "when", "then", "there", "were", "which", "into", "file",
    "files", "code", "line", "true", "false", "null", "none", "stdout", "stderr", "input", "cause",
    "issue", "turns", "root", "fixed", "because", "happens",
];

/// Lowercased alphanumeric words (4+ chars) that can name a failure: tool and
/// command words, file names, error codes.
fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_lowercase)
        .filter(|w| !STOP.contains(&w.as_str()))
        .collect()
}

/// Tokens of the failed results in the current exchange, plus the tool calls
/// that ran just before them (the command, the file).
fn failure_tokens(prev: &[EventRow]) -> HashSet<String> {
    let start = prev
        .iter()
        .rposition(|e| e.kind == StoredKind::UserText)
        .map_or(0, |i| i + 1);
    let window = &prev[start..];
    let mut out = HashSet::new();
    for (i, e) in window.iter().enumerate() {
        if e.kind == StoredKind::ToolResult && e.is_error {
            out.extend(tokens(&e.text));
            if let Some(call) = window[..i]
                .iter()
                .rev()
                .find(|c| c.kind == StoredKind::ToolCall)
            {
                out.extend(tokens(&call.text));
                out.extend(call.tool_name.iter().flat_map(|n| tokens(n)));
            }
        }
    }
    out
}

/// Gotcha candidates from one assistant-text event.
#[must_use]
pub fn extract(ctx: &EventCtx<'_>, project: Option<&str>) -> Vec<Candidate> {
    if ctx.event.kind != StoredKind::AssistantText || !error_in_current_exchange(ctx.prev) {
        return Vec::new();
    }
    let failure = failure_tokens(ctx.prev);
    let clean = strip_code(&ctx.event.text);
    sentences(&clean)
        .into_iter()
        .filter(|s| CAUSE.is_match(s) && !NOT_A_FINDING.is_match(s))
        // The explanation must be about this failure, not any stray error
        // that happens to sit in the window.
        .filter(|s| !tokens(s).is_disjoint(&failure))
        .take(MAX_PER_EVENT)
        .map(|s| Candidate {
            kind: Kind::Gotcha,
            text: s.to_owned(),
            confidence: CONFIDENCE,
            importance: IMPORTANCE,
            scope: project_scope(project),
            anchors: anchors_in(s),
        })
        .collect()
}

/// A failed tool result after the most recent user message in the window.
fn error_in_current_exchange(prev: &[EventRow]) -> bool {
    let start = prev
        .iter()
        .rposition(|e| e.kind == StoredKind::UserText)
        .map_or(0, |i| i + 1);
    prev[start..]
        .iter()
        .any(|e| e.kind == StoredKind::ToolResult && e.is_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::testutil::event;
    use crate::model::Scope;

    /// A failed build whose output mentions the vocabulary the sample
    /// explanations below talk about (a gotcha must be about the failure).
    fn err() -> EventRow {
        let mut e = event(
            StoredKind::ToolResult,
            "error[E0308]: mismatched types; lockfile cache config path migration schema \
             clock dependency init buffer target column index store import lock stale crash feature flag cargo toml",
        );
        e.is_error = true;
        e
    }

    fn run_with(prev: &[EventRow], text: &str) -> Vec<Candidate> {
        let e = event(StoredKind::AssistantText, text);
        extract(&EventCtx { event: &e, prev }, Some("/work/app"))
    }

    fn run(text: &str) -> Vec<Candidate> {
        run_with(&[err()], text)
    }

    #[test]
    fn positives() {
        for s in [
            "The root cause is a stale lockfile in the cache directory.",
            "The issue was that the config path was relative.",
            "The issue is the missing feature flag in Cargo.toml.",
            "Turns out the migration runs before the schema exists.",
            "It turns out the test relied on the wall clock.",
            "The fix is to pin the dependency to an older version.",
            "This was fixed by reordering the two init calls.",
            "The crash happens because the buffer is never flushed.",
            "The build failed because the target directory was locked.",
            "The query fails because the column was renamed earlier.",
            "The slowdown is because the index is rebuilt each run.",
            "That error was because the path had a trailing slash.",
        ] {
            let got = run(s);
            assert_eq!(got.len(), 1, "{s}");
            assert_eq!(got[0].kind, Kind::Gotcha);
            assert_eq!(got[0].importance, 1);
            assert!((got[0].confidence - 0.65).abs() < 1e-6);
        }
    }

    #[test]
    fn sidechain_assistant_text_still_emits() {
        let mut e = event(
            StoredKind::AssistantText,
            "The root cause is a stale lockfile in the cache directory.",
        );
        e.sidechain = true;
        let prev = [err()];
        assert_eq!(
            extract(
                &EventCtx {
                    event: &e,
                    prev: &prev
                },
                None
            )
            .len(),
            1
        );
    }

    #[test]
    fn negatives() {
        for s in [
            "I did this because the tests were slow to run.",
            "I picked the second option because it looked simpler.",
            "Because of the deadline we skipped the extra checks.",
            "If the root cause is the cache then clear it first.",
            "Whether the issue was the lockfile is still unclear.",
            "Should the fix is applied before the release goes out?",
            "What was the root cause of the earlier failure here?",
            "Let me look at the failing test output once more.",
            "The tests pass now and the build is green again.",
            "I will update the config and rerun the whole suite.",
            "Done, the change touches three files in total.",
            "That works because I said so earlier in the thread.",
        ] {
            assert!(run(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn explanation_must_be_about_the_failure() {
        let mut bad = event(StoredKind::ToolResult, "command not found: terraform");
        bad.is_error = true;
        // A stray password sentence next to an unrelated tool error.
        let s = "It turns out the password rotation job changes the credentials nightly.";
        assert!(run_with(&[bad.clone()], s).is_empty());
        // The same error does anchor an explanation that names the tool.
        let s = "Turns out terraform is not installed in this container.";
        assert_eq!(run_with(&[bad.clone()], s).len(), 1);
        // The command that failed counts, not only the output.
        let mut call = event(
            StoredKind::ToolCall,
            r#"{"command":"pnpm install --frozen-lockfile"}"#,
        );
        call.tool_name = Some("Bash".into());
        let mut res = event(StoredKind::ToolResult, "exit 1");
        res.is_error = true;
        let s = "Turns out pnpm wants a lockfile update first.";
        assert_eq!(run_with(&[call, res], s).len(), 1);
        // Generic words alone do not link a sentence to a failure.
        let s = "Turns out the error output was just noisy this time.";
        assert!(run_with(&[bad], s).is_empty());
    }

    #[test]
    fn needs_assistant_text_and_an_error() {
        let s = "The root cause is a stale lockfile in the cache.";
        assert!(run_with(&[], s).is_empty());
        let ok = event(StoredKind::ToolResult, "fine");
        assert!(run_with(&[ok], s).is_empty());
        for k in [
            StoredKind::UserText,
            StoredKind::ToolCall,
            StoredKind::Summary,
        ] {
            let e = event(k, s);
            let ctx = EventCtx {
                event: &e,
                prev: &[err()],
            };
            assert!(extract(&ctx, None).is_empty());
        }
    }

    #[test]
    fn error_must_be_in_the_current_exchange() {
        let s = "The root cause is a stale lockfile in the cache.";
        let user = event(StoredKind::UserText, "ok, go on");
        let reply = event(StoredKind::AssistantText, "trying again");
        // Error before the last user message does not count.
        assert!(run_with(&[err(), user.clone(), reply.clone()], s).is_empty());
        // Error after it does.
        assert_eq!(run_with(&[user.clone(), reply.clone(), err()], s).len(), 1);
        // No user message in the window: the whole window is the exchange.
        assert_eq!(run_with(&[reply, err()], s).len(), 1);
        // Non-error result after an old error and a user message.
        let ok = event(StoredKind::ToolResult, "fine");
        assert!(run_with(&[err(), user, ok], s).is_empty());
    }

    #[test]
    fn at_most_two_per_event_and_code_ignored() {
        let t = "The root cause is the stale cache here. Turns out the lock was held. \
                 The fix is to delete the lock file.";
        let got = run(t);
        assert_eq!(got.len(), 2);
        assert!(got[0].text.starts_with("The root cause"));
        let fenced = "```\nThe root cause is something in a log\n```";
        assert!(run(fenced).is_empty());
    }

    #[test]
    fn scope_and_anchors() {
        let g = &run("The root cause is a bad import in src/store/mod.rs today.")[0];
        assert_eq!(g.scope, Scope::Project("/work/app".into()));
        assert_eq!(g.anchors[0].path, "src/store/mod.rs");
        let e = event(StoredKind::AssistantText, "Turns out the cache was stale.");
        let mut e = e;
        e.cwd = None;
        let c = extract(
            &EventCtx {
                event: &e,
                prev: &[err()],
            },
            None,
        );
        assert_eq!(c[0].scope, Scope::Global);
    }
}
