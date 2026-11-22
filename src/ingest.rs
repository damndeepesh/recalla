//! Resumable, idempotent import of session logs into the store.
//!
//! Per file: identity check against the stored checkpoint, parse from the
//! resume offset, redact THEN truncate every content field, archive the raw
//! bytes just consumed, and commit events + checkpoint + segment in one
//! transaction. A crash loses at most one file's new tail, re-read next run.
//!
//! Privacy: the [`Report`] holds counts, paths and error kinds only.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};
use thiserror::Error;

use crate::archive;
use crate::harness::claude::ClaudeCode;
use crate::harness::{Event, EventKind, Harness, MAX_EVENT_BYTES, count_newlines, truncate_middle};
use crate::paths;
use crate::redact::redact;
use crate::store::{Checkpoint, SegmentRow, Store, StoreError, StoredEvent, StoredKind};

/// Bytes hashed from the start of a file to detect rewrites and rotation.
pub const IDENTITY_PREFIX: u64 = 4096;

/// What to ingest and where to put it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Harness config root (e.g. `~/.claude`).
    pub root: PathBuf,
    pub data_dir: PathBuf,
    /// Keep a raw zstd copy of ingested bytes.
    pub archive: bool,
    /// Compute everything, write nothing (no data dir is created).
    pub dry_run: bool,
}

/// A file that could not be ingested. Never carries content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub path: String,
    /// Error kind, e.g. `NotFound` or `sqlite`.
    pub error: String,
}

/// Counts from one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub files_seen: u64,
    pub files_new: u64,
    pub files_resumed: u64,
    pub files_restarted: u64,
    pub files_unchanged: u64,
    pub failed: Vec<Failure>,
    pub events_added: u64,
    /// Complete lines that were not valid JSON records (skipped).
    pub malformed_lines: u64,
    pub redactions_by_rule: BTreeMap<String, u64>,
    pub archive_raw_bytes: u64,
    pub archive_compressed_bytes: u64,
    pub duration_ms: u64,
}

impl Report {
    /// Machine-readable form (`recalla backfill --json`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let failed: Vec<Value> = self
            .failed
            .iter()
            .map(|f| json!({ "path": f.path, "error": f.error }))
            .collect();
        json!({
            "files": {
                "seen": self.files_seen,
                "new": self.files_new,
                "resumed": self.files_resumed,
                "restarted": self.files_restarted,
                "unchanged": self.files_unchanged,
                "failed": failed,
            },
            "events_added": self.events_added,
            "malformed_lines": self.malformed_lines,
            "redactions_by_rule": self.redactions_by_rule,
            "archive_raw_bytes": self.archive_raw_bytes,
            "archive_compressed_bytes": self.archive_compressed_bytes,
            "duration_ms": self.duration_ms,
        })
    }

    /// Total redactions across rules.
    #[must_use]
    pub fn redactions(&self) -> u64 {
        self.redactions_by_rule.values().sum()
    }
}

/// Errors that stop a run before any file is processed.
#[derive(Debug, Error)]
pub enum IngestError {
    #[error("cannot list sessions under {}: {source}", path.display())]
    Discover { path: PathBuf, source: io::Error },
    #[error("cannot prepare data dir {}: {source}", path.display())]
    DataDir { path: PathBuf, source: io::Error },
    #[error("cannot open store: {0}")]
    Store(#[from] StoreError),
}

/// A per-file error; recorded in the report, the run continues.
#[derive(Debug, Error)]
enum FileError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl FileError {
    fn kind(&self) -> String {
        match self {
            Self::Io(e) => format!("{:?}", e.kind()),
            Self::Store(StoreError::Sqlite(_)) => "sqlite".to_owned(),
            Self::Store(StoreError::NotFound(_)) => "store-not-found".to_owned(),
            Self::Store(StoreError::Invalid(_)) => "store-invalid".to_owned(),
        }
    }
}

/// What the checkpoint says to do with a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    New,
    Resume(u64),
    Restart,
    Unchanged,
}

