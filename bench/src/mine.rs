//! Builds a suite from a repo's git history, with no model in the loop.
//!
//! Facts are causal ("because", "root cause") and decision ("switched to",
//! "instead of") sentences in commit messages before `t0`. A fact is *live*
//! when most lines its commit added still exist at `t1`, *stale* when most are
//! gone. A probe is a later commit (the task) that touches files a fact is
//! about. Every rule here is mechanical and documented in `SCORING.md`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::git::{git, show};
use crate::suite::{Fact, Probe, RepoRef, Split, State, Suite};

const MAX_FILES_PER_COMMIT: usize = 25;
const MAX_PROBE_FILES: usize = 12;
const MAX_REQUIRED: usize = 3;
const MAX_STALE: usize = 3;
const MIN_WORDS: usize = 8;
const MAX_WORDS: usize = 60;
const MAX_CHARS: usize = 300;
const LIVE_AT_LEAST: f64 = 0.8;
const STALE_AT_MOST: f64 = 0.4;
const MIN_ADDED_LINES: usize = 3;

static CAUSAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(because|root cause|caused by|turns? out|the (problem|issue|bug) (is|was)|(happens|happened|occurs) when|due to|would (fail|panic|crash|hang|deadlock))\b",
    )
    .expect("causal regex")
});
static DECISION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(switch(ed|es)? to|replace[ds]? .{1,40} with|instead of|no longer|now uses?|drop(ped)? support|decided to|rather than)\b",
    )
    .expect("decision regex")
});
static SKIP_SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(merge|bump|release|version|update dep|chore\(deps\)|typo|fix typo|update changelog|docs?:)")
        .expect("skip regex")
});
static TRAILER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(signed-off-by|co-authored-by|fixes|closes|resolves|ref|see)\b|^https?://")
        .expect("trailer regex")
});

const NON_CODE: &[&str] = &[
    "md",
    "txt",
    "lock",
    "json",
    "toml",
    "yml",
    "yaml",
    "rst",
    "cfg",
    "ini",
    "svg",
    "png",
    "gitignore",
    "html",
    "css",
    "csv",
    "snap",
];

#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub author: String,
    pub subject: String,
    pub body: String,
    pub files: Vec<String>,
}

/// Where a suite starts and ends in the merge-free history up to `head`.
#[derive(Debug, Clone)]
pub struct Params {
    pub name: String,
    pub url: String,
    pub license: String,
    pub head: String,
    /// Position of `t0` in the history (0.0 oldest, 1.0 `head`).
    pub t0_frac: f64,
    /// Facts come from this many commits ending at `t0`: recent memory.
    pub window: usize,
    /// Commits between `t0` and `t1`, the time the code gets to change.
    pub gap: usize,
    /// Commits after `t1` that tasks are taken from.
    pub probe_window: usize,
    /// At most this many probes, spread evenly by hash.
    pub max_probes: usize,
}

/// All non-merge commits up to `head`, oldest first.
pub fn history(repo: &Path, head: &str) -> Result<Vec<Commit>, String> {
    let out = git(
        repo,
        &[
            "log",
            "--no-merges",
            "--reverse",
            "--name-only",
            "--format=%x1e%H%x1f%an%x1f%s%x1f%b%x1f",
            head,
        ],
    )?;
    Ok(out.split('\u{1e}').filter_map(parse_commit).collect())
}

fn parse_commit(chunk: &str) -> Option<Commit> {
    let mut f = chunk.splitn(5, '\u{1f}');
    let sha = f.next()?.trim().to_owned();
    if sha.len() < 40 {
        return None;
    }
    let author = f.next()?.to_owned();
    let subject = f.next()?.to_owned();
    let body = f.next()?.to_owned();
    let files = f
        .next()?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    Some(Commit {
        sha,
        author,
        subject,
        body,
        files,
    })
}

fn is_code(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| !NON_CODE.contains(&e.to_ascii_lowercase().as_str()))
}

fn code_files(c: &Commit) -> Vec<String> {
    c.files.iter().filter(|f| is_code(f)).cloned().collect()
}

fn skip(c: &Commit) -> bool {
    c.author.to_ascii_lowercase().contains("bot")
        || SKIP_SUBJECT.is_match(&c.subject)
        || c.files.len() > MAX_FILES_PER_COMMIT
}

