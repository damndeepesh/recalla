//! The markdown mirror and `recalla rebuild --from-mirror` end to end.

#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use common::Env;
use recalla::mirror::{self, RebuildError, format};
use recalla::model::{self, Anchor, Kind, Memory, NewMemory, Origin, Provenance, Scope, Status};
use recalla::paths;
use recalla::store::{Filter, FullMemory, Store};
use rusqlite::{Connection, types::Value};
use serde_json::json;

const PROJECT: &str = "/work/mirror-it";

fn new_mem(kind: Kind, text: &str, scope: Scope) -> NewMemory {
    NewMemory {
        kind,
        text: text.into(),
        scope,
        confidence: 0.8,
        importance: 2,
    }
}

fn prov(n: u32) -> Provenance {
    Provenance {
        harness: Some("claude-code".into()),
        session_id: Some(format!("s{n}")),
        message_uuid: Some(format!("m{n}")),
        source_path: Some(format!("/x/{n}.jsonl")),
        line: Some(i64::from(n)),
        git_commit: (n % 2 == 0).then(|| "abc123".into()),
        ..Provenance::new(Origin::Backfill)
    }
}

/// A store with every awkward shape the mirror must carry.
fn seeded(data: &Path) -> Store {
    let mut s = recalla::ops::open_or_create(data).unwrap();
    s.set_clock(Box::new(|| 1_700_000_000_000));
    let module = Scope::Module {
        project: PROJECT.into(),
        path: "src/store".into(),
    };
    let texts = [
        (
            Kind::Decision,
            "Use WAL: it's \"faster\"\n---\nand safer",
            Scope::Global,
        ),
        (
            Kind::Gotcha,
            "Multibyte 日本語 and emoji 😀 survive",
            Scope::Project(PROJECT.into()),
        ),
        (Kind::Fact, "Module scoped fact # not a comment", module),
        (Kind::Preference, "Old way", Scope::Project(PROJECT.into())),
        (Kind::Preference, "New way", Scope::Project(PROJECT.into())),
        (
            Kind::Fact,
            "Archived one",
            Scope::Project("/work/other".into()),
        ),
        (
            Kind::Fact,
            "Needs a second look",
            Scope::Project(PROJECT.into()),
        ),
    ];
    let mut uids = Vec::new();
    for (i, (kind, text, scope)) in texts.iter().enumerate() {
        let m = new_mem(*kind, text, scope.clone());
        let n = u32::try_from(i).unwrap();
        let uid = s.insert(&m, &prov(n)).unwrap().uid().to_owned();
        // A second and third source for the first memories.
        if i < 3 {
            s.insert(&m, &prov(n + 100)).unwrap();
            s.insert(&m, &prov(n + 200)).unwrap();
        }
        uids.push(uid);
    }
    s.set_clock(Box::new(|| 1_700_000_500_000));
    s.replace_anchors(
        &uids[1],
        &[
            Anchor {
                symbol: Some("Store::insert".into()),
                lang: Some("rust".into()),
                content_hash: Some("deadbeef".into()),
                git_commit: Some("abc123".into()),
                ..Anchor::new("src/store/mod.rs")
            },
            Anchor::new("src/gone.rs"),
        ],
    )
    .unwrap();
    s.add_anchor(&uids[6], &Anchor::new("src/lib.rs")).unwrap();
    s.supersede(&uids[3], &uids[4]).unwrap();
    s.archive(&uids[5]).unwrap();
    s.set_status(&uids[6], Status::NeedsReverify).unwrap();
    let gone = s
        .anchors_for_project(PROJECT)
        .unwrap()
        .into_iter()
        .find(|(_, _, a, _)| a.path == "src/gone.rs")
        .unwrap()
        .1;
    s.mark_stale(gone, "file `src/gone.rs` no longer exists")
        .unwrap();
    s
}

