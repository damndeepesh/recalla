//! Markdown mirror: one `.md` file per memory under `<data>/mirror`, kept in
//! sync with the database, and the rebuild that restores the memory layer
//! from those files. See ADR D-021 for the format and rebuild policy.
//!
//! Layout: `global/<kind>/<uid>.md` and `projects/<slug>/<kind>/<uid>.md`.
//! Every name is built from a kind, a hex uid and a sanitized slug; nothing
//! in a path comes from memory text. Dirs are 0700, files 0600.

pub mod format;
mod rebuild;

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::model::{Memory, Scope};
use crate::paths;
use crate::store::{Store, StoreError};

pub use rebuild::{Dropped, RebuildError, RebuildReport, rebuild};

/// Longest readable part of a project slug.
const SLUG_MAX: usize = 40;

/// Why a sync failed.
#[derive(Debug, Error)]
pub enum MirrorError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

impl MirrorError {
    fn io(path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_owned();
        move |source| Self::Io { path, source }
    }
}

/// What one [`sync`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Files created or rewritten.
    pub written: usize,
    /// Memories examined whose file was already current.
    pub unchanged: usize,
    /// `.md` files whose uid is not in the database. Only looked for on a
    /// full sync; never deleted.
    pub strays: Vec<PathBuf>,
}

/// Directory name for a project key: its sanitized basename plus six hex
/// chars of the key's hash, so distinct projects never share a directory.
#[must_use]
pub fn project_slug(key: &str) -> String {
    let base = key
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("");
    let mut slug = String::new();
    for c in base.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_alphanumeric() { c } else { '-' };
        if c != '-' || !slug.ends_with('-') {
            slug.push(c);
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "project" } else { slug };
    let slug: String = slug.chars().take(SLUG_MAX).collect();
    let hash = blake3::hash(key.as_bytes()).to_hex();
    format!("{}-{}", slug.trim_end_matches('-'), &hash[..6])
}

/// Where a memory's file lives under the mirror root.
#[must_use]
pub fn memory_path(dir: &Path, m: &Memory) -> PathBuf {
    let base = match &m.scope {
        Scope::Global => dir.join("global"),
        Scope::Project(p) | Scope::Module { project: p, .. } => {
            dir.join("projects").join(project_slug(p))
        }
    };
    base.join(m.kind.as_str()).join(format!("{}.md", m.uid))
}

/// Brings the mirror at `dir` up to date: renders the memories the database
/// has marked dirty (all of them when `full`) and writes only files whose
/// bytes differ. Strays are reported on a full sync. No clocks are involved:
/// see the `mirror_dirty` triggers in schema v7.
///
/// # Errors
/// Store or file-system failures. Files written before the failure stay, and
/// marks are only cleared after every write succeeded. A change made while the
/// sync runs keeps its (newer) mark and is picked up by the next sync.
pub fn sync(store: &mut Store, dir: &Path, full: bool) -> Result<SyncReport, MirrorError> {
    paths::ensure_dir(dir).map_err(MirrorError::io(dir))?;
    let batch = store.mirror_batch(full)?;
    let mut report = SyncReport::default();
    // Per-file fsync is only worth its cost for the small writes hooks make.
    let durable = batch.docs.len() <= DURABLE_MAX;
    for doc in &batch.docs {
        let path = memory_path(dir, &doc.memory);
        let bytes = format::render(doc);
        match fs::read(&path) {
            Ok(old) if old == bytes.as_bytes() => report.unchanged += 1,
            Ok(_) => {
                write_atomic(&path, bytes.as_bytes(), durable)?;
                report.written += 1;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                write_atomic(&path, bytes.as_bytes(), durable)?;
                report.written += 1;
            }
            Err(e) => return Err(MirrorError::io(&path)(e)),
        }
    }
    store.clear_mirror_marks(&batch.marks)?;
    if full {
        let known = store.memory_uids()?;
        report.strays = md_files(dir)
            .map_err(MirrorError::io(dir))?
            .into_iter()
            .filter(|p| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_none_or(|uid| !known.contains(uid))
            })
            .collect();
    }
    Ok(report)
}

/// Incremental [`sync`] into `<data_dir>/mirror` that never fails the caller:
/// returns a one-line warning (paths and OS errors, no memory text) instead.
pub fn sync_best_effort(store: &mut Store, data_dir: &Path) -> Option<String> {
    sync(store, &paths::mirror_dir(data_dir), false)
        .err()
        .map(|e| format!("mirror not updated: {e}"))
}

/// Writes of at most this many files are fsynced one by one.
const DURABLE_MAX: usize = 64;

/// tmp + (fsync) + rename, creating parents 0700 and the file 0600.
fn write_atomic(path: &Path, bytes: &[u8], durable: bool) -> Result<(), MirrorError> {
    let io_err = MirrorError::io(path);
    if let Some(parent) = path.parent() {
        paths::ensure_dir(parent).map_err(MirrorError::io(parent))?;
    }
    let tmp = path.with_extension("md.tmp");
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io_err(e)),
        _ => {}
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let result = opts
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            if durable {
                f.sync_all()?;
            }
            Ok(())
        })
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(e));
    }
    Ok(())
}

/// Every `.md` file under `dir`, sorted, not following symlinks.
///
/// # Errors
/// Any I/O failure except `dir` itself not existing (that gives an empty list).
pub fn md_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let entries = match fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound && d == dir => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, NewMemory, Origin, Provenance};

    #[test]
    fn slugs_are_sanitized_and_distinct() {
        let a = project_slug("/Users/me/Work/My App!");
        assert!(a.starts_with("my-app-"), "{a}");
        assert_eq!(a.len(), "my-app-".len() + 6);
        assert_ne!(project_slug("/a/app"), project_slug("/b/app"));
        assert_eq!(project_slug("/a/app"), project_slug("/a/app"));
        assert!(project_slug("/").starts_with("project-"));
        assert!(project_slug("git@github.com:a/b").starts_with("b-"));
        let long = project_slug(&format!("/{}", "x".repeat(200)));
        assert_eq!(long.len(), SLUG_MAX + 7);
        let weird = project_slug("/../é/..//\u{0}");
        assert!(
            weird
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
    }

    #[test]
    fn paths_use_kind_uid_and_slug_only() {
        let mut s = Store::open_in_memory().unwrap();
        let new = NewMemory {
            kind: Kind::Gotcha,
            text: "../../etc/passwd".into(),
            scope: Scope::Module {
                project: "/w/../x".into(),
                path: "src/../..".into(),
            },
            confidence: 0.5,
            importance: 1,
        };
        let uid = s
            .insert(&new, &Provenance::new(Origin::Remember))
            .unwrap()
            .uid()
            .to_owned();
        let m = s.get(&uid).unwrap().unwrap();
        let p = memory_path(Path::new("/m"), &m);
        let rel = p.strip_prefix("/m").unwrap();
        assert_eq!(rel.components().count(), 4, "{rel:?}");
        assert!(rel.to_str().unwrap().ends_with(&format!("gotcha/{uid}.md")));
        assert!(!rel.to_str().unwrap().contains(".."));
    }
}
