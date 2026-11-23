//! Dashboard data layer (`store::ui`) over a DB built by the real pipeline:
//! pagination, filters, sanitization, savings, details, and old rows.

#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::cast_precision_loss
)]

mod common;

use std::collections::HashSet;

use common::Env;
use recalla::model::{Anchor, Kind, NewMemory, Origin, Provenance, Scope, Status};
use recalla::store::{
    Checkpoint, Cursor, InjectionFilter, MemoryFilter, MemorySort, NewInjection, Page,
    SessionFilter, Store, StoredEvent, StoredKind, Via,
};

const HOSTILE: &str = "ESC\u{1b}[2J BIDI\u{202e}x TAG\u{e0041}\u{e0042} ZW\u{200b}!";
const HOSTILE_SESSION: &str = "hostile-session";
const HOSTILE_CWD: &str = "/work/hostile";
const WEB: &str = "eval-web-api";

fn clean(s: &str) -> bool {
    !s.chars().any(|c| {
        (c.is_control() && c != '\n' && c != '\t')
            || matches!(c, '\u{202e}' | '\u{200b}' | '\u{e0041}')
    })
}

fn event(line: u64, kind: StoredKind, text: &str) -> StoredEvent {
    StoredEvent {
        harness: "claude-code".into(),
        session_id: HOSTILE_SESSION.into(),
        message_uuid: None,
        parent_uuid: None,
        line,
        seq: 0,
        byte_offset: line * 10,
        ts: Some(1_900_000_000_000 + i64::try_from(line).unwrap()),
        cwd: Some(HOSTILE_CWD.into()),
        git_branch: None,
        sidechain: false,
        kind,
        tool_name: None,
        tool_id: None,
        is_error: false,
        text: text.into(),
    }
}

struct Fx {
    _env: Env,
    store: Store,
    long_event_len: usize,
    hostile_uid: String,
}

fn fixture() -> Fx {
    let env = Env::new();
    env.write_all();
    env.backfill();
    let mut store = env.store();

    // A session the pipeline can't produce: hostile text, multiline, long.
    let long = format!("line one\nline two {HOSTILE}\n{}", "x".repeat(5000));
    let id = store
        .upsert_source("claude-code", "/s/dir/hostile.jsonl")
        .unwrap();
    let events = [
        event(1, StoredKind::UserText, &format!("hi\n{HOSTILE}\nbye")),
        event(2, StoredKind::AssistantText, &long),
        event(3, StoredKind::ToolResult, "ok"),
    ];
    let cp = Checkpoint {
        identity: "i".into(),
        tail_hash: "t".into(),
        size: 100,
        next_offset: 100,
    };
    store.ingest_batch(id, &events, &cp, None, false).unwrap();

    let hostile = NewMemory {
        kind: Kind::Gotcha,
        text: format!("hostile memory\nsecond line {HOSTILE}"),
        scope: Scope::Project(HOSTILE_CWD.into()),
        confidence: 0.5,
        importance: 1,
    };
    let prov = Provenance {
        harness: Some("claude-code".into()),
        session_id: Some(HOSTILE_SESSION.into()),
        source_path: Some("/s/dir/hostile.jsonl".into()),
        line: Some(1),
        ..Provenance::new(Origin::Backfill)
    };
    let hostile_uid = store.insert(&hostile, &prov).unwrap().uid().to_owned();
    store
        .add_anchor(&hostile_uid, &Anchor::new("src/h.rs"))
        .unwrap();

    // Injections: some with session ids and pack text, one pre-v8 style.
    let uids: Vec<String> = store
        .all_memories(&[Status::Active])
        .unwrap()
        .into_iter()
        .take(3)
        .map(|m| m.uid)
        .collect();
    for i in 0..9 {
        let session = if i % 3 == 0 { Some(WEB) } else { None };
        store
            .record_injection(&NewInjection {
                session_id: session,
                pack_text: Some("# Recalla memory\n- a thing [abcd1234]"),
                candidate_count: Some(10),
                candidate_tokens: Some(400),
                ..NewInjection::new(
                    Some("/tmp/eval/web-api"),
                    1500,
                    100,
                    &uids,
                    if i % 2 == 0 { Via::Hook } else { Via::Mcp },
                )
            })
            .unwrap();
    }
    store
        .record_injection(&NewInjection::new(None, 800, 10, &[], Via::Cli))
        .unwrap();
    Fx {
        _env: env,
        store,
        long_event_len: long.len(),
        hostile_uid,
    }
}

