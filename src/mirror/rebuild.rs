//! `recalla rebuild --from-mirror`: restore the memory layer from mirror
//! files. See ADR D-021.
//!
//! Safety order: parse and verify every file first (any problem, including
//! unredacted or hostile content, refuses the whole rebuild before anything
//! is touched), back the database up with `VACUUM INTO`, then replace the
//! memory tables in ONE `BEGIN IMMEDIATE` transaction on the live database.
//! The database file is never swapped, so `recalla mcp` or a hook holding it
//! open just sees an ordinary commit.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use super::{format, md_files};
use crate::model::{self, Scope};
use crate::pack::{is_invisible, one_line};
use crate::paths;
use crate::redact::{redact, redact_path};
use crate::store::{FullMemory, Store, StoreError};

/// Most refused files listed in an error message.
const LIST_MAX: usize = 20;
/// Backup names tried before giving up.
const BACKUP_TRIES: u32 = 1000;
/// Longest echoed parse message or path.
const MSG_MAX: usize = 120;
/// Projects and dropped memories listed in a plan.
const PLAN_MAX: usize = 10;
/// Largest believable `seen_count`.
const SEEN_MAX: u32 = 1_000_000;

/// One mirror file that blocks a rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub msg: String,
}

/// Why a rebuild failed or was refused.
#[derive(Debug, Error)]
pub enum RebuildError {
    /// At least one file is malformed, tampered with, or inconsistent.
    /// Nothing was changed.
    #[error("{}", refused(.0))]
    Refused(Vec<Problem>),
    /// No `.md` files at all: restoring would empty the memory layer.
    #[error("no mirror files under {}; nothing to rebuild from", .0.display())]
    Empty(PathBuf),
    /// The mirror has fewer memories than the database.
    #[error(
        "{0} memories exist in the database but not in the mirror; \
         re-run with --yes --allow-drop to remove them from the memory layer (a backup is kept)"
    )]
    WouldDrop(usize),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

fn refused(problems: &[Problem]) -> String {
    let mut s = format!(
        "refusing to rebuild: {} mirror file(s) are invalid; nothing was changed",
        problems.len()
    );
    for p in problems.iter().take(LIST_MAX) {
        let _ = write!(
            s,
            "\n  {}: {}",
            one_line(&p.path.display().to_string(), 200),
            p.msg
        );
    }
    if problems.len() > LIST_MAX {
        let _ = write!(s, "\n  ... and {} more", problems.len() - LIST_MAX);
    }
    s
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> RebuildError {
    let path = path.to_owned();
    move |source| RebuildError::Io { path, source }
}

/// A memory in the database that the mirror does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    pub uid8: String,
    pub kind: String,
    /// Cleaned and shortened.
    pub text: String,
}

/// What a rebuild did, or (when not applied) would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    /// Memories restored (or to restore).
    pub memories: usize,
    /// Of those, with global scope.
    pub global: usize,
    /// Memories per project key, most first, at most 10 projects.
    pub per_project: Vec<(String, usize)>,
    /// Memories in the DB but not in the mirror: the total and the first few.
    pub dropped_total: usize,
    pub dropped: Vec<Dropped>,
    /// Whether the memory layer was actually replaced.
    pub applied: bool,
    /// The `VACUUM INTO` copy of the previous database, when there was one
    /// and `applied`.
    pub backup: Option<PathBuf>,
}

fn add<'a>(f: &mut Vec<(&'a str, bool, bool)>, s: Option<&'a str>, path: bool) {
    f.extend(s.map(|s| (s, path, false)));
}