/// The whole memory layer as ordered SQL rows (uid replaces row ids).
fn layer(data: &Path) -> Vec<Vec<Vec<Value>>> {
    let conn = Connection::open(paths::db_path(data)).unwrap();
    let queries = [
        "SELECT uid, kind, text, scope_key, scope_project, scope_path, confidence,
                importance, status, superseded_by, seen_count, created_at, updated_at,
                valid_from, valid_to FROM memories ORDER BY uid",
        "SELECT m.uid, p.origin, p.harness, p.session_id, p.message_uuid, p.source_path,
                p.line, p.git_commit, p.created_at
         FROM provenance p JOIN memories m ON m.id = p.memory_id ORDER BY m.uid, p.id",
        "SELECT m.uid, a.path, a.symbol, a.lang, a.content_hash, a.git_commit, a.stale_reason
         FROM anchors a JOIN memories m ON m.id = a.memory_id ORDER BY m.uid, a.id",
        "SELECT src, dst, kind, created_at FROM relations ORDER BY src, dst, kind",
    ];
    queries
        .iter()
        .map(|q| {
            let mut st = conn.prepare(q).unwrap();
            let n = st.column_count();
            st.query_map([], |r| (0..n).map(|i| r.get::<_, Value>(i)).collect())
                .unwrap()
                .map(Result::unwrap)
                .collect()
        })
        .collect()
}

fn count(data: &Path, table: &str) -> i64 {
    Connection::open(paths::db_path(data))
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn mirror_dir(data: &Path) -> PathBuf {
    paths::mirror_dir(data)
}

fn recalla(args: &[&str], data: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recalla"))
        .args(args)
        .arg("--data-dir")
        .arg(data)
        .output()
        .unwrap()
}

fn dir_listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn full_round_trip_into_a_fresh_dir_is_identical() {
    let env = Env::new();
    let mut s = seeded(&env.data);
    let report = mirror::sync(&mut s, &mirror_dir(&env.data), true).unwrap();
    assert_eq!(report.written, 7);
    assert!(report.strays.is_empty());
    drop(s);
    let before = layer(&env.data);
    assert!(
        before[2].iter().any(|r| r[6] != Value::Null),
        "stale reason seeded"
    );
    assert!(before[1].len() >= 9, "multiple provenance seeded");

    let fresh = Env::new();
    let r = mirror::rebuild(&fresh.data, &mirror_dir(&env.data), true, false).unwrap();
    assert!(r.applied && r.backup.is_none());
    assert_eq!(r.memories, 7);
    assert_eq!(layer(&fresh.data), before);
    // And the rebuilt DB mirrors back to the same bytes.
    let mut s2 = Store::open(paths::db_path(&fresh.data)).unwrap();
    let again = mirror::sync(&mut s2, &mirror_dir(&fresh.data), true).unwrap();
    assert_eq!(again.written, 7);
    for f in mirror::md_files(&mirror_dir(&env.data)).unwrap() {
        let rel = f.strip_prefix(mirror_dir(&env.data)).unwrap();
        assert_eq!(
            fs::read(&f).unwrap(),
            fs::read(mirror_dir(&fresh.data).join(rel)).unwrap(),
            "{rel:?}"
        );
    }
}

#[test]
fn incremental_sync_writes_exactly_the_changed_files() {
    let env = Env::new();
    let mut s = seeded(&env.data);
    let dir = mirror_dir(&env.data);
    mirror::sync(&mut s, &dir, true).unwrap();
    // Nothing changed: nothing to do.
    let r = mirror::sync(&mut s, &dir, false).unwrap();
    assert_eq!(r.written, 0);
    // Corrupt an untouched file: an incremental sync must not look at it.
    let files = mirror::md_files(&dir).unwrap();
    let victim = files[0].clone();
    fs::write(&victim, "hand edited").unwrap();
    let uid = s
        .search("Needs a second look", &Filter::default(), 1)
        .unwrap()[0]
        .memory
        .uid
        .clone();
    s.set_clock(Box::new(|| 1_800_000_000_000));
    s.set_confidence(&uid, 0.99).unwrap();
    let r = mirror::sync(&mut s, &dir, false).unwrap();
    assert_eq!(r.written, 1);
    assert_eq!(fs::read_to_string(&victim).unwrap(), "hand edited");
    let changed = files
        .iter()
        .find(|f| f.file_stem().unwrap().to_str().unwrap() == uid)
        .unwrap();
    assert!(
        fs::read_to_string(changed)
            .unwrap()
            .contains("confidence: 0.99")
    );
    // A full sync repairs the hand-edited file.
    let r = mirror::sync(&mut s, &dir, true).unwrap();
    assert_eq!(r.written, 1);
    assert!(
        fs::read_to_string(&victim)
            .unwrap()
            .starts_with("---\nformat: 1\n")
    );
}

#[test]
fn tampered_file_refuses_the_rebuild_and_touches_nothing() {
    let env = Env::new();
    let mut s = seeded(&env.data);
    let dir = mirror_dir(&env.data);
    mirror::sync(&mut s, &dir, true).unwrap();
    drop(s);
    let before = layer(&env.data);
    let files_before = dir_listing(&env.data);
    let target = mirror::md_files(&dir).unwrap().remove(0);
    let text = fs::read_to_string(&target).unwrap();
    fs::write(
        &target,
        text.replace("Old way", "Evil way")
            .replace("Use WAL", "Use evil"),
    )
    .unwrap();
    let err = mirror::rebuild(&env.data, &dir, true, false).unwrap_err();
    let RebuildError::Refused(problems) = &err else {
        panic!("{err}")
    };
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].path, target);
    assert!(err.to_string().contains("nothing was changed"));
    assert_eq!(layer(&env.data), before);
    assert_eq!(
        dir_listing(&env.data),
        files_before,
        "no tmp or backup files"
    );
}