/// Walks every page with `limit`, returning all rows; asserts cursors end.
fn walk<T>(mut fetch: impl FnMut(Option<&Cursor>) -> Page<T>) -> Vec<T> {
    let (mut all, mut cursor) = (Vec::new(), None);
    for _ in 0..10_000 {
        let page = fetch(cursor.as_ref());
        all.extend(page.rows);
        match page.next {
            Some(c) => cursor = Some(c),
            None => return all,
        }
    }
    panic!("pagination did not terminate");
}

fn assert_unique<T: std::hash::Hash + Eq + std::fmt::Debug>(items: impl Iterator<Item = T>) {
    let items: Vec<T> = items.collect();
    let set: HashSet<&T> = items.iter().collect();
    assert_eq!(set.len(), items.len(), "duplicates in {items:?}");
}

#[test]
fn sessions_paginate_exactly_once_in_order() {
    let fx = fixture();
    let f = SessionFilter::default();
    let all = fx.store.ui_sessions(&f, None, 200).unwrap();
    assert!(all.next.is_none());
    assert_eq!(all.rows.len(), 12, "11 fixtures + hostile");
    for limit in [1, 2, 5] {
        let walked = walk(|c| fx.store.ui_sessions(&f, c, limit).unwrap());
        let key = |r: &recalla::store::SessionRow| (r.harness.clone(), r.session_id.clone());
        assert_eq!(
            walked.iter().map(key).collect::<Vec<_>>(),
            all.rows.iter().map(key).collect::<Vec<_>>(),
            "limit {limit}"
        );
        assert_unique(walked.iter().map(key));
    }
    assert!(all.rows.windows(2).all(|w| w[0].last_ts >= w[1].last_ts));
    let web = all.rows.iter().find(|r| r.session_id == WEB).unwrap();
    assert_eq!(web.harness, "claude-code");
    assert_eq!(web.project.as_deref(), Some("/tmp/eval/web-api"));
    assert!(web.event_count > 5 && web.memory_count > 0);
    assert_eq!(web.injection_count, 3);
    assert_eq!(web.sources, ["eval-web-api.jsonl"]);
    let sub = all
        .rows
        .iter()
        .find(|r| r.project.as_deref() == Some("/tmp/eval/sub"));
    assert!(sub.unwrap().sidechain_event_count > 0);
}

#[test]
fn session_filters_and_hostile_strings_are_inert() {
    let fx = fixture();
    let ask = |f: SessionFilter| fx.store.ui_sessions(&f, None, 50).unwrap().rows;
    let by_h = ask(SessionFilter {
        harness: Some("claude-code".into()),
        ..Default::default()
    });
    assert_eq!(by_h.len(), 12);
    assert!(
        ask(SessionFilter {
            harness: Some("codex".into()),
            ..Default::default()
        })
        .is_empty()
    );
    let by_p = ask(SessionFilter {
        project: Some("/tmp/eval/web-api".into()),
        ..Default::default()
    });
    assert_eq!(by_p.len(), 1);
    let by_q = ask(SessionFilter {
        q: Some("WEB-API".into()),
        ..Default::default()
    });
    assert_eq!(by_q.len(), 1);
    for evil in ["'; DROP TABLE events; --", "%", "\" OR 1=1 --", "\0"] {
        for f in [
            SessionFilter {
                q: Some(evil.into()),
                ..Default::default()
            },
            SessionFilter {
                project: Some(evil.into()),
                ..Default::default()
            },
            SessionFilter {
                harness: Some(evil.into()),
                ..Default::default()
            },
        ] {
            assert!(ask(f).is_empty(), "{evil:?}");
        }
    }
    assert_eq!(ask(SessionFilter::default()).len(), 12, "tables intact");
    let bad = Cursor::parse("nonsense").unwrap();
    assert!(
        fx.store
            .ui_sessions(&SessionFilter::default(), Some(&bad), 5)
            .is_err()
    );
    assert!(Cursor::parse("").is_err() && Cursor::parse("a\nb").is_err());
}