/// Imports every session under `opts.root`.
///
/// # Errors
/// Setup failures only (listing sessions, creating the data dir, opening the
/// store). Per-file failures are reported in [`Report::failed`].
pub fn run(opts: &Options) -> Result<Report, IngestError> {
    let started = Instant::now();
    let harness = ClaudeCode;
    let discover_err = |source| IngestError::Discover {
        path: opts.root.clone(),
        source,
    };
    // Sources are keyed by path, so every spelling of the root (`./`, `..`,
    // symlinks) must resolve to one canonical path. A missing root stays as
    // given and discovers nothing.
    let root = match fs::canonicalize(&opts.root) {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => opts.root.clone(),
        Err(e) => return Err(discover_err(e)),
    };
    let files = harness.discover(&root).map_err(discover_err)?;
    let mut store = open_store(opts)?;
    let mut report = Report::default();
    let archive_dir = paths::archive_dir(&opts.data_dir);
    for file in &files {
        report.files_seen += 1;
        if let Err(e) = ingest_file(
            &harness,
            &mut store,
            opts,
            &archive_dir,
            &file.path,
            &mut report,
        ) {
            report.failed.push(Failure {
                path: file.path.to_string_lossy().into_owned(),
                error: e.kind(),
            });
        }
    }
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(report)
}

/// The real store (data dir 0700, DB 0600), or an in-memory one for a dry
/// run, which therefore treats every file as new.
fn open_store(opts: &Options) -> Result<Store, IngestError> {
    if opts.dry_run {
        return Ok(Store::open_in_memory()?);
    }
    let dir_err = |source| IngestError::DataDir {
        path: opts.data_dir.clone(),
        source,
    };
    paths::ensure_dir(&opts.data_dir).map_err(dir_err)?;
    let db = paths::db_path(&opts.data_dir);
    paths::ensure_private_file(&db).map_err(dir_err)?;
    let store = Store::open(&db)?;
    paths::restrict_file(&db).map_err(dir_err)?;
    Ok(store)
}

/// Hex blake3 of the first `min(IDENTITY_PREFIX, upto)` bytes of `file`.
/// Bounded by the checkpoint offset (not the file length), so appending to a
/// short file keeps its identity and resumes instead of restarting.
fn identity(file: &mut File, upto: u64) -> io::Result<String> {
    let mut buf = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.by_ref()
        .take(upto.min(IDENTITY_PREFIX))
        .read_to_end(&mut buf)?;
    Ok(blake3::hash(&buf).to_hex().to_string())
}

/// Whether the bytes before `next_offset` still look like what was ingested:
/// the last one is a line end and the last complete line (capped at
/// `IDENTITY_PREFIX` bytes) hashes to the stored value. The prefix identity
/// alone cannot see a file rewritten past its first 4 KB.
fn tail_hash(file: &mut File, next_offset: u64) -> io::Result<Option<String>> {
    if next_offset == 0 {
        return Ok(Some(blake3::hash(b"").to_hex().to_string()));
    }
    let start = next_offset.saturating_sub(IDENTITY_PREFIX);
    Ok(last_line_hash(&read_range(file, start, next_offset)?))
}

/// Hash of the last complete line in `window`, or `None` when it doesn't end
/// on a line boundary. Callers pass the LAST `IDENTITY_PREFIX` bytes before the
/// checkpoint, so the file and buffer paths hash the same window.
fn last_line_hash(window: &[u8]) -> Option<String> {
    let (&b'\n', body) = window.split_last()? else {
        return None;
    };
    let line = body
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(body, |i| &body[i + 1..]);
    let line = &line[line
        .len()
        .saturating_sub(usize::try_from(IDENTITY_PREFIX).unwrap_or(usize::MAX))..];
    Some(blake3::hash(line).to_hex().to_string())
}

fn read_range(file: &mut File, start: u64, end: u64) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    file.seek(SeekFrom::Start(start))?;
    file.by_ref().take(end - start).read_to_end(&mut buf)?;
    if buf.len() as u64 != end - start {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "file shrank while ingesting",
        ));
    }
    Ok(buf)
}

