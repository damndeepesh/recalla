//! Rule-based extraction: stored events to typed memory candidates.
//!
//! Pure and deterministic (D-004): no I/O except the scope resolver reading
//! `.git` existence under an event's own cwd. Persistence is the pipeline's
//! job; this module only proposes [`Candidate`]s.

mod pipeline;
pub mod rules;
pub mod scope;
pub mod text;

pub use pipeline::{Options, Report, run};

use crate::model::{Anchor, Kind, Scope};
use crate::store::EventRow;

/// Bump when rules change so `extract --rebuild` can re-derive memories.
pub const EXTRACTOR_VERSION: u32 = 1;

/// A memory proposed by a rule. Provenance comes from the source event.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub kind: Kind,
    pub text: String,
    pub confidence: f32,
    pub importance: u8,
    pub scope: Scope,
    pub anchors: Vec<Anchor>,
}

/// The event under extraction plus its session context.
#[derive(Debug, Clone, Copy)]
pub struct EventCtx<'a> {
    pub event: &'a EventRow,
    /// Up to 8 previous events of the same session, oldest first.
    pub prev: &'a [EventRow],
}

/// Runs every rule on one event, resolving the project key from the event's
/// cwd on the real filesystem.
#[must_use]
pub fn extract_event(ctx: &EventCtx<'_>) -> Vec<Candidate> {
    extract_event_with(ctx, scope::project_key)
}

/// As [`extract_event`], with the cwd-to-project resolver injected (tests, or
/// a caching resolver in the pipeline).
pub fn extract_event_with(
    ctx: &EventCtx<'_>,
    project_of: impl Fn(&str) -> String,
) -> Vec<Candidate> {
    let project = ctx
        .event
        .cwd
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(project_of);
    let project = project.as_deref();
    let mut out = Vec::new();
    out.extend(rules::preference::extract(ctx, project));
    out.extend(rules::decision::extract(ctx, project));
    out.extend(rules::gotcha::extract(ctx, project));
    out.extend(rules::procedure::extract(ctx, project));
    out.extend(rules::task::extract(ctx, project));
    out.extend(rules::fact::extract(ctx, project));
    out
}

#[cfg(test)]
pub(crate) mod testutil {
    use crate::store::{EventRow, StoredKind};

    pub fn event(kind: StoredKind, text: &str) -> EventRow {
        EventRow {
            id: 1,
            source_path: "/s.jsonl".into(),
            harness: "claude-code".into(),
            session_id: "s1".into(),
            message_uuid: Some("m1".into()),
            line: 1,
            cwd: Some("/work/app".into()),
            kind,
            tool_name: None,
            tool_id: None,
            is_error: false,
            text: text.into(),
            ts: Some(1),
            sidechain: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::event;
    use super::*;
    use crate::store::StoredKind;

    #[test]
    fn dispatches_with_injected_project_resolver() {
        let e = event(StoredKind::UserText, "Never edit src/gen.rs by hand.");
        let ctx = EventCtx {
            event: &e,
            prev: &[],
        };
        let got = extract_event_with(&ctx, |c| format!("root-of:{c}"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, Kind::Preference);
        assert_eq!(got[0].scope, Scope::Project("root-of:/work/app".into()));
    }

    #[test]
    fn unrelated_events_yield_nothing() {
        let e = event(StoredKind::ToolResult, "Always do this and decided that.");
        let ctx = EventCtx {
            event: &e,
            prev: &[],
        };
        assert!(extract_event_with(&ctx, str::to_owned).is_empty());
    }
}
