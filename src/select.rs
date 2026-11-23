//! Choosing which session files a backfill imports: by project directory
//! (the session's working directory) or by session id / file. Pure matching;
//! the only I/O is a bounded peek at the head of a session file for its `cwd`.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Bytes read from the head of a session file when looking for its `cwd`.
const PEEK_BYTES: u64 = 1 << 20;
/// Records inspected for a `cwd` before giving up.
const PEEK_LINES: usize = 64;
/// Shortest session-id fragment accepted (a stem suffix or prefix).
const MIN_ID_LEN: usize = 8;

/// What to import. Empty means everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Import sessions whose working directory is this directory or inside it.
    pub dirs: Vec<PathBuf>,
    /// Import this session: an id (full, or at least 8 characters of its start
    /// or end) or the path of a session `.jsonl` file.
    pub sessions: Vec<String>,
}

impl Selection {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && self.sessions.is_empty()
    }
}

/// One session file seen during discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub agent: &'static str,
    pub path: PathBuf,
    pub cwd: Option<PathBuf>,
}

/// The files that match, and the selectors that matched nothing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Matched {
    pub keep: Vec<usize>,
    /// Human-readable selectors (`--dir X`, `--session Y`) with no match.
    pub unmatched: Vec<String>,
}

/// Ids a session file answers to: its stem, plus the session directory name
/// for a Claude subagent file (`<session>/subagents/<agent>.jsonl`).
#[must_use]
pub fn session_ids(path: &Path) -> Vec<String> {
    let mut ids: Vec<String> = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .into_iter()
        .collect();
    let parent = path.parent();
    if parent
        .and_then(Path::file_name)
        .is_some_and(|n| n == "subagents")
        && let Some(session) = parent.and_then(Path::parent).and_then(Path::file_name)
    {
        ids.push(session.to_string_lossy().into_owned());
    }
    ids
}

fn id_matches(id: &str, selector: &str) -> bool {
    id == selector
        || (selector.len() >= MIN_ID_LEN && (id.ends_with(selector) || id.starts_with(selector)))
}

/// The working directory recorded near the top of a session file: a top-level
/// `cwd` (Claude Code) or `payload.cwd` (Codex). `None` when absent.
#[must_use]
pub fn peek_cwd(path: &Path) -> Option<PathBuf> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(PEEK_BYTES));
    let mut line = String::new();
    for _ in 0..PEEK_LINES {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let Ok(v) = serde_json::from_str::<Value>(line.trim_end()) else {
            continue;
        };
        let cwd = v
            .get("cwd")
            .or_else(|| v.get("payload").and_then(|p| p.get("cwd")))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(c) = cwd {
            return Some(PathBuf::from(c));
        }
    }
    None
}

/// `p` made absolute and symlink-resolved when it exists; otherwise lexically
/// absolute as given (a project directory may since have been deleted).
fn normal(p: &Path) -> PathBuf {
    fs::canonicalize(p)
        .or_else(|_| std::path::absolute(p))
        .unwrap_or_else(|_| p.to_path_buf())
}

/// Indexes of `candidates` selected by `sel`, in order. With an empty
/// selection every index is kept.
#[must_use]
pub fn select(candidates: &[Candidate], sel: &Selection) -> Matched {
    if sel.is_empty() {
        return Matched {
            keep: (0..candidates.len()).collect(),
            unmatched: Vec::new(),
        };
    }
    let dirs: Vec<PathBuf> = sel.dirs.iter().map(|d| normal(d)).collect();
    let files: Vec<Option<PathBuf>> = sel
        .sessions
        .iter()
        .map(|s| {
            let p = Path::new(s);
            (p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
                || p.is_file())
            .then(|| normal(p))
        })
        .collect();
    let cwds: Vec<Option<PathBuf>> = candidates
        .iter()
        .map(|c| c.cwd.as_deref().map(normal))
        .collect();
    let mut dir_hit = vec![false; dirs.len()];
    let mut ses_hit = vec![false; sel.sessions.len()];
    let mut keep = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        let ids = session_ids(&c.path);
        let mut take = false;
        for (k, d) in dirs.iter().enumerate() {
            if cwds[i].as_ref().is_some_and(|w| w.starts_with(d)) {
                dir_hit[k] = true;
                take = true;
            }
        }
        for (k, s) in sel.sessions.iter().enumerate() {
            let hit = match &files[k] {
                Some(f) => normal(&c.path) == *f,
                None => ids.iter().any(|id| id_matches(id, s)),
            };
            if hit {
                ses_hit[k] = true;
                take = true;
            }
        }
        if take {
            keep.push(i);
        }
    }
    let unmatched = sel
        .dirs
        .iter()
        .zip(&dir_hit)
        .filter(|(_, hit)| !**hit)
        .map(|(d, _)| format!("--dir {}", d.display()))
        .chain(
            sel.sessions
                .iter()
                .zip(&ses_hit)
                .filter(|(_, hit)| !**hit)
                .map(|(s, _)| format!("--session {s}")),
        )
        .collect();
    Matched { keep, unmatched }
}