/// First problem with a parsed memory, as a fixed message that never echoes
/// file content. Everything that is later printed, injected into a pack or
/// restored must already be clean: a mirror written from the (redacted)
/// database is stable under `redact`, so legitimate files pass.
fn taint(doc: &FullMemory) -> Option<&'static str> {
    let m = &doc.memory;
    let (project, module) = m.scope.parts();
    let mut fields: Vec<(&str, bool, bool)> = vec![(m.text.as_str(), false, true)];
    add(&mut fields, project, true);
    add(&mut fields, module, true);
    add(&mut fields, m.superseded_by.as_deref(), false);
    for p in &m.provenance {
        add(&mut fields, p.harness.as_deref(), false);
        add(&mut fields, p.session_id.as_deref(), false);
        add(&mut fields, p.message_uuid.as_deref(), false);
        add(&mut fields, p.source_path.as_deref(), true);
        add(&mut fields, p.git_commit.as_deref(), false);
    }
    for (a, stale) in m.anchors.iter().zip(&doc.stale) {
        add(&mut fields, Some(&a.path), true);
        add(&mut fields, a.symbol.as_deref(), false);
        add(&mut fields, a.lang.as_deref(), false);
        add(&mut fields, a.content_hash.as_deref(), false);
        add(&mut fields, a.git_commit.as_deref(), false);
        add(&mut fields, stale.as_deref(), false);
    }
    for (s, path, body) in fields {
        if s.chars()
            .any(|c| is_invisible(c) || (c.is_control() && !(body && matches!(c, '\n' | '\t'))))
        {
            return Some("contains a control or invisible character");
        }
        let r = if path { redact_path(s) } else { redact(s) };
        if r.text != s {
            return Some("contains a secret-shaped value or text redaction would alter");
        }
    }
    range_problem(doc)
}

fn range_problem(doc: &FullMemory) -> Option<&'static str> {
    let m = &doc.memory;
    let times_ok = [m.created_at, m.updated_at, m.valid_from]
        .iter()
        .chain(&doc.provenance_at)
        .all(|t| *t >= 0);
    if !(m.confidence.is_finite() && (0.0..=1.0).contains(&m.confidence)) {
        Some("confidence out of range 0..1")
    } else if m.importance > 3 {
        Some("importance out of range 0..3")
    } else if !(1..=SEEN_MAX).contains(&m.seen_count) {
        Some("seen_count out of range")
    } else if !times_ok || m.valid_to.is_some_and(|t| t < 0) {
        Some("negative timestamp")
    } else if m.valid_to.is_some_and(|t| t < m.valid_from) {
        Some("valid_to is before valid_from")
    } else if m.provenance.iter().any(|p| p.line.is_some_and(|l| l < 0)) {
        Some("negative line number")
    } else {
        None
    }
}

/// Parses and verifies every file under `mirror`. All problems are
/// collected, none stops the scan.
fn load(mirror: &Path) -> Result<Vec<FullMemory>, RebuildError> {
    let files = md_files(mirror).map_err(io_err(mirror))?;
    if files.is_empty() {
        return Err(RebuildError::Empty(mirror.to_owned()));
    }
    let mut docs = Vec::with_capacity(files.len());
    let mut problems = Vec::new();
    let mut seen = HashSet::new();
    for path in files {
        let parsed = fs::read_to_string(&path)
            .map_err(|e| format!("unreadable: {:?}", e.kind()))
            .and_then(|t| format::parse(&t).map_err(|e| one_line(&e.to_string(), MSG_MAX)));
        match parsed {
            Err(msg) => problems.push(Problem { path, msg }),
            Ok(doc) => {
                let m = &doc.memory;
                let msg = if model::uid(m.kind, &m.scope, &m.text) != m.uid {
                    Some("uid does not match the memory's kind, scope and text (edited?)")
                } else if !seen.insert(m.uid.clone()) {
                    Some("duplicate uid (another file has it)")
                } else {
                    taint(&doc)
                };
                match msg {
                    Some(msg) => problems.push(Problem {
                        path,
                        msg: msg.to_owned(),
                    }),
                    None => docs.push((path, doc)),
                }
            }
        }
    }
    for (path, doc) in &docs {
        if let Some(to) = &doc.memory.superseded_by
            && !seen.contains(to)
        {
            problems.push(Problem {
                path: path.clone(),
                msg: format!("superseded_by {} has no file", one_line(to, MSG_MAX)),
            });
        }
    }
    if problems.is_empty() {
        Ok(docs.into_iter().map(|(_, d)| d).collect())
    } else {
        Err(RebuildError::Refused(problems))
    }
}