/// Sentences of a commit message: hard-wrapped paragraphs are joined, code
/// blocks, trailers and links are dropped.
#[must_use]
pub fn sentences(subject: &str, body: &str) -> Vec<String> {
    let mut paragraphs: Vec<String> = vec![subject.trim().to_owned()];
    let mut cur = String::new();
    for line in body.lines() {
        let t = line.trim();
        let code = line.starts_with("    ") || line.starts_with('\t') || t.starts_with("```");
        if t.is_empty() || code || TRAILER.is_match(t) {
            if !cur.is_empty() {
                paragraphs.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(t);
    }
    if !cur.is_empty() {
        paragraphs.push(cur);
    }
    let mut out = Vec::new();
    for p in paragraphs {
        let mut start = 0;
        let bytes: Vec<(usize, char)> = p.char_indices().collect();
        for (i, &(pos, c)) in bytes.iter().enumerate() {
            let boundary = matches!(c, '.' | '!' | '?')
                && bytes.get(i + 1).is_none_or(|&(_, n)| n.is_whitespace());
            if boundary {
                out.push(p[start..pos + 1].trim().to_owned());
                start = pos + 1;
            }
        }
        if start < p.len() {
            out.push(p[start..].trim().to_owned());
        }
    }
    out.retain(|s| usable(s));
    out
}

fn usable(s: &str) -> bool {
    let words = s.split_whitespace().count();
    (MIN_WORDS..=MAX_WORDS).contains(&words)
        && s.chars().count() <= MAX_CHARS
        && !s.contains("http")
        && !s.ends_with(':')
}

/// The first causal and the first decision sentence of a commit, if any.
#[must_use]
pub fn fact_sentences(c: &Commit) -> Vec<(&'static str, String)> {
    let all = sentences(&c.subject, &c.body);
    let mut out = Vec::new();
    for (kind, re) in [("gotcha", &*CAUSAL), ("decision", &*DECISION)] {
        if let Some(s) = all.iter().find(|s| re.is_match(s)) {
            out.push((kind, s.clone()));
        }
    }
    out
}

/// Added lines (trimmed, at least 8 chars) of `commit` in `file`.
fn added_lines(repo: &Path, commit: &str, file: &str) -> Vec<String> {
    git(repo, &["show", "--format=", "-U0", commit, "--", file])
        .map(|d| {
            d.lines()
                .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                .map(|l| l[1..].trim().to_owned())
                .filter(|l| l.len() >= 8)
                .collect()
        })
        .unwrap_or_default()
}

/// Old path to new path for files renamed between two commits, so a file that
/// only moved is not mistaken for code that disappeared.
fn renames(repo: &Path, from: &str, to: &str) -> HashMap<String, String> {
    let out = git(repo, &["diff", "--name-status", "-M30%", from, to]).unwrap_or_default();
    out.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let status = p.next()?;
            let (old, new) = (p.next()?, p.next()?);
            status
                .starts_with('R')
                .then(|| (old.to_owned(), new.to_owned()))
        })
        .collect()
}

/// Share of the lines `commit` added to `files` that still exist at `at`
/// (following renames), and how many lines that is based on.
fn survival(repo: &Path, commit: &str, files: &[String], at: &str) -> (f64, usize) {
    let (mut added, mut kept) = (0_usize, 0_usize);
    let moved = renames(repo, commit, at);
    for f in files {
        let lines = added_lines(repo, commit, f);
        if lines.is_empty() {
            continue;
        }
        let now_path = moved.get(f).unwrap_or(f);
        let now: HashSet<String> = show(repo, at, now_path)
            .map(|c| c.lines().map(|l| l.trim().to_owned()).collect())
            .unwrap_or_default();
        added += lines.len();
        kept += lines.iter().filter(|l| now.contains(*l)).count();
    }
    if added == 0 {
        (0.0, 0)
    } else {
        #[allow(clippy::cast_precision_loss)]
        (kept as f64 / added as f64, added)
    }
}

/// Deterministic split from the commit hash: no state, no randomness.
#[must_use]
pub fn split_of(sha: &str) -> Split {
    let n = u32::from_str_radix(sha.get(..8).unwrap_or("0"), 16).unwrap_or(0);
    if n % 2 == 0 {
        Split::Tune
    } else {
        Split::Holdout
    }
}

fn state_of(ratio: f64, added: usize) -> State {
    if added < MIN_ADDED_LINES {
        State::Unknown
    } else if ratio >= LIVE_AT_LEAST {
        State::Live
    } else if ratio <= STALE_AT_MOST {
        State::Stale
    } else {
        State::Unknown
    }
}

fn at(commits: &[Commit], frac: f64) -> usize {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let i = (commits.len() as f64 * frac) as usize;
    i.min(commits.len().saturating_sub(1))
}