/// Parses a menu answer such as `1,3-5`, `2 4` or `a` (all) into sorted,
/// de-duplicated zero-based indexes in `0..max`.
///
/// # Errors
/// A message naming the part that is not a number in range.
pub fn parse_choice(input: &str, max: usize) -> Result<Vec<usize>, String> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("a") || input.eq_ignore_ascii_case("all") {
        return Ok((0..max).collect());
    }
    let num = |t: &str| {
        t.trim()
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=max).contains(n))
            .ok_or_else(|| format!("'{}' is not a number from 1 to {max}", t.trim()))
    };
    let mut out = Vec::new();
    for part in input.split([',', ' ']).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b) = (num(a)?, num(b)?);
            if a > b {
                return Err(format!("'{part}' is a backwards range"));
            }
            out.extend(a - 1..b);
        } else {
            out.push(num(part)? - 1);
        }
    }
    if out.is_empty() {
        return Err("nothing chosen".to_owned());
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Whether `path` is a Claude Code subagent transcript (it follows its parent
/// session, so menus list only the parent).
#[must_use]
pub fn is_subagent(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n == "subagents")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(path: &str, cwd: Option<&str>) -> Candidate {
        Candidate {
            agent: "claude",
            path: PathBuf::from(path),
            cwd: cwd.map(PathBuf::from),
        }
    }

    fn sel(dirs: &[&str], sessions: &[&str]) -> Selection {
        Selection {
            dirs: dirs.iter().map(PathBuf::from).collect(),
            sessions: sessions.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn empty_selection_keeps_everything() {
        let c = [cand("/r/a.jsonl", None), cand("/r/b.jsonl", None)];
        let m = select(&c, &Selection::default());
        assert_eq!(m.keep, vec![0, 1]);
        assert!(m.unmatched.is_empty());
    }

    #[test]
    fn dir_matches_itself_and_children_but_not_siblings() {
        let c = [
            cand("/r/1.jsonl", Some("/work/app")),
            cand("/r/2.jsonl", Some("/work/app/crates/x")),
            cand("/r/3.jsonl", Some("/work/app2")),
            cand("/r/4.jsonl", None),
        ];
        let m = select(&c, &sel(&["/work/app"], &[]));
        assert_eq!(m.keep, vec![0, 1]);
        assert!(m.unmatched.is_empty());
    }

    #[test]
    fn session_matches_full_id_and_long_fragments_only() {
        let c = [
            cand("/r/0a1b2c3d-1111-2222-3333-444455556666.jsonl", None),
            cand("/r/ffffffff-1111-2222-3333-444455556666.jsonl", None),
        ];
        let full = "0a1b2c3d-1111-2222-3333-444455556666";
        assert_eq!(select(&c, &sel(&[], &[full])).keep, vec![0]);
        assert_eq!(select(&c, &sel(&[], &["0a1b2c3d"])).keep, vec![0]);
        assert_eq!(select(&c, &sel(&[], &["44455556666"])).keep, vec![0, 1]);
        let short = select(&c, &sel(&[], &["0a1b"]));
        assert!(short.keep.is_empty());
        assert_eq!(short.unmatched, vec!["--session 0a1b".to_owned()]);
    }

    #[test]
    fn codex_rollout_matches_by_trailing_uuid() {
        let c = [cand(
            "/c/sessions/2026/11/23/rollout-2026-11-23T10-00-00-0a1b2c3d-1111-2222-3333-444455556666.jsonl",
            None,
        )];
        assert_eq!(
            select(&c, &sel(&[], &["0a1b2c3d-1111-2222-3333-444455556666"])).keep,
            vec![0]
        );
    }

    #[test]
    fn subagent_files_follow_their_session() {
        let c = [
            cand("/r/projects/p/SESS-1234567/subagents/agent-1.jsonl", None),
            cand("/r/projects/p/SESS-1234567.jsonl", None),
            cand("/r/projects/p/OTHER-7654321.jsonl", None),
        ];
        assert_eq!(select(&c, &sel(&[], &["SESS-1234567"])).keep, vec![0, 1]);
    }

    #[test]
    fn unmatched_selectors_are_reported_and_others_still_apply() {
        let c = [cand("/r/1.jsonl", Some("/work/app"))];
        let m = select(&c, &sel(&["/work/app", "/nope"], &["deadbeefdead"]));
        assert_eq!(m.keep, vec![0]);
        assert_eq!(
            m.unmatched,
            vec![
                "--dir /nope".to_owned(),
                "--session deadbeefdead".to_owned()
            ]
        );
    }

    #[test]
    fn a_session_path_selects_exactly_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        fs::write(&a, "{}\n").unwrap();
        fs::write(&b, "{}\n").unwrap();
        let c = [
            Candidate {
                agent: "claude",
                path: a.clone(),
                cwd: None,
            },
            Candidate {
                agent: "claude",
                path: b,
                cwd: None,
            },
        ];
        let m = select(&c, &sel(&[], &[a.to_str().unwrap()]));
        assert_eq!(m.keep, vec![0]);
    }

    #[test]
    fn peek_reads_claude_and_codex_cwd_and_survives_junk() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("c.jsonl");
        fs::write(
            &claude,
            "not json\n{\"type\":\"summary\"}\n{\"cwd\":\"/w/a\",\"type\":\"user\"}\n",
        )
        .unwrap();
        assert_eq!(peek_cwd(&claude), Some(PathBuf::from("/w/a")));
        let codex = dir.path().join("x.jsonl");
        fs::write(
            &codex,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"1\",\"cwd\":\"/w/b\"}}\n",
        )
        .unwrap();
        assert_eq!(peek_cwd(&codex), Some(PathBuf::from("/w/b")));
        let none = dir.path().join("n.jsonl");
        fs::write(&none, "{\"type\":\"user\"}\n").unwrap();
        assert_eq!(peek_cwd(&none), None);
        assert_eq!(peek_cwd(&dir.path().join("missing.jsonl")), None);
    }

    #[test]
    fn parse_choice_handles_lists_ranges_all_and_errors() {
        assert_eq!(parse_choice("1,3-4 2", 5).unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(parse_choice(" A ", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_choice("2,2", 3).unwrap(), vec![1]);
        for bad in ["", "0", "4", "x", "3-1", "1-9", "-"] {
            assert!(parse_choice(bad, 3).is_err(), "{bad}");
        }
    }
}
