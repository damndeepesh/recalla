//! One module per memory kind. Each exposes
//! `extract(&EventCtx, project: Option<&str>) -> Vec<Candidate>`, where
//! `project` is the resolved project key of the event's cwd.

pub mod decision;
pub mod fact;
pub mod gotcha;
pub mod preference;
pub mod procedure;
pub mod task;

use crate::model::Anchor;
use crate::redact::redact;

use super::text::path_tokens;

/// Path-only anchors for the source paths named in `text`.
fn anchors_in(text: &str) -> Vec<Anchor> {
    path_tokens(text).into_iter().map(Anchor::new).collect()
}

/// Redacts a string obtained by JSON-decoding tool input. Ingest already
/// redacts, but only the escaped form can slip (e.g. `password=\t...`), so
/// every decoded string is redacted again before it can become a memory.
fn decoded(s: &str) -> String {
    redact(s).text.into_owned()
}