/// Mines a suite from `repo` (a checkout with full history up to `p.head`).
pub fn mine(repo: &Path, p: &Params) -> Result<Suite, String> {
    let commits = history(repo, &p.head)?;
    let i0 = at(&commits, p.t0_frac);
    let i1 = (i0 + p.gap).min(commits.len().saturating_sub(1));
    let end = (i1 + p.probe_window).min(commits.len().saturating_sub(1));
    if i0 < 20 || end <= i1 {
        return Err(format!("history too short ({} commits)", commits.len()));
    }
    let (t0, t1) = (&commits[i0], &commits[i1]);

    let mut facts = Vec::new();
    for c in commits[i0.saturating_sub(p.window)..=i0]
        .iter()
        .filter(|c| !skip(c))
    {
        let files: Vec<String> = code_files(c).into_iter().take(MAX_PROBE_FILES).collect();
        if files.is_empty() {
            continue;
        }
        for (n, (kind, text)) in fact_sentences(c).into_iter().enumerate() {
            let (ratio, added) = survival(repo, &c.sha, &files, &t1.sha);
            facts.push(Fact {
                id: format!("f{}-{n}", &c.sha[..8]),
                kind: kind.to_owned(),
                text,
                files: files.clone(),
                commit: c.sha.clone(),
                split: split_of(&c.sha),
                state: state_of(ratio, added),
                survival: (ratio * 100.0).round() / 100.0,
            });
        }
    }

    let mut probes: Vec<Probe> = limit_reuse(
        commits[i1 + 1..=end]
            .iter()
            .filter(|c| !skip(c))
            .filter_map(|c| probe_for(c, &facts))
            .collect(),
    );
    if probes.len() > p.max_probes {
        // Spread evenly over a hash order so the cap is neither the first
        // nor the last N commits, and stays reproducible.
        probes.sort_by(|a, b| a.commit.cmp(&b.commit));
        let step = probes.len() as f64 / p.max_probes as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let keep: Vec<Probe> = (0..p.max_probes)
            .map(|i| probes[(i as f64 * step) as usize].clone())
            .collect();
        probes = keep;
    }

    Ok(Suite {
        name: p.name.clone(),
        repo: RepoRef {
            url: p.url.clone(),
            license: p.license.clone(),
            t0: t0.sha.clone(),
            t1: t1.sha.clone(),
            head: commits[end].sha.clone(),
        },
        facts,
        probes,
    })
}

/// A fact may be the answer to at most this many probes, so a few facts about
/// a hot file cannot dominate the suite.
const MAX_FACT_USES: u32 = 2;

/// Keeps a probe only while it brings a fact that is not yet used up.
fn limit_reuse(probes: Vec<Probe>) -> Vec<Probe> {
    let mut uses: HashMap<String, u32> = HashMap::new();
    probes
        .into_iter()
        .filter(|p| {
            let ids: Vec<&String> = p.required.iter().chain(&p.stale).collect();
            let fresh = ids
                .iter()
                .any(|id| uses.get(*id).copied().unwrap_or(0) < MAX_FACT_USES);
            if fresh {
                for id in ids {
                    *uses.entry(id.clone()).or_default() += 1;
                }
            }
            fresh
        })
        .collect()
}

/// A fact is only ground truth for a task when it is about the same thing:
/// sharing a file is not enough, it must also share a content word with the
/// task subject or a file path. Keeps incidental co-edits out of the labels.
fn related(fact: &Fact, subject: &str, files: &[String]) -> bool {
    let words = |t: &str| -> std::collections::HashSet<String> {
        t.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.chars().count() >= 4)
            .map(str::to_lowercase)
            .collect()
    };
    let fw = words(&fact.text);
    let mut tw = words(subject);
    for f in files {
        tw.extend(words(f));
    }
    !fw.is_disjoint(&tw)
}