/// Counts for the plan: global memories and the biggest projects.
fn summarize(docs: &[FullMemory], report: &mut RebuildReport) {
    let mut by_project: BTreeMap<&str, usize> = BTreeMap::new();
    for d in docs {
        match &d.memory.scope {
            Scope::Global => report.global += 1,
            Scope::Project(p) | Scope::Module { project: p, .. } => {
                *by_project.entry(p).or_default() += 1;
            }
        }
    }
    let mut v: Vec<(String, usize)> = by_project
        .into_iter()
        .map(|(p, n)| (one_line(p, 200), n))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(PLAN_MAX);
    report.per_project = v;
}

/// Restores the memory layer of `<data_dir>/recalla.db` from the files under
/// `mirror`. With `apply == false` only parses, verifies and reports.
///
/// The live database is never replaced or renamed. A consistent copy goes to
/// `recalla.db.bak-<unix-secs>[-n]` (`VACUUM INTO`, never overwriting an
/// existing file), then the memory tables are replaced in one `BEGIN
/// IMMEDIATE` transaction, so it is safe while `recalla mcp` or hooks have the
/// database open. Events, sources, segments and the injection ledger are not
/// touched. If the mirror lacks memories the database has, `apply` needs
/// `allow_drop`.
///
/// # Errors
/// [`RebuildError::Refused`] / [`RebuildError::Empty`] /
/// [`RebuildError::WouldDrop`] before anything is touched; store or
/// file-system failures otherwise (including a busy database, in which case
/// the transaction did not happen and only the backup file was added).
pub fn rebuild(
    data_dir: &Path,
    mirror: &Path,
    apply: bool,
    allow_drop: bool,
) -> Result<RebuildReport, RebuildError> {
    let docs = load(mirror)?;
    let mut report = RebuildReport {
        memories: docs.len(),
        ..RebuildReport::default()
    };
    summarize(&docs, &mut report);
    let db = paths::db_path(data_dir);
    let existing = if db.is_file() {
        Some(Store::open(&db)?)
    } else {
        None
    };
    if let Some(store) = &existing {
        let keep: HashSet<String> = docs.iter().map(|d| d.memory.uid.clone()).collect();
        let gone = store.memories_not_in(&keep)?;
        report.dropped_total = gone.len();
        report.dropped = gone
            .into_iter()
            .take(PLAN_MAX)
            .map(|(uid, kind, text)| Dropped {
                uid8: uid.chars().take(8).collect(),
                kind: one_line(&kind, 20),
                text: one_line(&text, 60),
            })
            .collect();
    }
    if !apply {
        return Ok(report);
    }
    if report.dropped_total > 0 && !allow_drop {
        return Err(RebuildError::WouldDrop(report.dropped_total));
    }
    let mut store = if let Some(store) = existing {
        report.backup = Some(backup(&store, &db)?);
        store
    } else {
        paths::ensure_dir(data_dir).map_err(io_err(data_dir))?;
        paths::ensure_private_file(&db).map_err(io_err(&db))?;
        let store = Store::open(&db)?;
        paths::restrict_file(&db).map_err(io_err(&db))?;
        store
    };
    store.replace_memory_layer(&docs)?;
    report.applied = true;
    Ok(report)
}

/// Copies the database to a free `recalla.db.bak-<stamp>[-n]` with `VACUUM
/// INTO`. The target is pre-created (0600, `create_new`, so no existing backup
/// is ever overwritten) because SQLite accepts an empty file as a target.
fn backup(store: &Store, db: &Path) -> Result<PathBuf, RebuildError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for n in 0..BACKUP_TRIES {
        let suffix = if n == 0 {
            String::new()
        } else {
            format!("-{n}")
        };
        let bak = db.with_file_name(format!("{}.bak-{stamp}{suffix}", paths::DB_FILE));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(&bak) {
            Ok(_) => {
                return store.backup_into(&bak).map(|()| bak.clone()).map_err(|e| {
                    let _ = fs::remove_file(&bak);
                    e.into()
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err(&bak)(e)),
        }
    }
    Err(io_err(db)(io::Error::other("no free backup name")))
}
