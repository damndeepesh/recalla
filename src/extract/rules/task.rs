//! Tasks: open todo items from `TodoWrite` and `TaskCreate`/`TaskUpdate` calls.
//!
//! One candidate per item that is pending or in progress. Completed and
//! deleted items yield nothing; superseding earlier task memories when a
//! task's status changes is the pipeline's job, not this rule's.

use serde_json::Value;

use super::{anchors_in, decoded};
use crate::extract::scope::project_scope;
use crate::extract::{Candidate, EventCtx};
use crate::model::Kind;
use crate::store::StoredKind;

const CONFIDENCE: f32 = 0.6;
const IMPORTANCE: u8 = 1;
const MAX_CHARS: usize = 200;
const TEXT_FIELDS: [&str; 3] = ["content", "subject", "description"];

/// The parsed input and canonical tool name of a task-tool call; `Null` for any other event.
fn call_items(ctx: &EventCtx<'_>) -> (Value, &'static str) {
    let e = ctx.event;
    let (Some(name), StoredKind::ToolCall) = (e.tool_name.as_deref(), e.kind) else {
        return (Value::Null, "");
    };
    let name = match name {
        "TodoWrite" => "TodoWrite",
        "TaskCreate" => "TaskCreate",
        "TaskUpdate" => "TaskUpdate",
        _ => return (Value::Null, ""),
    };
    (serde_json::from_str(&e.text).unwrap_or(Value::Null), name)
}

fn items_of<'v>(input: &'v Value, name: &str) -> Vec<&'v Value> {
    match name {
        "TodoWrite" => input
            .get("todos")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |a| a.iter().collect()),
        "TaskCreate" | "TaskUpdate" => vec![input],
        _ => Vec::new(),
    }
}

/// Task candidates from one task-tool call.
#[must_use]
pub fn extract(ctx: &EventCtx<'_>, project: Option<&str>) -> Vec<Candidate> {
    let (input, name) = call_items(ctx);
    items_of(&input, name)
        .into_iter()
        .filter_map(|item| open_task(item, name == "TaskUpdate"))
        .map(|text| Candidate {
            kind: Kind::Task,
            confidence: CONFIDENCE,
            importance: IMPORTANCE,
            scope: project_scope(project),
            anchors: anchors_in(&text),
            text,
        })
        .collect()
}

/// Texts of items this call marks completed or deleted, in the same
/// normalized form [`extract`] emits, so the pipeline can find and archive
/// the matching open task memory.
#[must_use]
pub fn finished_items(ctx: &EventCtx<'_>) -> Vec<String> {
    let (input, name) = call_items(ctx);
    items_of(&input, name)
        .into_iter()
        .filter(|i| {
            matches!(
                i.get("status").and_then(Value::as_str),
                Some("completed" | "deleted")
            )
        })
        .filter_map(item_text)
        .collect()
}

fn item_text(item: &Value) -> Option<String> {
    let text = TEXT_FIELDS
        .iter()
        .find_map(|f| item.get(f).and_then(Value::as_str))
        .map(decoded)
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())?;
    Some(text.chars().take(MAX_CHARS).collect())
}