#[test]
fn memories_paginate_exactly_once_for_every_sort() {
    let fx = fixture();
    let total = fx
        .store
        .all_memories(&[Status::Active, Status::NeedsReverify])
        .unwrap()
        .len();
    for sort in [
        MemorySort::Updated,
        MemorySort::Confidence,
        MemorySort::Seen,
        MemorySort::Created,
    ] {
        let f = MemoryFilter {
            sort,
            ..Default::default()
        };
        let all = fx.store.ui_memories(&f, None, 200).unwrap();
        assert_eq!(all.rows.len(), total, "{sort:?}");
        for limit in [1, 7] {
            let walked = walk(|c| fx.store.ui_memories(&f, c, limit).unwrap());
            let uids = |rows: &[recalla::store::MemoryRow]| {
                rows.iter().map(|r| r.uid.clone()).collect::<Vec<_>>()
            };
            assert_eq!(uids(&walked), uids(&all.rows), "{sort:?} limit {limit}");
            assert_unique(walked.iter().map(|r| r.uid.clone()));
        }
        let key = |r: &recalla::store::MemoryRow| match sort {
            MemorySort::Updated => r.updated_at as f64,
            MemorySort::Created => r.created_at as f64,
            MemorySort::Seen => r.seen_count as f64,
            MemorySort::Confidence => f64::from(r.confidence),
        };
        assert!(
            all.rows.windows(2).all(|w| key(&w[0]) >= key(&w[1])),
            "{sort:?}"
        );
    }
}

#[test]
fn memory_filters_search_and_hostile_strings() {
    let fx = fixture();
    let ask = |f: MemoryFilter| fx.store.ui_memories(&f, None, 200).unwrap().rows;
    let default = ask(MemoryFilter::default());
    let prefs = ask(MemoryFilter {
        kinds: vec![Kind::Preference],
        ..Default::default()
    });
    assert!(!prefs.is_empty() && prefs.iter().all(|m| m.kind == Kind::Preference));
    assert!(prefs.len() < default.len());
    assert!(
        ask(MemoryFilter {
            statuses: vec![Status::Archived],
            ..Default::default()
        })
        .is_empty()
    );
    let web = ask(MemoryFilter {
        project: Some("/tmp/eval/web-api".into()),
        ..Default::default()
    });
    assert!(!web.is_empty());
    assert!(
        web.iter()
            .all(|m| m.scope_project.as_deref() == Some("/tmp/eval/web-api"))
    );
    let claude = ask(MemoryFilter {
        harness: Some("claude-code".into()),
        ..Default::default()
    });
    assert_eq!(
        claude.len(),
        default.len(),
        "every memory came from claude-code"
    );
    let hits = ask(MemoryFilter {
        q: Some("token bucket".into()),
        ..Default::default()
    });
    assert!(!hits.is_empty() && hits.iter().any(|m| m.text.contains("token bucket")));
    assert!(hits.len() < default.len());
    // Blank q is no filter.
    assert_eq!(
        ask(MemoryFilter {
            q: Some("  ".into()),
            ..Default::default()
        })
        .len(),
        default.len()
    );
    for evil in [
        "'; DROP TABLE memories; --",
        "\" OR 1=1",
        "NEAR(",
        "*",
        "a\0b",
    ] {
        for f in [
            MemoryFilter {
                q: Some(evil.into()),
                ..Default::default()
            },
            MemoryFilter {
                project: Some(evil.into()),
                ..Default::default()
            },
            MemoryFilter {
                harness: Some(evil.into()),
                ..Default::default()
            },
        ] {
            assert!(ask(f).is_empty(), "{evil:?}");
        }
    }
    assert_eq!(
        ask(MemoryFilter::default()).len(),
        default.len(),
        "tables intact"
    );
}