#[test]
fn malformed_files_refuse_the_rebuild_and_the_list_is_capped() {
    let env = Env::new();
    let mut s = seeded(&env.data);
    let dir = mirror_dir(&env.data);
    mirror::sync(&mut s, &dir, true).unwrap();
    drop(s);
    let before = layer(&env.data);
    for i in 0..25 {
        fs::write(dir.join(format!("global/junk{i:02}.md")), "not a memory").unwrap();
    }
    let err = mirror::rebuild(&env.data, &dir, true, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("25 mirror file(s)"), "{err}");
    assert_eq!(err.matches("junk").count(), 20, "{err}");
    assert!(err.contains("and 5 more"));
    assert_eq!(layer(&env.data), before);
    // Dry run refuses too.
    assert!(mirror::rebuild(&env.data, &dir, false, false).is_err());
    // An empty mirror never wipes the layer.
    let empty = env.data.join("empty-mirror");
    fs::create_dir_all(&empty).unwrap();
    assert!(matches!(
        mirror::rebuild(&env.data, &empty, true, false),
        Err(RebuildError::Empty(_))
    ));
}

#[test]
fn strays_are_reported_on_full_sync_and_left_alone() {
    let env = Env::new();
    let mut s = seeded(&env.data);
    let dir = mirror_dir(&env.data);
    mirror::sync(&mut s, &dir, true).unwrap();
    let stray = dir.join("global/fact/00000000000000000000000000000000.md");
    fs::create_dir_all(stray.parent().unwrap()).unwrap();
    fs::write(&stray, "leftover").unwrap();
    assert!(mirror::sync(&mut s, &dir, false).unwrap().strays.is_empty());
    assert_eq!(
        mirror::sync(&mut s, &dir, true).unwrap().strays,
        std::slice::from_ref(&stray)
    );
    assert!(stray.exists());
    drop(s);
    let out = recalla(&["mirror", "--full", "--json"], &env.data);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let got: Vec<std::path::PathBuf> = v["strays"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().into())
        .collect();
    assert_eq!(got, std::slice::from_ref(&stray)); // Path equality ignores separator style
    assert_eq!(v["written"], 0);
}

#[cfg(unix)]
#[test]
fn mirror_dirs_are_0700_and_files_0600() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let mut s = seeded(&env.data);
    let dir = mirror_dir(&env.data);
    mirror::sync(&mut s, &dir, true).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let mut stack = vec![dir.clone()];
    let mut files = 0;
    while let Some(d) = stack.pop() {
        assert_eq!(mode(&d), 0o700, "{}", d.display());
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                assert_eq!(mode(&p), 0o600, "{}", p.display());
                files += 1;
            }
        }
    }
    assert_eq!(files, 7);
}