fn ingest_file(
    harness: &impl Harness,
    store: &mut Store,
    opts: &Options,
    archive_dir: &Path,
    path: &Path,
    report: &mut Report,
) -> Result<(), FileError> {
    let key = path.to_string_lossy();
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let existing = store.source(&key)?;
    let plan = match &existing {
        // No source, or one whose first batch never committed (empty
        // identity): nothing was ingested yet, so it is new, not a rewrite.
        None => Plan::New,
        Some(src) if src.identity.is_empty() => Plan::New,
        Some(src)
            if len >= src.next_offset
                && identity(&mut file, src.next_offset)? == src.identity
                && tail_hash(&mut file, src.next_offset)?.as_deref() == Some(&src.tail_hash) =>
        {
            if len == src.next_offset {
                Plan::Unchanged
            } else {
                Plan::Resume(src.next_offset)
            }
        }
        Some(_) => Plan::Restart,
    };
    let offset = match plan {
        Plan::Unchanged => {
            report.files_unchanged += 1;
            return Ok(());
        }
        Plan::Resume(at) => at,
        Plan::New | Plan::Restart => 0,
    };
    let source_id = match &existing {
        Some(src) => src.id,
        None => store.upsert_source(harness.name(), &key)?,
    };

    // Read the new tail once; parse and archive both work from this buffer, so
    // the archived bytes are exactly the parsed bytes even if the file keeps
    // growing. Memory is bounded by the file's new tail (the whole file on
    // first ingest); files are processed one at a time.
    let bytes = read_range(&mut file, offset, len)?;
    file.seek(SeekFrom::Start(0))?;
    let base_line = count_newlines(&mut file, offset)?;
    let (events, stats, consumed) = harness.parse_bytes(&bytes, offset, base_line, path);
    let next_offset = offset + consumed;
    report.malformed_lines += stats.malformed_lines;
    let stored: Vec<StoredEvent> = with_seq(&events)
        .map(|(seq, e)| to_stored(e, seq, &mut report.redactions_by_rule))
        .collect();

    let segment = if opts.archive && consumed > 0 {
        let raw = &bytes[..usize::try_from(consumed).expect("consumed <= buffer length")];
        report.archive_raw_bytes += raw.len() as u64;
        if opts.dry_run {
            report.archive_compressed_bytes += archive::compress(raw)?.len() as u64;
            None
        } else {
            let w = archive::write_segment(archive_dir, raw)?;
            report.archive_compressed_bytes += w.compressed_bytes;
            Some(SegmentRow {
                start: offset,
                end: next_offset,
                hash: w.hash,
            })
        }
    } else {
        None
    };
    let checkpoint = Checkpoint {
        identity: identity(&mut file, next_offset)?,
        // From the parsed buffer when this run consumed bytes: the hash then
        // describes exactly what was ingested, even if the file changes now.
        tail_hash: if consumed > 0 {
            let end = usize::try_from(consumed).expect("consumed <= buffer length");
            let cap = usize::try_from(IDENTITY_PREFIX).unwrap_or(usize::MAX);
            last_line_hash(&bytes[end.saturating_sub(cap)..end])
        } else {
            tail_hash(&mut file, next_offset)?
        }
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "checkpoint not at a line end")
        })?,
        size: len.max(next_offset),
        next_offset,
    };
    report.events_added += store.ingest_batch(
        source_id,
        &stored,
        &checkpoint,
        segment.as_ref(),
        plan == Plan::Restart,
    )?;
    match plan {
        Plan::New => report.files_new += 1,
        Plan::Resume(_) => report.files_resumed += 1,
        Plan::Restart => report.files_restarted += 1,
        Plan::Unchanged => {}
    }
    Ok(())
}

/// Pairs each event with its index among events from the same line (one
/// assistant record can hold text and several tool calls).
fn with_seq(events: &[Event]) -> impl Iterator<Item = (u32, &Event)> {
    let mut prev_line = None;
    let mut seq = 0u32;
    events.iter().map(move |e| {
        seq = if prev_line == Some(e.line) {
            seq + 1
        } else {
            0
        };
        prev_line = Some(e.line);
        (seq, e)
    })
}