#[test]
fn archived_and_stale_memories_show_status_and_reason() {
    let mut fx = fixture();
    let uid = fx.hostile_uid.clone();
    fx.store.set_status(&uid, Status::NeedsReverify).unwrap();
    let anchor_id = fx.store.anchors_for_project(HOSTILE_CWD).unwrap()[0].1;
    fx.store.mark_stale(anchor_id, "symbol changed: h").unwrap();
    let rows = fx
        .store
        .ui_memories(
            &MemoryFilter {
                project: Some(HOSTILE_CWD.into()),
                ..Default::default()
            },
            None,
            10,
        )
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, Status::NeedsReverify);
    assert_eq!(rows[0].stale_reason.as_deref(), Some("symbol changed: h"));
    assert_eq!(rows[0].anchor_count, 1);
    fx.store.archive(&uid).unwrap();
    let archived = MemoryFilter {
        statuses: vec![Status::Archived],
        ..Default::default()
    };
    assert_eq!(
        fx.store
            .ui_memories(&archived, None, 10)
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(
        fx.store
            .ui_memories(
                &MemoryFilter {
                    project: Some(HOSTILE_CWD.into()),
                    ..Default::default()
                },
                None,
                10
            )
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn limit_is_clamped() {
    let fx = fixture();
    let zero = fx
        .store
        .ui_memories(&MemoryFilter::default(), None, 0)
        .unwrap();
    assert_eq!(zero.rows.len(), 1);
    assert!(zero.next.is_some());
    let huge = fx
        .store
        .ui_memories(&MemoryFilter::default(), None, 10_000)
        .unwrap();
    assert!(huge.rows.len() <= 200 && huge.rows.len() > 1);
    assert_eq!(
        fx.store
            .ui_sessions(&SessionFilter::default(), None, 0)
            .unwrap()
            .rows
            .len(),
        1
    );
    assert_eq!(
        fx.store
            .ui_injections(&InjectionFilter::default(), None, 0)
            .unwrap()
            .rows
            .len(),
        1
    );
    assert_eq!(
        fx.store
            .ui_session_events("claude-code", WEB, None, 0)
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn events_paginate_and_expand_lazily() {
    let fx = fixture();
    let sessions = fx
        .store
        .ui_sessions(&SessionFilter::default(), None, 200)
        .unwrap()
        .rows;
    let web = sessions.iter().find(|r| r.session_id == WEB).unwrap();
    let mut all = Vec::new();
    let mut after = None;
    loop {
        let page = fx
            .store
            .ui_session_events("claude-code", WEB, after, 4)
            .unwrap();
        all.extend(page.rows);
        match page.next {
            Some(c) => after = Some(c.as_str().parse::<i64>().unwrap()),
            None => break,
        }
    }
    assert_eq!(all.len() as u64, web.event_count);
    assert_unique(all.iter().map(|e| e.id));
    assert!(all.windows(2).all(|w| w[0].id < w[1].id));
    assert!(
        fx.store
            .ui_session_events("claude-code", "nope", None, 10)
            .unwrap()
            .rows
            .is_empty()
    );

    let evs = fx
        .store
        .ui_session_events("claude-code", HOSTILE_SESSION, None, 10)
        .unwrap()
        .rows;
    assert_eq!(evs.len(), 3);
    assert!(evs.iter().all(|e| clean(&e.text)), "{evs:?}");
    assert!(
        evs[0].text.contains("hi\n") && evs[0].text.contains("\nbye"),
        "newlines kept"
    );
    assert!(!evs[0].truncated);
    assert!(evs[1].truncated && (1900..=2000).contains(&evs[1].text.chars().count()));
    let full = fx.store.ui_event_text(evs[1].id).unwrap().unwrap();
    assert!(clean(&full) && full.chars().count() > 2000);
    assert!(full.len() <= fx.long_event_len && full.contains("line two"));
    assert!(fx.store.ui_event_text(-1).unwrap().is_none());
}

#[test]
fn text_from_logs_is_sanitized_everywhere() {
    let fx = fixture();
    let rows = fx
        .store
        .ui_memories(
            &MemoryFilter {
                project: Some(HOSTILE_CWD.into()),
                ..Default::default()
            },
            None,
            10,
        )
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert!(
        clean(&rows[0].text) && !rows[0].text.contains('\n'),
        "{:?}",
        rows[0].text
    );
    let d = fx.store.ui_memory_detail(&fx.hostile_uid).unwrap().unwrap();
    assert!(clean(&d.text_full) && d.text_full.contains("hostile memory\nsecond line"));
    let excerpt = d.provenance[0].excerpt.as_deref().unwrap();
    assert!(clean(excerpt) && excerpt.starts_with("hi"), "{excerpt:?}");
    assert_eq!(
        d.provenance[0].source.file.as_deref(),
        Some("hostile.jsonl")
    );
    let s = fx
        .store
        .ui_session_detail("claude-code", HOSTILE_SESSION)
        .unwrap()
        .unwrap();
    assert_eq!(s.session.sources, ["hostile.jsonl"]);
    assert_eq!(s.memories.len(), 1);
    assert!(clean(&s.memories[0].text));
}

#[test]
fn injections_paginate_and_report_savings() {
    let fx = fixture();
    let f = InjectionFilter::default();
    let all = fx.store.ui_injections(&f, None, 200).unwrap().rows;
    assert_eq!(all.len(), 10);
    for limit in [1, 3, 4] {
        let walked = walk(|c| fx.store.ui_injections(&f, c, limit).unwrap());
        assert_eq!(
            walked.iter().map(|r| r.id).collect::<Vec<_>>(),
            all.iter().map(|r| r.id).collect::<Vec<_>>()
        );
    }
    assert!(all.windows(2).all(|w| w[0].id > w[1].id));
    let saved = all
        .iter()
        .find(|r| r.candidate_tokens == Some(400))
        .unwrap();
    assert_eq!((saved.est_tokens, saved.saved_pct), (100, Some(75.0)));
    assert_eq!((saved.candidate_count, saved.memory_count), (Some(10), 3));

    let old = all.last().unwrap_or(&all[0]);
    let old = all
        .iter()
        .find(|r| r.candidate_tokens.is_none())
        .unwrap_or(old);
    assert_eq!((old.saved_pct, old.session_id.clone()), (None, None));
    let d = fx.store.ui_injection_detail(old.id).unwrap().unwrap();
    assert!(d.pack_text.is_none() && d.memories.is_empty());
    let d = fx.store.ui_injection_detail(saved.id).unwrap().unwrap();
    assert_eq!(
        d.pack_text.as_deref(),
        Some("# Recalla memory\n- a thing [abcd1234]")
    );
    assert_eq!(d.memories.len(), 3);
    assert!(fx.store.ui_injection_detail(9999).unwrap().is_none());

    let hook = fx
        .store
        .ui_injections(
            &InjectionFilter {
                via: Some("hook".into()),
                ..Default::default()
            },
            None,
            50,
        )
        .unwrap()
        .rows;
    assert!(!hook.is_empty() && hook.iter().all(|r| r.via == "hook"));
    let web = fx
        .store
        .ui_injections(
            &InjectionFilter {
                session_id: Some(WEB.into()),
                ..Default::default()
            },
            None,
            50,
        )
        .unwrap()
        .rows;
    assert_eq!(web.len(), 3);
    let proj = fx
        .store
        .ui_injections(
            &InjectionFilter {
                project: Some("/tmp/eval/web-api".into()),
                ..Default::default()
            },
            None,
            50,
        )
        .unwrap()
        .rows;
    assert_eq!(proj.len(), 9);
    for evil in ["'; DROP TABLE injections; --", "%"] {
        let f = InjectionFilter {
            project: Some(evil.into()),
            via: Some(evil.into()),
            session_id: Some(evil.into()),
        };
        assert!(
            fx.store
                .ui_injections(&f, None, 50)
                .unwrap()
                .rows
                .is_empty()
        );
    }
    assert_eq!(
        fx.store
            .ui_injections(&InjectionFilter::default(), None, 50)
            .unwrap()
            .rows
            .len(),
        10
    );
}

#[test]
fn memory_detail_lists_injections_and_resolves_prefixes() {
    let fx = fixture();
    let injected = fx
        .store
        .ui_injection_detail(
            fx.store
                .ui_injections(&InjectionFilter::default(), None, 1)
                .unwrap()
                .rows
                .iter()
                .find(|r| r.memory_count > 0)
                .map_or(1, |r| r.id),
        )
        .unwrap()
        .unwrap()
        .memories[0]
        .uid
        .clone();
    let d = fx.store.ui_memory_detail(&injected).unwrap().unwrap();
    assert_eq!(d.injections.len(), 9);
    assert!(d.injections.windows(2).all(|w| w[0].id > w[1].id));
    assert!(!d.provenance.is_empty() && !d.text_full.is_empty());
    let p = &d.provenance[0];
    assert!(p.excerpt.is_some(), "{p:?}");
    assert!(p.source.file.as_deref().is_some_and(|f| !f.contains('/')));
    assert!(d.row.first_source.is_some());

    let by_prefix = fx.store.ui_memory_detail(&injected[..8]).unwrap().unwrap();
    assert_eq!(by_prefix.row.uid, injected);
    assert!(fx.store.ui_memory_detail(&injected[..3]).unwrap().is_none());
    assert!(fx.store.ui_memory_detail("zzzz").unwrap().is_none());
    assert!(fx.store.ui_memory_detail("' OR 1=1 --").unwrap().is_none());
    assert!(fx.store.ui_memory_detail("00000000ffff").unwrap().is_none());
    assert!(fx.store.ui_memory_detail("").unwrap().is_none());

    let anchored = fx.store.ui_memory_detail(&fx.hostile_uid).unwrap().unwrap();
    assert_eq!(anchored.anchors.len(), 1);
    assert!(anchored.injections.is_empty());
}

#[test]
fn session_detail_has_memories_and_injections() {
    let fx = fixture();
    let d = fx
        .store
        .ui_session_detail("claude-code", WEB)
        .unwrap()
        .unwrap();
    assert_eq!(d.session.session_id, WEB);
    assert_eq!(d.memories.len() as u64, d.session.memory_count);
    assert_eq!(d.injections.len(), 3);
    assert!(
        d.injections
            .iter()
            .all(|i| i.session_id.as_deref() == Some(WEB))
    );
    assert!(
        fx.store
            .ui_session_detail("claude-code", "nope")
            .unwrap()
            .is_none()
    );
    assert!(fx.store.ui_session_detail("codex", WEB).unwrap().is_none());
}

#[test]
fn overview_counts_match() {
    let fx = fixture();
    let o = fx.store.ui_overview().unwrap();
    assert_eq!(o.sessions, 12);
    assert_eq!(o.injections, 10);
    assert_eq!(
        (o.injected_tokens, o.candidate_tokens),
        (9 * 100 + 10, 9 * 400)
    );
    assert!(o.events > 50 && o.sources >= 12);
    assert_eq!(o.stale, 0);
    assert!(o.memories.iter().map(|m| m.2).sum::<u64>() > 20);
    assert_eq!(o.last_injection_via.as_deref(), Some("cli"));
    assert!(o.last_event_ts.is_some() && o.last_injection_at.is_some());
}