#[test]
fn rebuild_keeps_events_and_sources_and_leaves_a_backup() {
    let env = Env::new();
    env.write_session(&json!({
        "id": "keep", "cwd": PROJECT,
        "turns": [{"user": "Always run make check before pushing in this repo."}],
    }));
    env.ingest();
    env.extract(false);
    let mut s = env.store();
    mirror::sync(&mut s, &mirror_dir(&env.data), true).unwrap();
    drop(s);
    let (events, sources) = (count(&env.data, "events"), count(&env.data, "sources"));
    assert!(events > 0 && count(&env.data, "memories") > 0);
    let before = layer(&env.data);

    // Dry run (no --yes) changes nothing and exits 0.
    let out = recalla(&["rebuild", "--from-mirror"], &env.data);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("re-run with --yes"));
    assert!(!dir_listing(&env.data).iter().any(|n| n.contains(".bak-")));

    let out = recalla(&["rebuild", "--from-mirror", "--yes"], &env.data);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(layer(&env.data), before);
    assert_eq!(count(&env.data, "events"), events);
    assert_eq!(count(&env.data, "sources"), sources);
    let backups: Vec<String> = dir_listing(&env.data)
        .into_iter()
        .filter(|n| n.starts_with("recalla.db.bak-"))
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    let old: i64 = Connection::open(env.data.join(&backups[0]))
        .unwrap()
        .query_row("SELECT count(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(usize::try_from(old).unwrap(), before[0].len());
    assert!(
        !dir_listing(&env.data)
            .iter()
            .any(|n| n.contains(".rebuild-"))
    );

    // A second rebuild in the same second still gets its own backup.
    assert!(
        recalla(&["rebuild", "--from-mirror", "--yes"], &env.data)
            .status
            .success()
    );
    let n = dir_listing(&env.data)
        .iter()
        .filter(|n| n.starts_with("recalla.db.bak-"))
        .count();
    assert_eq!(n, 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&paths::db_path(&env.data)), 0o600);
        for b in dir_listing(&env.data)
            .iter()
            .filter(|n| n.starts_with("recalla.db.bak-"))
        {
            assert_eq!(mode(&env.data.join(b)), 0o600);
        }
    }
}

#[test]
fn sync_of_10k_memories_is_fast_enough() {
    let env = Env::new();
    let mut s = recalla::ops::open_or_create(&env.data).unwrap();
    let items: Vec<_> = (0..10_000u32)
        .map(|i| {
            (
                new_mem(
                    Kind::Fact,
                    &format!("Synthetic memory number {i} with some body text"),
                    Scope::Project(format!("/work/p{}", i % 20)),
                ),
                prov(i),
                vec![Anchor::new(format!("src/f{}.rs", i % 50))],
            )
        })
        .collect();
    s.insert_many(&items).unwrap();
    let dir = mirror_dir(&env.data);
    let t = Instant::now();
    let r = mirror::sync(&mut s, &dir, true).unwrap();
    let took = t.elapsed();
    println!("mirror full sync of 10k memories (debug build): {took:?}");
    assert_eq!(r.written, 10_000);
    // Debug builds on Windows runners are several times slower (file creation).
    let limit = if cfg!(windows) { 60 } else { 10 };
    assert!(took.as_secs() < limit, "{took:?}");
    let t = Instant::now();
    let r = mirror::sync(&mut s, &dir, false).unwrap();
    let noop = t.elapsed();
    println!("incremental no-op sync after full: {noop:?}");
    assert_eq!(r.written, 0);
    assert!(noop.as_secs() < 1, "{noop:?}");
    // The first incremental sync of a fresh mirror writes everything (every
    // memory is dirty); the next one is a no-op.
    let fresh = Env::new();
    let mut s2 = recalla::ops::open_or_create(&fresh.data).unwrap();
    s2.insert_many(&items).unwrap();
    let t = Instant::now();
    let r = mirror::sync(&mut s2, &mirror_dir(&fresh.data), false).unwrap();
    println!("incremental first sync of 10k memories: {:?}", t.elapsed());
    assert_eq!(r.written, 10_000);
    let t = Instant::now();
    assert_eq!(
        mirror::sync(&mut s2, &mirror_dir(&fresh.data), false)
            .unwrap()
            .written,
        0
    );
    let noop = t.elapsed();
    println!("incremental no-op sync: {noop:?}");
    assert!(noop.as_secs() < 1, "{noop:?}");
}

/// Writes a mirror file for `text` as a hostile editor would: a valid uid
/// (via `model::uid`) and a renderer-made file.
fn craft(dir: &Path, text: &str, scope: Scope) -> PathBuf {
    let uid = model::uid(Kind::Preference, &scope, text);
    let doc = FullMemory {
        memory: Memory {
            uid,
            kind: Kind::Preference,
            text: text.into(),
            scope,
            confidence: 0.8,
            importance: 2,
            status: Status::Active,
            superseded_by: None,
            seen_count: 1,
            created_at: 1,
            updated_at: 1,
            valid_from: 1,
            valid_to: None,
            provenance: vec![],
            anchors: vec![],
            stale_reason: None,
        },
        provenance_at: vec![],
        stale: vec![],
    };
    let p = mirror::memory_path(dir, &doc.memory);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, format::render(&doc)).unwrap();
    p
}

