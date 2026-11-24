//! Contradiction handling end to end: backfill sessions whose later prompts
//! reverse earlier decisions, then check what survives and what `why` says.

#![allow(clippy::unwrap_used)]

mod common;

use common::Env;
use recalla::model::Status;
use serde_json::{Value, json};

const CWD: &str = "/tmp/eval/conflicts";

fn session(id: &str, users: &[&str]) -> Value {
    let turns: Vec<Value> = users.iter().map(|u| json!({"user": u})).collect();
    json!({"id": id, "cwd": CWD, "turns": turns})
}

fn status_of(env: &Env, needle: &str) -> Vec<(String, Status)> {
    env.store()
        .all_memories(&[
            Status::Active,
            Status::NeedsReverify,
            Status::Superseded,
            Status::Archived,
        ])
        .unwrap()
        .into_iter()
        .filter(|m| m.text.contains(needle))
        .map(|m| (m.uid, m.status))
        .collect()
}

#[test]
fn reversed_decision_supersedes_the_old_one_and_confirm_brings_it_back() {
    let env = Env::new();
    env.write_session(&session(
        "a",
        &[
            "Decision for this project: use tabs for indentation.",
            "Decision for this project: all timestamps are stored as UTC epoch milliseconds.",
        ],
    ));
    env.write_session(&session(
        "b",
        &["Decision for this project: use spaces for indentation."],
    ));
    // Session files sort by name, so "a" is ingested (and extracted) first.
    let (_, ex) = env.backfill();
    assert_eq!(ex.conflicts_superseded, 1, "{ex:?}");
    let tabs = status_of(&env, "tabs");
    assert_eq!(tabs[0].1, Status::Superseded);
    assert_eq!(status_of(&env, "spaces")[0].1, Status::Active);
    assert_eq!(status_of(&env, "epoch")[0].1, Status::Active);

    let uid8 = &tabs[0].0[..8];
    let (code, why, _) = env.cli(&["why", uid8]);
    assert_eq!(code, 0);
    assert!(
        why.contains("superseded-by") && why.contains("opposed choices"),
        "{why}"
    );

    let (code, _, err) = env.cli(&["confirm", uid8]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(status_of(&env, "tabs")[0].1, Status::Active);
    let (_, why, _) = env.cli(&["why", uid8]);
    assert!(!why.contains("superseded-by"), "{why}");
}

#[test]
fn unrelated_decisions_are_left_alone() {
    let env = Env::new();
    env.write_session(&session(
        "a",
        &[
            "Decision for this project: use tabs for indentation.",
            "Decision for this project: use tabs in Makefiles.",
            "Rule: never log request bodies.",
            "Rule: always log request ids.",
        ],
    ));
    let (_, ex) = env.backfill();
    assert_eq!(
        (ex.conflicts_superseded, ex.conflicts_flagged),
        (0, 0),
        "{ex:?}"
    );
    assert!(env.active().len() >= 4);
}

#[test]
fn rebuild_keeps_the_resolution() {
    let env = Env::new();
    env.write_session(&session(
        "a",
        &[
            "Decision for this project: use tabs for indentation.",
            "Decision for this project: use spaces for indentation.",
        ],
    ));
    env.backfill();
    env.extract(true);
    assert_eq!(status_of(&env, "tabs")[0].1, Status::Superseded);
    assert_eq!(status_of(&env, "spaces")[0].1, Status::Active);
}
