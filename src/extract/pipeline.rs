//! Incremental extraction: stored events in id order through the rules into
//! memories, checkpointed after every batch.
//!
//! A crash loses at most one batch of progress; replaying it is harmless
//! because inserts are idempotent through the memory uid.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Instant;

use serde_json::{Value, json};

use super::rules::task;
use super::scope::{project_key, project_scope};
use super::{EXTRACTOR_VERSION, EventCtx, extract_event_with};
use crate::model::{self, Kind, NewMemory, Origin, Provenance, Status};
use crate::store::{EventRow, Insert, Store, StoreError};

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
            process(store, &ctx, restart, &project_of, &mut report)?;
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
        for anchor in &c.anchors {
            store.add_anchor(outcome.uid(), anchor)?;
        }
        // A rebuild archived this memory moments ago; extracting it again
        // means it is still wanted.
        if restoring
            && matches!(outcome, Insert::Duplicate(_))
            && store
                .get(outcome.uid())?
                .is_some_and(|m| m.status == Status::Archived)
        {
            store.restore(outcome.uid())?;
        }
    }
    archive_finished_tasks(store, ctx, project_of, report)
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