/// Redacts `text`, adding its findings to `counts`.
fn scrub(text: &str, counts: &mut BTreeMap<String, u64>) -> String {
    let r = redact(text);
    for f in &r.findings {
        *counts.entry(f.rule.to_owned()).or_default() += 1;
    }
    r.text.into_owned()
}

/// Redact every content field, THEN truncate the text, so a cut can never
/// split a secret. Ids (session, uuids, tool ids) are stored as-is.
fn to_stored(e: &Event, seq: u32, counts: &mut BTreeMap<String, u64>) -> StoredEvent {
    let mut opt = |v: Option<&str>| v.map(|s| scrub(s, counts));
    let cwd = opt(e.cwd.as_deref());
    let git_branch = opt(e.git_branch.as_deref());
    let (kind, tool_name, tool_id, is_error) = match &e.kind {
        EventKind::UserText => (StoredKind::UserText, None, None, false),
        EventKind::AssistantText => (StoredKind::AssistantText, None, None, false),
        EventKind::Summary => (StoredKind::Summary, None, None, false),
        EventKind::ToolCall { id, name } => (
            StoredKind::ToolCall,
            Some(scrub(name, counts)),
            Some(id.clone()),
            false,
        ),
        EventKind::ToolResult {
            tool_use_id,
            is_error,
        } => (
            StoredKind::ToolResult,
            None,
            Some(tool_use_id.clone()),
            *is_error,
        ),
    };
    let (text, _) = truncate_middle(&scrub(&e.text, counts), MAX_EVENT_BYTES);
    StoredEvent {
        harness: e.harness.to_owned(),
        session_id: e.session_id.clone(),
        message_uuid: e.message_uuid.clone(),
        parent_uuid: e.parent_uuid.clone(),
        line: e.line,
        seq,
        byte_offset: e.byte_offset,
        ts: e.timestamp_ms,
        cwd,
        git_branch,
        sidechain: e.sidechain,
        kind,
        tool_name,
        tool_id,
        is_error,
        text,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn identity_is_bounded_by_offset_and_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f");
        std::fs::write(&p, b"abc\n").unwrap();
        let mut f = File::open(&p).unwrap();
        let short = identity(&mut f, 4).unwrap();
        // Appending keeps the identity of the already-ingested prefix.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"def\n")
            .unwrap();
        let mut f = File::open(&p).unwrap();
        assert_eq!(identity(&mut f, 4).unwrap(), short);
        assert_ne!(identity(&mut f, 8).unwrap(), short);

        let big = vec![b'x'; 10_000];
        std::fs::write(&p, &big).unwrap();
        let mut f = File::open(&p).unwrap();
        assert_eq!(
            identity(&mut f, 10_000).unwrap(),
            blake3::hash(&big[..4096]).to_hex().to_string()
        );
    }

    #[test]
    fn seq_counts_events_within_a_line() {
        let ev = |line| Event {
            harness: "h",
            session_id: "s".into(),
            message_uuid: None,
            parent_uuid: None,
            source_path: PathBuf::new(),
            line,
            byte_offset: 0,
            timestamp_ms: None,
            cwd: None,
            git_branch: None,
            sidechain: false,
            kind: EventKind::UserText,
            text: "t".into(),
        };
        let events = [ev(1), ev(2), ev(2), ev(2), ev(3)];
        let seqs: Vec<_> = with_seq(&events).map(|(s, e)| (e.line, s)).collect();
        assert_eq!(seqs, [(1, 0), (2, 0), (2, 1), (2, 2), (3, 0)]);
    }

    #[test]
    fn report_json_has_counts_only() {
        let mut r = Report {
            files_seen: 2,
            ..Report::default()
        };
        r.redactions_by_rule.insert("github-token".into(), 3);
        r.failed.push(Failure {
            path: "/x.jsonl".into(),
            error: "NotFound".into(),
        });
        let j = r.to_json();
        assert_eq!(j["files"]["seen"], 2);
        assert_eq!(j["files"]["failed"][0]["error"], "NotFound");
        assert_eq!(j["redactions_by_rule"]["github-token"], 3);
        assert_eq!(r.redactions(), 3);
    }
}