/// The item's text if it is open. A missing status counts as pending, except
/// on an update, where it means the call did not touch the status.
fn open_task(item: &Value, is_update: bool) -> Option<String> {
    match item.get("status").and_then(Value::as_str) {
        Some("pending" | "in_progress") => {}
        None if !is_update => {}
        _ => return None,
    }
    item_text(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::testutil::event;
    use crate::model::Scope;
    use serde_json::json;

    fn run_as(kind: StoredKind, name: &str, input: &Value) -> Vec<Candidate> {
        let mut e = event(kind, &input.to_string());
        e.tool_name = Some(name.into());
        extract(
            &EventCtx {
                event: &e,
                prev: &[],
            },
            Some("/work/app"),
        )
    }

    fn run(name: &str, input: &Value) -> Vec<Candidate> {
        run_as(StoredKind::ToolCall, name, input)
    }

    fn texts(c: &[Candidate]) -> Vec<&str> {
        c.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn todowrite_emits_open_items_only() {
        let got = run(
            "TodoWrite",
            &json!({"todos": [
                {"content": "Write the parser", "status": "pending", "activeForm": "Writing"},
                {"content": "Add tests for src/parse.rs", "status": "in_progress"},
                {"content": "Set up CI", "status": "completed"},
                {"content": "Drop the old API", "status": "deleted"},
            ]}),
        );
        assert_eq!(
            texts(&got),
            ["Write the parser", "Add tests for src/parse.rs"]
        );
        assert_eq!(got[1].anchors[0].path, "src/parse.rs");
        for c in &got {
            assert_eq!(c.kind, Kind::Task);
            assert_eq!(c.importance, 1);
            assert!((c.confidence - 0.6).abs() < 1e-6);
            assert_eq!(c.scope, Scope::Project("/work/app".into()));
        }
    }

    #[test]
    fn sidechain_calls_still_emit() {
        let mut e = event(
            StoredKind::ToolCall,
            &json!({"subject": "Fix the flaky test"}).to_string(),
        );
        e.tool_name = Some("TaskCreate".into());
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
            1
        );
    }

    #[test]
    fn many_open_items_all_emitted() {
        let todos: Vec<Value> = (1..=9)
            .map(|i| json!({"content": format!("Step number {i}"), "status": "pending"}))
            .collect();
        assert_eq!(run("TodoWrite", &json!({ "todos": todos })).len(), 9);
    }

    #[test]
    fn taskcreate_shapes() {
        let got = run(
            "TaskCreate",
            &json!({"subject": "Fix the flaky test", "description": "long text"}),
        );
        assert_eq!(texts(&got), ["Fix the flaky test"]);
        let got = run("TaskCreate", &json!({"description": "Only a description"}));
        assert_eq!(texts(&got), ["Only a description"]);
        let got = run(
            "TaskCreate",
            &json!({"content": "Explicit pending", "status": "pending"}),
        );
        assert_eq!(texts(&got), ["Explicit pending"]);
    }

    #[test]
    fn taskupdate_needs_an_open_status() {
        let open = json!({"taskId": "3", "subject": "Wire the CLI", "status": "in_progress"});
        assert_eq!(texts(&run("TaskUpdate", &open)), ["Wire the CLI"]);
        for input in [
            json!({"taskId": "3", "subject": "Wire the CLI", "status": "completed"}),
            json!({"taskId": "3", "subject": "Wire the CLI", "status": "deleted"}),
            json!({"taskId": "3", "subject": "Wire the CLI"}),
            json!({"taskId": "3", "status": "in_progress"}),
        ] {
            assert!(run("TaskUpdate", &input).is_empty(), "{input}");
        }
    }

    #[test]
    fn finished_items_are_completed_or_deleted_only() {
        let mut e = event(
            StoredKind::ToolCall,
            &json!({"todos": [
                {"content": "Set  up CI", "status": "completed"},
                {"content": "Drop the old API", "status": "deleted"},
                {"content": "Still open", "status": "pending"},
            ]})
            .to_string(),
        );
        e.tool_name = Some("TodoWrite".into());
        let ctx = EventCtx {
            event: &e,
            prev: &[],
        };
        assert_eq!(finished_items(&ctx), ["Set up CI", "Drop the old API"]);
        e.tool_name = Some("Bash".into());
        let ctx = EventCtx {
            event: &e,
            prev: &[],
        };
        assert!(finished_items(&ctx).is_empty());
    }

    #[test]
    fn negatives() {
        for (name, input) in [
            ("TodoWrite", json!({"todos": []})),
            ("TodoWrite", json!({})),
            ("TodoWrite", json!({"todos": "nope"})),
            ("TodoWrite", json!({"todos": [{"status": "pending"}]})),
            (
                "TodoWrite",
                json!({"todos": [{"content": "  ", "status": "pending"}]}),
            ),
            (
                "TodoWrite",
                json!({"todos": [{"content": 5, "status": "pending"}]}),
            ),
            ("TodoWrite", json!({"todos": ["just a string"]})),
            (
                "TodoWrite",
                json!({"todos": [{"content": "x", "status": "blocked"}]}),
            ),
            (
                "TaskCreate",
                json!({"subject": "Done already", "status": "completed"}),
            ),
            ("Bash", json!({"command": "cargo test"})),
            ("TaskList", json!({"subject": "Not a writer"})),
        ] {
            assert!(run(name, &input).is_empty(), "{name} {input}");
        }
    }

    #[test]
    fn only_tool_calls_count_and_bad_json_is_ignored() {
        let input = json!({"todos": [{"content": "Write the parser", "status": "pending"}]});
        assert!(run_as(StoredKind::ToolResult, "TodoWrite", &input).is_empty());
        for name in [Some("TodoWrite"), None] {
            let mut e = event(StoredKind::ToolCall, "not json");
            e.tool_name = name.map(Into::into);
            let ctx = EventCtx {
                event: &e,
                prev: &[],
            };
            assert!(extract(&ctx, None).is_empty());
        }
    }

    #[test]
    fn text_is_normalized_and_capped() {
        let long = "word ".repeat(100);
        let got = run(
            "TodoWrite",
            &json!({"todos": [{"content": format!("  a\n  b {long}"), "status": "pending"}]}),
        );
        assert!(got[0].text.starts_with("a b word"));
        assert_eq!(got[0].text.chars().count(), 200);
    }
}
