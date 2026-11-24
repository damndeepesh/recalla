//! Incremental extraction: stored events in id order through the rules into
//! memories, checkpointed after every batch.
//!
//! A crash loses at most one batch of progress; replaying it is harmless
//! because inserts are idempotent through the memory uid.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::time::Instant;

use serde_json::{Value, json};

use super::conflict::{Analyzed, detect};
use super::rules::task;
use super::scope::{project_key, project_scope};
use super::{EXTRACTOR_VERSION, EventCtx, extract_event_with};
use crate::anchor;
use crate::model::{self, Kind, NewMemory, Origin, Provenance, Scope, Status};
use crate::ops::CONFIRMED_CONFIDENCE;
use crate::store::{EventRow, Insert, Store, StoreError, StoredKind};

/// Events read per batch (and per checkpoint).
const BATCH: u32 = 1000;
/// Previous same-session events handed to the rules.
const WINDOW: usize = 8;
const WINDOW_U32: u32 = 8;

/// What to extract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Archive backfill-only memories and re-derive everything from event 0.
    pub rebuild: bool,
}

/// Counts from one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub events_processed: u64,
    pub memories_new: BTreeMap<String, u64>,
    pub memories_duplicate: BTreeMap<String, u64>,
    pub tasks_archived: u64,
    /// Older rules replaced by a newer contradicting one.
    pub conflicts_superseded: u64,
    /// Contradicting pairs left in place and flagged `needs_reverify`.
    pub conflicts_flagged: u64,
    /// True when the run restarted from event 0 (rebuild or version bump).
    pub restarted: bool,
    pub memories_archived_for_rebuild: u64,
    pub duration_ms: u64,
}

impl Report {
    /// Machine-readable form (`recalla extract --json`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "events_processed": self.events_processed,
            "memories_new": self.memories_new,
            "memories_duplicate": self.memories_duplicate,
            "tasks_archived": self.tasks_archived,
            "conflicts_superseded": self.conflicts_superseded,
            "conflicts_flagged": self.conflicts_flagged,
            "restarted": self.restarted,
            "memories_archived_for_rebuild": self.memories_archived_for_rebuild,
            "duration_ms": self.duration_ms,
        })
    }

    /// Total new memories across kinds.
    #[must_use]
    pub fn new_total(&self) -> u64 {
        self.memories_new.values().sum()
    }

    /// Total duplicate hits across kinds.
    #[must_use]
    pub fn duplicate_total(&self) -> u64 {
        self.memories_duplicate.values().sum()
    }
}

/// Extracts memories from every event past the checkpoint.
///
/// # Errors
/// Any store failure; progress up to the last committed batch is kept.
pub fn run(store: &mut Store, opts: &Options) -> Result<Report, StoreError> {
    let started = Instant::now();
    let mut report = Report::default();
    let (mut last_id, version) = store.extract_checkpoint()?;
    let restart = opts.rebuild || version < EXTRACTOR_VERSION;
    if restart {
        report.memories_archived_for_rebuild = store.archive_backfill_only_memories()?;
        last_id = 0;
        report.restarted = true;
    }
    let mut rules = RuleIndex::load(store)?;
    let mut windows: HashMap<String, VecDeque<EventRow>> = HashMap::new();
    let projects: RefCell<HashMap<String, String>> = RefCell::default();
    let project_of = |cwd: &str| {
        projects
            .borrow_mut()
            .entry(cwd.to_owned())
            .or_insert_with(|| project_key(cwd))
            .clone()
    };
    loop {
        let batch = store.events_after(last_id, BATCH)?;
        let Some(last) = batch.last() else { break };
        let next_id = last.id;
        for event in &batch {
            if !windows.contains_key(&event.session_id) {
                let seed =
                    store.events_before_in_session(&event.session_id, event.id, WINDOW_U32)?;
                windows.insert(event.session_id.clone(), seed.into());
            }
            let window = windows.entry(event.session_id.clone()).or_default();
            let prev: Vec<EventRow> = window.iter().cloned().collect();
            let ctx = EventCtx { event, prev: &prev };
            process(store, &ctx, restart, &project_of, &mut rules, &mut report)?;
            if window.len() == WINDOW {
                window.pop_front();
            }
            window.push_back(event.clone());
            report.events_processed += 1;
        }
        last_id = next_id;
        store.set_extract_checkpoint(last_id, EXTRACTOR_VERSION)?;
    }
    if report.events_processed == 0 && restart {
        store.set_extract_checkpoint(last_id, EXTRACTOR_VERSION)?;
    }
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(report)
}