fn probe_for(c: &Commit, facts: &[Fact]) -> Option<Probe> {
    let files: Vec<String> = code_files(c).into_iter().take(MAX_PROBE_FILES).collect();
    if files.is_empty() {
        return None;
    }
    let pick = |state: State, cap: usize| -> Vec<String> {
        let mut hits: Vec<(usize, &Fact)> = facts
            .iter()
            .filter(|f| f.state == state)
            .map(|f| (f.files.iter().filter(|x| files.contains(x)).count(), f))
            .filter(|(n, f)| *n > 0 && related(f, &c.subject, &files))
            .collect();
        // Most shared files first; ties broken by id so the suite is stable.
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        hits.into_iter()
            .take(cap)
            .map(|(_, f)| f.id.clone())
            .collect()
    };
    let required = pick(State::Live, MAX_REQUIRED);
    let stale = pick(State::Stale, MAX_STALE);
    if required.is_empty() && stale.is_empty() {
        return None;
    }
    Some(Probe {
        id: format!("p{}", &c.sha[..8]),
        task: c.subject.clone(),
        files,
        commit: c.sha.clone(),
        required,
        stale,
        split: split_of(&c.sha),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn related_needs_a_shared_content_word() {
        let mut f = Fact {
            id: "x".into(),
            kind: "gotcha".into(),
            text: "The glob builder drops separators".into(),
            files: vec!["a.rs".into()],
            commit: "c".into(),
            split: Split::Tune,
            state: State::Live,
            survival: 1.0,
        };
        assert!(related(&f, "glob: fix repeated use of **", &[]));
        assert!(related(&f, "unrelated", &["src/builder.rs".into()]));
        f.text = "Windows console needs vt flag".into();
        assert!(!related(
            &f,
            "globset: fix repeated use of **",
            &["src/glob.rs".into()]
        ));
    }

    #[test]
    fn sentences_join_wrapped_lines_and_drop_noise() {
        let body = "The cache was keyed by path\nbecause the old key collided on rename.\n\n    let x = 1;\n\nFixes #12\nSigned-off-by: A <a@b.c>\n";
        let s = sentences("Fix cache key collision on rename of files", body);
        assert!(
            s.iter()
                .any(|x| x == "The cache was keyed by path because the old key collided on rename.")
        );
        assert!(
            s.iter()
                .all(|x| !x.contains("let x") && !x.contains("Signed"))
        );
    }

    #[test]
    fn causal_and_decision_are_picked_once_each() {
        let c = Commit {
            sha: "a".repeat(40),
            author: "dev".into(),
            subject: "Handle empty globs without panicking in matcher".into(),
            body: "This used to crash because the matcher assumed one pattern.\n\nWe switched to a set builder instead of one by one compilation of globs.\n".into(),
            files: vec!["src/glob.rs".into()],
        };
        let f = fact_sentences(&c);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].0, "gotcha");
        assert!(f[0].1.contains("because the matcher"));
        assert_eq!(f[1].0, "decision");
    }

    #[test]
    fn short_or_linky_sentences_are_not_facts() {
        for s in [
            "Because it broke.",
            "See https://example.com/issue/1 because that explains the whole story here.",
        ] {
            assert!(
                sentences("x", s)
                    .iter()
                    .all(|x| !CAUSAL.is_match(x) || !usable(x)),
                "{s}"
            );
        }
    }

    fn run(repo: &Path, args: &[&str]) {
        git(repo, args).unwrap();
    }

    #[test]
    fn survival_counts_added_lines_that_still_exist() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        run(r, &["init", "-q"]);
        run(r, &["config", "user.email", "t@example.com"]);
        run(r, &["config", "user.name", "t"]);
        let write = |c: &str| std::fs::write(r.join("a.rs"), c).unwrap();
        write("fn keep_one() {}\nfn keep_two() {}\nfn drop_three() {}\nfn drop_four() {}\n");
        run(r, &["add", "."]);
        run(r, &["commit", "-qm", "add"]);
        let c1 = git(r, &["rev-parse", "HEAD"]).unwrap().trim().to_owned();
        write("fn keep_one() {}\nfn keep_two() {}\nfn other_five() {}\n");
        run(r, &["commit", "-qam", "edit"]);
        let t1 = git(r, &["rev-parse", "HEAD"]).unwrap().trim().to_owned();
        let (ratio, added) = survival(r, &c1, &["a.rs".to_owned()], &t1);
        assert_eq!(added, 4);
        assert!((ratio - 0.5).abs() < 1e-9, "{ratio}");
        let (gone, _) = survival(r, &c1, &["missing.rs".to_owned()], &t1);
        assert!(gone.abs() < 1e-9);
    }

    #[test]
    fn code_files_exclude_docs_and_data() {
        assert!(is_code("src/main.rs"));
        assert!(is_code("pkg/server.go"));
        assert!(!is_code("README.md"));
        assert!(!is_code("Cargo.lock"));
        assert!(!is_code("docs/guide.rst"));
    }

    #[test]
    fn state_thresholds() {
        assert_eq!(state_of(0.9, 10), State::Live);
        assert_eq!(state_of(0.2, 10), State::Stale);
        assert_eq!(state_of(0.6, 10), State::Unknown);
        assert_eq!(state_of(1.0, 2), State::Unknown);
    }

    #[test]
    fn split_is_stable_and_balanced_by_hash() {
        assert_eq!(split_of("00000000aaaa"), Split::Tune);
        assert_eq!(split_of("00000001aaaa"), Split::Holdout);
        assert_eq!(split_of("zz"), Split::Tune);
    }
}
