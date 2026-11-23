//! The dashboard's "Import history" job: the same ingest + extract + mirror
//! path as `recalla backfill`, on one background thread. At most one job runs
//! at a time; the state is a small mutex-protected record (counts only, never
//! content).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use serde_json::{Value, json};

use super::api::{now_ms, tilde};
use super::security::sanitize;
use crate::extract;
use crate::ingest::{self, Options};
use crate::mirror;
use crate::ops;
use crate::paths;

#[derive(Debug, Default, Clone)]
enum Job {
    #[default]
    Idle,
    Running {
        id: String,
        started_at: i64,
    },
    Done {
        id: String,
        started_at: i64,
        finished_at: i64,
        report: Value,
    },
    Failed {
        id: String,
        started_at: i64,
        finished_at: i64,
        error: String,
    },
}

/// What a finished job produced.
pub type Outcome = Result<Value, String>;

/// Shared job state (one per server).
#[derive(Debug, Default)]
pub struct SyncState {
    job: Mutex<Job>,
    counter: AtomicU64,
}

impl SyncState {
    fn lock(&self) -> MutexGuard<'_, Job> {
        self.job.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Claims the single job slot: the new job id, or the id of the job that
    /// is already running.
    ///
    /// # Errors
    /// The running job's id.
    pub fn try_start(&self) -> Result<String, String> {
        let mut job = self.lock();
        if let Job::Running { id, .. } = &*job {
            return Err(id.clone());
        }
        let id = format!("j{}", self.counter.fetch_add(1, Ordering::SeqCst) + 1);
        *job = Job::Running {
            id: id.clone(),
            started_at: now_ms(),
        };
        Ok(id)
    }

    fn finish(&self, outcome: Outcome) {
        let mut job = self.lock();
        let Job::Running { id, started_at } = &*job else {
            return;
        };
        let (id, started_at, finished_at) = (id.clone(), *started_at, now_ms());
        *job = match outcome {
            Ok(report) => Job::Done {
                id,
                started_at,
                finished_at,
                report,
            },
            Err(error) => Job::Failed {
                id,
                started_at,
                finished_at,
                error,
            },
        };
    }

    /// `GET /api/sync/status` body.
    #[must_use]
    pub fn status_json(&self) -> Value {
        let (state, id, started, finished, report, error) = match &*self.lock() {
            Job::Idle => ("idle", None, None, None, None, None),
            Job::Running { id, started_at } => (
                "running",
                Some(id.clone()),
                Some(*started_at),
                None,
                None,
                None,
            ),
            Job::Done {
                id,
                started_at,
                finished_at,
                report,
            } => (
                "done",
                Some(id.clone()),
                Some(*started_at),
                Some(*finished_at),
                Some(report.clone()),
                None,
            ),
            Job::Failed {
                id,
                started_at,
                finished_at,
                error,
            } => (
                "failed",
                Some(id.clone()),
                Some(*started_at),
                Some(*finished_at),
                None,
                Some(error.clone()),
            ),
        };
        let mut v = json!({
            "state": state, "job": id, "started_at": started, "finished_at": finished,
        });
        if let (Some(obj), Some(report)) = (v.as_object_mut(), report) {
            obj.insert("report".to_owned(), report);
        }
        if let (Some(obj), Some(error)) = (v.as_object_mut(), error) {
            obj.insert("error".to_owned(), json!(error));
        }
        v
    }
}

/// What the job needs; copied so the thread owns it.
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub data_dir: PathBuf,
    pub claude_root: Option<PathBuf>,
    pub codex_root: Option<PathBuf>,
}

/// Runs the job on a new thread. The caller already holds the slot (`try_start`).
pub fn spawn(state: &Arc<SyncState>, job: JobSpec) {
    let worker = Arc::clone(state);
    let started = thread::Builder::new()
        .name("recalla-ui-sync".into())
        .spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| run(&job)))
                .unwrap_or_else(|_| Err("sync crashed".to_owned()));
            worker.finish(outcome);
        });
    if let Err(e) = started {
        eprintln!("recalla ui: cannot start sync thread: {:?}", e.kind());
        state.finish(Err("could not start the sync".to_owned()));
    }
}

/// Same steps and options as `recalla backfill` (archive on, extract on,
/// mirror sync). Internal error text goes to stderr, never to the client.
fn run(job: &JobSpec) -> Outcome {
    let fail = |what: &str, e: &dyn std::fmt::Display| {
        eprintln!("recalla ui: sync {what} failed: {e}");
        format!("sync failed during {what}; see the terminal running recalla ui")
    };
    if job.claude_root.is_none() && job.codex_root.is_none() {
        return Err("no session folders to import (HOME is unset)".to_owned());
    }
    let opts = Options {
        claude_root: job.claude_root.clone(),
        codex_root: job.codex_root.clone(),
        data_dir: job.data_dir.clone(),
        archive: true,
        dry_run: false,
    };
    let ingested = ingest::run(&opts).map_err(|e| fail("import", &e))?;
    let mut store = ops::open_existing(&job.data_dir).map_err(|e| fail("extract", &e))?;
    let extracted = extract::run(&mut store, &extract::Options { rebuild: false })
        .map_err(|e| fail("extract", &e))?;
    let mirror_warning = mirror::sync_best_effort(&mut store, &job.data_dir);
    if let Some(w) = &mirror_warning {
        eprintln!("recalla ui: {w}");
    }
    let mut ingest_json = ingested.to_json();
    redact_failed_paths(&mut ingest_json);
    Ok(json!({
        "ingest": ingest_json,
        "extract": extracted.to_json(),
        "mirror": mirror_warning.is_none().then(|| tilde(&paths::mirror_dir(&job.data_dir))),
    }))
}

/// Failed files are listed by basename only.
fn redact_failed_paths(ingest: &mut Value) {
    let Some(failed) = ingest
        .pointer_mut("/files/failed")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for f in failed {
        if let Some(path) = f.get("path").and_then(Value::as_str) {
            let name = Path::new(path)
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            f["path"] = json!(sanitize(&name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_job_at_a_time_and_states_report_correctly() {
        let s = SyncState::default();
        assert_eq!(s.status_json()["state"], "idle");
        let first = s.try_start().unwrap();
        assert_eq!(s.try_start(), Err(first.clone()));
        assert_eq!(s.status_json()["state"], "running");
        s.finish(Ok(json!({"n": 1})));
        let done = s.status_json();
        assert_eq!(
            (done["state"].as_str(), done["report"]["n"].as_i64()),
            (Some("done"), Some(1))
        );
        let second = s.try_start().unwrap();
        assert_ne!(first, second);
        s.finish(Err("boom".to_owned()));
        let failed = s.status_json();
        assert_eq!(
            (failed["state"].as_str(), failed["error"].as_str()),
            (Some("failed"), Some("boom"))
        );
        assert!(failed.get("report").is_none());
        s.finish(Ok(json!({})));
        assert_eq!(
            s.status_json()["state"],
            "failed",
            "finish without a running job is ignored"
        );
    }

    #[test]
    fn failed_file_paths_are_cut_to_basenames() {
        let mut v = json!({"files": {"failed": [{"path": "/home/u/.claude/p/a\u{1b}b.jsonl", "error": "x"}]}});
        redact_failed_paths(&mut v);
        assert_eq!(v["files"]["failed"][0]["path"], "ab.jsonl");
    }
}