fn process(
    store: &mut Store,
    ctx: &EventCtx<'_>,
    restoring: bool,
    project_of: &impl Fn(&str) -> String,
    rules: &mut RuleIndex,
    report: &mut Report,
) -> Result<(), StoreError> {
    let e = ctx.event;
    for c in extract_event_with(ctx, project_of) {
        let new = NewMemory {
            kind: c.kind,
            text: c.text,
            scope: c.scope,
            confidence: c.confidence,
            importance: c.importance,
        };
        let prov = Provenance {
            origin: Origin::Backfill,
            harness: Some(e.harness.clone()),
            session_id: Some(e.session_id.clone()),
            message_uuid: e.message_uuid.clone(),
            source_path: Some(e.source_path.clone()),
            line: i64::try_from(e.line).ok(),
            git_commit: None,
        };
        let outcome = store.insert(&new, &prov)?;
        let counts = match &outcome {
            Insert::Inserted(_) => &mut report.memories_new,
            Insert::Duplicate(_) => &mut report.memories_duplicate,
        };
        *counts.entry(new.kind.to_string()).or_default() += 1;
        let root = matches!(outcome, Insert::Inserted(_))
            .then(|| anchor_root(&new.scope))
            .flatten();
        if let Some(root) = root {
            let paths: Vec<String> = c.anchors.iter().map(|a| a.path.clone()).collect();
            let window = window_files(ctx);
            let resolved = anchor::resolve_with(&new.text, &paths, &window, Path::new(root));
            store.replace_anchors(outcome.uid(), &resolved)?;
        } else {
            for anchor in &c.anchors {
                store.add_anchor(outcome.uid(), anchor)?;
            }
        }
        // A rebuild archived this memory moments ago; extracting it again
        // means it is still wanted.
        let restored = restoring
            && matches!(outcome, Insert::Duplicate(_))
            && store
                .get(outcome.uid())?
                .is_some_and(|m| m.status == Status::Archived);
        if restored {
            store.restore(outcome.uid())?;
        }
        if matches!(outcome, Insert::Inserted(_)) || restored {
            rules.resolve(store, outcome.uid(), &new, report)?;
        }
    }
    archive_finished_tasks(store, ctx, project_of, report)
}

/// A live preference or decision, as the contradiction check sees it.
struct Rule {
    uid: String,
    scope: Scope,
    confidence: f32,
    analyzed: Analyzed,
}

/// Live rule memories kept in memory for one run, so each new rule is
/// compared with its scope's rules without a query per insert.
struct RuleIndex(Vec<Rule>);

impl RuleIndex {
    fn load(store: &Store) -> Result<Self, StoreError> {
        Ok(Self(
            store
                .live_rules()?
                .into_iter()
                .map(|m| Rule {
                    analyzed: Analyzed::new(&m.text),
                    uid: m.uid,
                    scope: m.scope,
                    confidence: m.confidence,
                })
                .collect(),
        ))
    }

    /// Compares a just-stored rule with the live rules of its scope (D-028).
    /// A newer rule that is at least as confident supersedes an unconfirmed
    /// older one; any other contradiction flags the newcomer (and the older
    /// rule too, unless it is confirmed) for re-verification.
    fn resolve(
        &mut self,
        store: &mut Store,
        uid: &str,
        new: &NewMemory,
        report: &mut Report,
    ) -> Result<(), StoreError> {
        if !matches!(new.kind, Kind::Preference | Kind::Decision) {
            return Ok(());
        }
        let analyzed = Analyzed::new(&new.text);
        let mut gone: Vec<String> = Vec::new();
        for old in &self.0 {
            if old.uid == uid
                || old.scope != new.scope
                || detect(&old.analyzed, &analyzed).is_none()
            {
                continue;
            }
            if new.confidence >= old.confidence && old.confidence < CONFIRMED_CONFIDENCE {
                store.supersede(&old.uid, uid)?;
                store.add_relation(uid, &old.uid, crate::store::CONFLICTS)?;
                gone.push(old.uid.clone());
                report.conflicts_superseded += 1;
            } else {
                // A confirmed rule is the user's word: automatic detection may
                // flag the newcomer but never demotes it.
                store.add_relation(uid, &old.uid, crate::store::CONFLICTS)?;
                if old.confidence < CONFIRMED_CONFIDENCE {
                    store.set_status(&old.uid, Status::NeedsReverify)?;
                }
                store.set_status(uid, Status::NeedsReverify)?;
                report.conflicts_flagged += 1;
            }
        }
        self.0.retain(|r| !gone.contains(&r.uid));
        if !self.0.iter().any(|r| r.uid == uid) {
            self.0.push(Rule {
                uid: uid.to_owned(),
                scope: new.scope.clone(),
                confidence: new.confidence,
                analyzed,
            });
        }
        Ok(())
    }
}

/// Files the session read or edited in the window (previous events plus the
/// triggering one): `file_path`/`path`/`notebook_path` of Read, Edit, Write
/// and `MultiEdit` calls. Candidates for symbol anchors only.
fn window_files(ctx: &EventCtx<'_>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in ctx.prev.iter().chain(std::iter::once(ctx.event)) {
        let is_file_tool = e.kind == StoredKind::ToolCall
            && matches!(
                e.tool_name.as_deref(),
                Some("Read" | "Edit" | "Write" | "MultiEdit")
            );
        if !is_file_tool {
            continue;
        }
        let Ok(input) = serde_json::from_str::<Value>(&e.text) else {
            continue;
        };
        let file = ["file_path", "path", "notebook_path"]
            .iter()
            .find_map(|k| input.get(k)?.as_str())
            .filter(|p| !p.is_empty());
        if let Some(f) = file
            && !out.iter().any(|o| o == f)
        {
            out.push(f.to_owned());
        }
    }
    out
}