fn runtime_token() -> String {
    format!("ghp_{}", "aB3dE5gH7jK9mN1pQ3sT5vX7zA9cD1fG3hJ5")
}

#[test]
fn hostile_or_unredacted_files_refuse_the_rebuild() {
    let hostile = [
        format!("Use token {} for deploys", runtime_token()),
        "Always \u{1b}[2J clear the screen".to_owned(),
        "Plain looking \u{e0069}\u{e0067}\u{e006e} text".to_owned(),
        "Zero\u{200b}width and \u{202e}bidi".to_owned(),
    ];
    for text in &hostile {
        let env = Env::new();
        let mut s = seeded(&env.data);
        let dir = mirror_dir(&env.data);
        mirror::sync(&mut s, &dir, true).unwrap();
        drop(s);
        let before = layer(&env.data);
        let bad = craft(&dir, text, Scope::Global);
        for allow in [false, true] {
            let err = mirror::rebuild(&env.data, &dir, true, allow).unwrap_err();
            let RebuildError::Refused(problems) = &err else {
                panic!("{err}")
            };
            assert_eq!(problems.len(), 1, "{err}");
            assert_eq!(problems[0].path, bad);
            let msg = err.to_string();
            assert!(
                !msg.contains(&runtime_token()) && !msg.contains('\u{1b}'),
                "{msg}"
            );
        }
        assert_eq!(layer(&env.data), before);
        assert!(!dir_listing(&env.data).iter().any(|n| n.contains(".bak-")));
        fs::remove_file(&bad).unwrap();
    }
}

#[test]
fn out_of_range_values_refuse_the_rebuild() {
    for (from, to) in [
        ("confidence: 0.8", "confidence: 7"),
        ("importance: 2", "importance: 9"),
        ("seen_count: 1\n", "seen_count: 0\n"),
        (
            "created_at: 1970-01-01T00:00:00.001Z",
            "created_at: 1969-12-31T23:59:59.000Z",
        ),
    ] {
        let env = Env::new();
        let dir = mirror_dir(&env.data);
        let p = craft(&dir, "A fine preference", Scope::Global);
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.contains(from), "{text}");
        fs::write(&p, text.replace(from, to)).unwrap();
        assert!(
            matches!(
                mirror::rebuild(&env.data, &dir, true, false),
                Err(RebuildError::Refused(_))
            ),
            "{to}"
        );
        assert!(!paths::db_path(&env.data).exists());
    }
}

#[test]
fn a_legitimate_mirror_still_rebuilds_and_a_crafted_clean_file_too() {
    let env = Env::new();
    let dir = mirror_dir(&env.data);
    craft(&dir, "Prefer small functions", Scope::Global);
    let r = mirror::rebuild(&env.data, &dir, true, false).unwrap();
    assert!(r.applied && r.memories == 1 && r.global == 1);
}

#[test]
fn dry_run_reports_counts_and_what_would_be_dropped() {
    let env = Env::new();
    let mut s = seeded(&env.data);
    let dir = mirror_dir(&env.data);
    mirror::sync(&mut s, &dir, true).unwrap();
    let gone = mirror::md_files(&dir).unwrap().remove(0);
    fs::remove_file(&gone).unwrap();
    drop(s);
    let before = layer(&env.data);
    let out = recalla(&["rebuild", "--from-mirror"], &env.data);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("6 memories"), "{text}");
    assert!(text.contains("global"), "{text}");
    assert!(text.contains(PROJECT), "{text}");
    assert!(
        text.contains("1 memories exist in the DB but not in the mirror"),
        "{text}"
    );
    assert_eq!(layer(&env.data), before);
    // --yes alone is refused; nothing changes, no backup.
    let out = recalla(&["rebuild", "--from-mirror", "--yes"], &env.data);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--allow-drop"));
    assert_eq!(layer(&env.data), before);
    assert!(!dir_listing(&env.data).iter().any(|n| n.contains(".bak-")));
    // With --allow-drop it applies and keeps a backup holding the old rows.
    let out = recalla(
        &["rebuild", "--from-mirror", "--yes", "--allow-drop"],
        &env.data,
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(count(&env.data, "memories"), 6);
    assert!(dir_listing(&env.data).iter().any(|n| n.contains(".bak-")));
}

