//! Staleness check: re-reads anchored files and flags memories whose
//! symbol (or file) changed or vanished. Never clears; only `confirm` does.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use super::lang::{Def, MAX_SOURCE_BYTES, definitions, language_for};
use super::{contained_path, normalized_hash};
use crate::model::{Anchor, Status};
use crate::store::{Store, StoreError};

/// What one [`check`] run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// Anchors examined.
    pub checked: usize,
    /// Anchors newly marked stale.
    pub stale_new: usize,
    /// Stale anchors whose code matches again. They stay stale until `confirm`.
    pub recovered: usize,
    /// Anchors left unexamined because the time budget ran out.
    pub skipped_budget: usize,
}

/// A parsed source file: text plus its definitions.
struct Parsed {
    src: String,
    defs: Vec<Def>,
}

impl CheckReport {
    /// Machine-readable form (`recalla check --json`).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "checked": self.checked,
            "stale_new": self.stale_new,
            "recovered": self.recovered,
            "skipped_budget": self.skipped_budget,
        })
    }
}

enum Verdict {
    Fresh,
    Stale(String),
    /// Can't tell (oversized, unreadable, unknown language): never stale.
    Unknown,
}

/// Per-run cache of parsed files keyed by path, valid while mtime and size match.
#[derive(Default)]
struct Cache(HashMap<PathBuf, (SystemTime, u64, Rc<Parsed>)>);

impl Cache {
    fn parsed(&mut self, file: &Path) -> Option<Rc<Parsed>> {
        let meta = fs::metadata(file).ok()?;
        let (mtime, size) = (meta.modified().ok()?, meta.len());
        if usize::try_from(size).ok()? > MAX_SOURCE_BYTES {
            return None;
        }
        if let Some((t, n, p)) = self.0.get(file)
            && (*t, *n) == (mtime, size)
        {
            return Some(Rc::clone(p));
        }
        let src = String::from_utf8_lossy(&fs::read(file).ok()?).into_owned();
        let defs = language_for(file).map_or_else(Vec::new, |l| definitions(l, &src));
        let parsed = Rc::new(Parsed { src, defs });
        self.0
            .insert(file.to_owned(), (mtime, size, Rc::clone(&parsed)));
        Some(parsed)
    }
}

fn verdict(root: &Path, a: &Anchor, cache: &mut Cache) -> Verdict {
    let Some(file) = contained_path(root, &a.path) else {
        return Verdict::Stale("file removed".to_owned());
    };
    let (Some(symbol), Some(want)) = (&a.symbol, &a.content_hash) else {
        return Verdict::Fresh;
    };
    if language_for(&file).is_none() {
        return Verdict::Unknown;
    }
    let Some(parsed) = cache.parsed(&file) else {
        return Verdict::Unknown;
    };
    let Some(def) = parsed.defs.iter().find(|d| &d.qualified == symbol) else {
        return Verdict::Stale(format!("symbol removed: {symbol}"));
    };
    if normalized_hash(&parsed.src[def.range.clone()]) == *want {
        Verdict::Fresh
    } else {
        Verdict::Stale(format!("symbol changed: {symbol}"))
    }
}

/// Checks every anchor of the live memories scoped to `project_key`, whose
/// files live under that directory (a project key is its root path). Stale anchors get a reason and their
/// memory becomes `needs_reverify` (only if it is `active`). With a
/// `budget`, stops once it is spent and counts the rest as skipped.
///
/// # Errors
/// Any store failure.
pub fn check(
    store: &mut Store,
    project_key: &str,
    budget: Option<Duration>,
) -> Result<CheckReport, StoreError> {
    let started = Instant::now();
    let mut report = CheckReport::default();
    let rows = store.anchors_for_project(project_key)?;
    let Ok(root) = Path::new(project_key).canonicalize() else {
        return Ok(report);
    };
    let mut cache = Cache::default();
    let mut flagged: HashSet<String> = HashSet::new();
    for (i, (uid, id, anchor, reason)) in rows.iter().enumerate() {
        if budget.is_some_and(|b| started.elapsed() >= b) {
            report.skipped_budget = rows.len() - i;
            break;
        }
        report.checked += 1;
        match (verdict(&root, anchor, &mut cache), reason) {
            (Verdict::Stale(why), None) => {
                store.mark_stale(*id, &why)?;
                report.stale_new += 1;
                if flagged.insert(uid.clone())
                    && store.get(uid)?.is_some_and(|m| m.status == Status::Active)
                {
                    store.set_status(uid, Status::NeedsReverify)?;
                }
            }
            (Verdict::Fresh, Some(_)) => report.recovered += 1,
            _ => {}
        }
    }
    Ok(report)
}