/// The project directory to anchor a memory under: its Project/Module key,
/// when that is an absolute existing dir.
fn anchor_root(scope: &Scope) -> Option<&str> {
    match scope {
        Scope::Project(key) | Scope::Module { project: key, .. } => {
            anchor::project_dir(key).map(|_| key.as_str())
        }
        Scope::Global => None,
    }
}

/// Archives the open task memory of every item this event completes or deletes.
fn archive_finished_tasks(
    store: &mut Store,
    ctx: &EventCtx<'_>,
    project_of: &impl Fn(&str) -> String,
    report: &mut Report,
) -> Result<(), StoreError> {
    let finished = task::finished_items(ctx);
    if finished.is_empty() {
        return Ok(());
    }
    let project = ctx
        .event
        .cwd
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(project_of);
    let scope = project_scope(project.as_deref());
    for text in finished {
        let uid = model::uid(Kind::Task, &scope, &text);
        if store.get(&uid)?.is_some_and(|m| m.status == Status::Active) {
            store.archive(&uid)?;
            report.tasks_archived += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::why;

    fn add(store: &mut Store, kind: Kind, text: &str, confidence: f32) -> (NewMemory, String) {
        let new = NewMemory {
            kind,
            text: text.to_owned(),
            scope: Scope::Project("/p".to_owned()),
            confidence,
            importance: 1,
        };
        let prov = Provenance {
            origin: Origin::Backfill,
            harness: None,
            session_id: None,
            message_uuid: None,
            source_path: None,
            line: None,
            git_commit: None,
        };
        let uid = store.insert(&new, &prov).unwrap().uid().to_owned();
        (new, uid)
    }

    fn status(store: &Store, uid: &str) -> Status {
        store.get(uid).unwrap().unwrap().status
    }

    fn resolve(store: &mut Store, rules: &mut RuleIndex, m: &(NewMemory, String)) -> Report {
        let mut r = Report::default();
        rules.resolve(store, &m.1, &m.0, &mut r).unwrap();
        r
    }

    #[test]
    fn newer_equally_confident_rule_supersedes_the_older_and_confirm_undoes_it() {
        let mut s = Store::open_in_memory().unwrap();
        let mut rules = RuleIndex::load(&s).unwrap();
        let old = add(&mut s, Kind::Preference, "Use tabs for indentation", 0.8);
        assert_eq!(resolve(&mut s, &mut rules, &old).conflicts_superseded, 0);
        let new = add(&mut s, Kind::Preference, "Use spaces for indentation", 0.8);
        let r = resolve(&mut s, &mut rules, &new);
        assert_eq!(r.conflicts_superseded, 1);
        assert_eq!(status(&s, &old.1), Status::Superseded);
        assert_eq!(status(&s, &new.1), Status::Active);
        let why = why(&s, &old.1[..8], false, false).unwrap();
        let crate::ops::Why::Found(text) = why else {
            panic!()
        };
        assert!(
            text.contains("superseded-by") && text.contains("opposed choices"),
            "{text}"
        );

        s.restore(&old.1).unwrap(); // what `confirm` does
        assert_eq!(status(&s, &old.1), Status::Active);
        assert!(s.relations_of(&old.1).unwrap().is_empty());
    }

    #[test]
    fn confirmed_or_more_confident_old_rule_is_flagged_not_replaced() {
        let mut s = Store::open_in_memory().unwrap();
        let mut rules = RuleIndex::load(&s).unwrap();
        let old = add(&mut s, Kind::Decision, "Use WAL mode for SQLite", 0.95);
        resolve(&mut s, &mut rules, &old);
        let new = add(
            &mut s,
            Kind::Decision,
            "Use rollback journal mode for SQLite",
            0.95,
        );
        let r = resolve(&mut s, &mut rules, &new);
        assert_eq!((r.conflicts_superseded, r.conflicts_flagged), (0, 1));
        assert_eq!(status(&s, &old.1), Status::Active, "confirmed rule stays");
        assert_eq!(status(&s, &new.1), Status::NeedsReverify);
    }

    #[test]
    fn unrelated_scopes_kinds_and_wording_never_conflict() {
        let mut s = Store::open_in_memory().unwrap();
        let mut rules = RuleIndex::load(&s).unwrap();
        let a = add(&mut s, Kind::Preference, "Use tabs for indentation", 0.8);
        resolve(&mut s, &mut rules, &a);
        let fact = add(&mut s, Kind::Fact, "Use spaces for indentation", 0.8);
        let other = {
            let mut m = add(
                &mut s,
                Kind::Preference,
                "Use spaces for indentation here",
                0.8,
            );
            m.0.scope = Scope::Project("/other".to_owned());
            m
        };
        assert_eq!(resolve(&mut s, &mut rules, &fact).conflicts_superseded, 0);
        assert_eq!(resolve(&mut s, &mut rules, &other).conflicts_superseded, 0);
        assert_eq!(status(&s, &a.1), Status::Active);
    }
}
