//! File-backed store behaviour across reopen.

#![allow(clippy::unwrap_used)]

use recalla::model::{Anchor, Kind, NewMemory, Origin, Provenance, Scope, Status};
use recalla::store::{Filter, Insert, Store, schema_version};

fn memory(text: &str) -> NewMemory {
    NewMemory {
        kind: Kind::Decision,
        text: text.into(),
        scope: Scope::Project("recalla".into()),
        confidence: 0.7,
        importance: 2,
    }
}

fn prov(session: &str, line: i64) -> Provenance {
    Provenance {
        harness: Some("claude-code".into()),
        session_id: Some(session.into()),
        line: Some(line),
        ..Provenance::new(Origin::Backfill)
    }
}

fn hits(store: &Store, q: &str) -> usize {
    store.search(q, &Filter::default(), 10).unwrap().len()
}

#[test]
fn data_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recalla.db");

    let uid = {
        let mut s = Store::open(&path).unwrap();
        let uid = s
            .insert(&memory("Store lives in SQLite"), &prov("s1", 1))
            .unwrap()
            .uid()
            .to_owned();
        s.add_anchor(&uid, &Anchor::new("src/store/mod.rs"))
            .unwrap();
        uid
    };

    let s = Store::open(&path).unwrap();
    assert_eq!(s.user_version().unwrap(), schema_version());
    let m = s.get(&uid).unwrap().unwrap();
    assert_eq!(m.text, "Store lives in SQLite");
    assert_eq!(m.anchors.len(), 1);
    assert_eq!(hits(&s, "sqlite"), 1);
}

#[test]
fn duplicate_across_reopen_merges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recalla.db");

    let uid = {
        let mut s = Store::open(&path).unwrap();
        s.insert(&memory("Use WAL"), &prov("s1", 1))
            .unwrap()
            .uid()
            .to_owned()
    };

    let mut s = Store::open(&path).unwrap();
    assert_eq!(
        s.insert(&memory("Use   WAL "), &prov("s2", 7)).unwrap(),
        Insert::Duplicate(uid.clone())
    );
    // Re-importing the same source is idempotent.
    s.insert(&memory("Use WAL"), &prov("s2", 7)).unwrap();

    let m = s.get(&uid).unwrap().unwrap();
    assert_eq!(m.seen_count, 3);
    assert_eq!(m.provenance.len(), 2);
    assert_eq!(hits(&s, "wal"), 1);
}

#[test]
fn supersede_and_restore_round_trip_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recalla.db");

    let (old, new) = {
        let mut s = Store::open(&path).unwrap();
        let old = s
            .insert(&memory("Use redb for storage"), &prov("s1", 1))
            .unwrap()
            .uid()
            .to_owned();
        let new = s
            .insert(&memory("Use rusqlite for storage"), &prov("s1", 2))
            .unwrap()
            .uid()
            .to_owned();
        s.supersede(&old, &new).unwrap();
        (old, new)
    };

    let mut s = Store::open(&path).unwrap();
    assert_eq!(hits(&s, "redb"), 0);
    assert_eq!(hits(&s, "storage"), 1);
    let m = s.get(&old).unwrap().unwrap();
    assert_eq!(m.status, Status::Superseded);
    assert_eq!(m.superseded_by.as_deref(), Some(new.as_str()));

    s.restore(&old).unwrap();
    assert_eq!(hits(&s, "redb"), 1);
    assert_eq!(hits(&s, "storage"), 2);
    let m = s.get(&old).unwrap().unwrap();
    assert_eq!(
        (m.status, m.superseded_by, m.valid_to),
        (Status::Active, None, None)
    );
}

#[test]
fn two_handles_share_one_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recalla.db");
    let mut a = Store::open(&path).unwrap();
    let b = Store::open(&path).unwrap();
    a.insert(&memory("visible to the other handle"), &prov("s1", 1))
        .unwrap();
    assert_eq!(hits(&b, "visible"), 1);
}
