//! opencode backfill end to end: a synthetic opencode database (never a real
//! one) is exported, ingested, resumed after an append, and extracted.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use recalla::extract::{self, Options as ExtractOptions};
use recalla::ingest::{Options, Report, run};
use recalla::model::Status;
use recalla::paths;
use recalla::store::Store;
use rusqlite::Connection;

fn fixture(db: &Path) {
    let c = Connection::open(db).unwrap();
    c.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL, parent_id TEXT, time_created INTEGER NOT NULL);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
         INSERT INTO session VALUES ('ses_a', '/work/app', NULL, 1);
         INSERT INTO message VALUES ('m1','ses_a',10,'{\"role\":\"user\"}'),('m2','ses_a',20,'{\"role\":\"assistant\"}');
         INSERT INTO part VALUES
          ('p1','m1','ses_a',10,'{\"type\":\"text\",\"text\":\"From now on always use pnpm, never npm, in this project.\"}'),
          ('p2','m2','ses_a',21,'{\"type\":\"tool\",\"tool\":\"bash\",\"callID\":\"c1\",\"state\":{\"status\":\"error\",\"input\":{\"command\":\"npm install\"},\"error\":\"error: this project uses pnpm\"}}'),
          ('p3','m2','ses_a',22,'{\"type\":\"tool\",\"tool\":\"bash\",\"callID\":\"c2\",\"state\":{\"status\":\"completed\",\"input\":{\"command\":\"pnpm install\"},\"output\":\"done\"}}');",
    )
    .unwrap();
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("opencode");
        std::fs::create_dir_all(&root).unwrap();
        fixture(&root.join("opencode.db"));
        let data = tmp.path().join("data");
        Self {
            _tmp: tmp,
            root,
            data,
        }
    }

    fn backfill(&self) -> Report {
        run(&Options {
            claude_root: None,
            codex_root: None,
            opencode_root: Some(self.root.clone()),
            data_dir: self.data.clone(),
            archive: true,
            dry_run: false,
        })
        .unwrap()
    }
}

#[test]
fn backfill_imports_opencode_sessions_and_resumes_after_an_append() {
    let env = Env::new();
    let first = env.backfill();
    assert_eq!(
        first.files_by_harness.get("opencode"),
        Some(&1),
        "{first:?}"
    );
    assert_eq!(first.events_added, 5); // user + 2 calls + 2 results
    assert_eq!(first.failed.len(), 0);

    let again = env.backfill();
    assert_eq!((again.files_unchanged, again.events_added), (1, 0));

    Connection::open(env.root.join("opencode.db"))
        .unwrap()
        .execute(
            "INSERT INTO part VALUES ('p4','m2','ses_a',99,'{\"type\":\"text\",\"text\":\"All set.\"}')",
            [],
        )
        .unwrap();
    let resumed = env.backfill();
    assert_eq!(
        (resumed.files_resumed, resumed.events_added),
        (1, 1),
        "{resumed:?}"
    );
}

#[test]
fn opencode_history_extracts_memories_with_the_right_project() {
    let env = Env::new();
    env.backfill();
    let mut store = Store::open(paths::db_path(&env.data)).unwrap();
    extract::run(&mut store, &ExtractOptions { rebuild: false }).unwrap();
    let mems = store.all_memories(&[Status::Active]).unwrap();
    assert!(
        mems.iter().any(|m| m.text.contains("pnpm")),
        "expected a pnpm memory, got {:?}",
        mems.iter().map(|m| &m.text).collect::<Vec<_>>()
    );
}

#[test]
fn dry_run_and_a_missing_database_write_nothing_and_do_not_fail() {
    let env = Env::new();
    let dry = run(&Options {
        claude_root: None,
        codex_root: None,
        opencode_root: Some(env.root.clone()),
        data_dir: env.data.clone(),
        archive: true,
        dry_run: true,
    })
    .unwrap();
    assert_eq!(dry.files_seen, 0);
    assert!(!env.data.exists());

    std::fs::remove_file(env.root.join("opencode.db")).unwrap();
    assert_eq!(env.backfill().files_seen, 0);
}

#[test]
fn a_corrupt_database_is_a_reported_error() {
    let env = Env::new();
    std::fs::write(env.root.join("opencode.db"), b"not a database").unwrap();
    let err = run(&Options {
        claude_root: None,
        codex_root: None,
        opencode_root: Some(env.root.clone()),
        data_dir: env.data.clone(),
        archive: true,
        dry_run: false,
    })
    .unwrap_err();
    assert!(err.to_string().contains("opencode.db"), "{err}");
}
