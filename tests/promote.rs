//! `recalla promote`: moving a memory between scopes, end to end.

#![allow(clippy::unwrap_used)]

mod common;

use common::Env;
use recalla::mirror;
use recalla::model::{Kind, Scope, Status};
use recalla::paths;
use recalla::paths::mirror_dir;
use recalla::store::Store;

const WEB_API: &str = "/tmp/eval/web-api";

fn backfilled() -> Env {
    let env = Env::new();
    env.write_all();
    env.backfill();
    env
}

fn project_preference(env: &Env) -> recalla::model::Memory {
    env.store()
        .all_memories(&[Status::Active])
        .unwrap()
        .into_iter()
        .find(|m| m.kind == Kind::Preference && m.scope == Scope::Project(WEB_API.into()))
        .expect("a web-api preference")
}

#[test]
fn promoting_a_preference_makes_it_appear_in_other_projects() {
    let env = backfilled();
    let m = project_preference(&env);
    let short = &m.uid[..8];
    let other = ["pack", "--project", "/no/such/other-project", "--no-log"];
    let (_, before, _) = env.cli(&other);
    assert!(!before.contains(&m.text), "not visible yet");

    let (code, out, err) = env.cli(&["promote", short]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Moved") && out.contains("global"), "{out}");

    let (_, after, _) = env.cli(&other);
    assert!(after.contains(&m.text), "{after}");
    // The original is superseded, not deleted; the new memory is global.
    let s = env.store();
    assert_eq!(s.get(&m.uid).unwrap().unwrap().status, Status::Superseded);
    let moved: Vec<_> = s
        .all_memories(&[Status::Active])
        .unwrap()
        .into_iter()
        .filter(|x| x.text == m.text)
        .collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].scope, Scope::Global);
    drop(s);

    // Promoting again is refused; so is a task.
    let (code, _, err) = env.cli(&["promote", &moved[0].uid[..8]]);
    assert_eq!(code, 1);
    assert!(err.contains("already in that scope"), "{err}");
    let task = env
        .store()
        .all_memories(&[Status::Active])
        .unwrap()
        .into_iter()
        .find(|x| x.kind == Kind::Task);
    if let Some(t) = task {
        let (code, _, err) = env.cli(&["promote", &t.uid[..8]]);
        assert_eq!(code, 1);
        assert!(err.contains("tasks"), "{err}");
    }
}

#[test]
fn project_flag_pins_to_one_project_and_survives_a_mirror_rebuild() {
    let env = backfilled();
    let m = project_preference(&env);
    let (code, _, err) = env.cli(&["promote", &m.uid[..8], "--project", "/tmp/eval/elsewhere"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, _) = env.cli(&["promote", &m.uid[..8], "--project", ""]);
    assert_eq!(code, 2, "empty project key is a usage error");

    let before = env
        .store()
        .all_memories(&[Status::Active, Status::Superseded])
        .unwrap();
    let fresh = Env::new();
    let r = mirror::rebuild(&fresh.data, &mirror_dir(&env.data), true, false).unwrap();
    assert!(r.applied);
    let s = Store::open(paths::db_path(&fresh.data)).unwrap();
    let after = s
        .all_memories(&[Status::Active, Status::Superseded])
        .unwrap();
    assert_eq!(before.len(), after.len());
    let pinned = after
        .iter()
        .find(|x| x.scope == Scope::Project("/tmp/eval/elsewhere".into()))
        .unwrap();
    assert_eq!(pinned.text, m.text);
    assert_eq!(
        s.get(&m.uid).unwrap().unwrap().superseded_by.as_deref(),
        Some(pinned.uid.as_str())
    );
}