fn integrity(data: &Path) -> String {
    Connection::open(paths::db_path(data))
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap()
}

fn raw_insert(conn: &Connection, uid: &str) {
    conn.execute(
        "INSERT INTO memories (uid, kind, text, scope_key, confidence, importance, status,
             seen_count, created_at, updated_at, valid_from)
         VALUES (?1, 'fact', ?1, 'g', 0.5, 1, 'active', 1, 0, 0, 0)",
        [uid],
    )
    .unwrap();
}

#[test]
fn rebuild_while_another_connection_is_open_does_not_corrupt() {
    let env = Env::new();
    let mut s = seeded(&env.data);
    let dir = mirror_dir(&env.data);
    mirror::sync(&mut s, &dir, true).unwrap();
    // The "mcp server": an old connection that stays open across the rebuild.
    let old = Connection::open(paths::db_path(&env.data)).unwrap();
    old.busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let held: i64 = old
        .query_row("SELECT count(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(held, 7);
    drop(s);
    let out = recalla(&["rebuild", "--from-mirror", "--yes"], &env.data);
    assert!(out.status.success(), "{out:?}");
    // Writes through the old connection land in the live database.
    for i in 0..5 {
        raw_insert(&old, &format!("after-{i}"));
    }
    let seen: i64 = old
        .query_row("SELECT count(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(seen, 12);
    drop(old);
    assert_eq!(integrity(&env.data), "ok");
    assert_eq!(count(&env.data, "memories"), 12);
    assert_eq!(count(&env.data, "memories_fts_docsize"), 12);
}

#[test]
fn concurrent_rebuilds_do_not_corrupt() {
    let env = Env::new();
    let mut s = seeded(&env.data);
    let dir = mirror_dir(&env.data);
    mirror::sync(&mut s, &dir, true).unwrap();
    drop(s);
    let results: Vec<_> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4)
            .map(|_| sc.spawn(|| mirror::rebuild(&env.data, &dir, true, false)))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(results.iter().any(Result::is_ok), "{results:?}");
    for r in &results {
        // Busy is reported, never swallowed or corrupting.
        if let Err(e) = r {
            assert!(e.to_string().contains("sqlite"), "{e}");
        }
    }
    assert_eq!(integrity(&env.data), "ok");
    assert_eq!(count(&env.data, "memories"), 7);
}

#[test]
fn a_clock_skewed_memory_does_not_freeze_the_mirror() {
    let env = Env::new();
    let mut s = recalla::ops::open_or_create(&env.data).unwrap();
    let dir = mirror_dir(&env.data);
    let day = 86_400_000;
    s.set_clock(Box::new(move || 1_700_000_000_000 + 30 * day));
    s.insert(
        &new_mem(Kind::Fact, "from the future", Scope::Global),
        &prov(1),
    )
    .unwrap();
    assert_eq!(mirror::sync(&mut s, &dir, false).unwrap().written, 1);
    s.set_clock(Box::new(|| 1_700_000_000_000));
    for t in ["second", "third"] {
        s.insert(&new_mem(Kind::Fact, t, Scope::Global), &prov(2))
            .unwrap();
    }
    assert_eq!(mirror::sync(&mut s, &dir, false).unwrap().written, 2);
    assert_eq!(mirror::md_files(&dir).unwrap().len(), 3);
}

#[test]
fn interleaved_writers_and_syncs_never_lose_a_memory() {
    let env = Env::new();
    let mut a = recalla::ops::open_or_create(&env.data).unwrap();
    let mut b = Store::open(paths::db_path(&env.data)).unwrap();
    let dir = mirror_dir(&env.data);
    let mut total = 0;
    for i in 0..20u32 {
        let w = if i % 2 == 0 { &mut a } else { &mut b };
        w.insert(
            &new_mem(Kind::Fact, &format!("memory {i}"), Scope::Global),
            &prov(i),
        )
        .unwrap();
        total += 1;
        if i % 3 == 0 {
            let w = if i % 2 == 0 { &mut b } else { &mut a };
            mirror::sync(w, &dir, false).unwrap();
        }
    }
    mirror::sync(&mut a, &dir, false).unwrap();
    assert_eq!(mirror::md_files(&dir).unwrap().len(), total);
    assert_eq!(mirror::sync(&mut b, &dir, false).unwrap().written, 0);
}
