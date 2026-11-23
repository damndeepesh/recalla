//! What every memory tool is driven through. Adapters talk to the tool as a
//! black box (a process, a CLI, an MCP server); none gets a private path.

use std::path::{Path, PathBuf};

use crate::matching::words;
use crate::suite::{Fact, Suite};
use crate::tokens;

/// Everything an adapter may need about the run.
pub struct Ctx<'a> {
    pub suite: &'a Suite,
    /// Detached worktree; the harness moves it from `t0` to `t1`.
    pub worktree: &'a Path,
    /// Private scratch directory for this adapter and suite.
    pub scratch: &'a Path,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub text: String,
    /// Tool-side identifier, used to ask for provenance.
    pub id: Option<String>,
    /// The tool says this may no longer be true.
    pub flagged_stale: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Recall {
    pub items: Vec<Item>,
}

impl Recall {
    /// Tokens of everything the agent would receive.
    #[must_use]
    pub fn tokens(&self) -> usize {
        self.items.iter().map(|i| tokens::est(&i.text) + 1).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Done,
    /// The tool has no such interface. Reported as n/a, never as zero.
    Unsupported,
}

pub trait Adapter {
    fn name(&self) -> String;

    /// Writes the suite's facts through the tool's write interface.
    fn feed_facts(&mut self, ctx: &Ctx<'_>) -> Result<Support, String>;

    /// The working tree has moved from `t0` to `t1`; tools that track code may re-check.
    fn advance(&mut self, _ctx: &Ctx<'_>) -> Result<(), String> {
        Ok(())
    }

    fn recall(&mut self, query: &str, budget: usize) -> Result<Recall, String>;

    /// Whether the tool has any way to say where an item came from. Tools
    /// without one are n/a for provenance, not zero.
    fn has_provenance(&self) -> bool {
        false
    }

    /// Where the tool says an item came from, if it can say.
    fn source(&mut self, _item: &Item) -> Option<String> {
        None
    }

    /// Every memory the tool holds, for extraction scoring. `None` if it cannot list them.
    fn exported(&mut self) -> Result<Option<Vec<String>>, String> {
        Ok(None)
    }
}

/// Creates a scratch dir path inside `base` (not created here).
#[must_use]
pub fn scratch_for(base: &Path, suite: &str, adapter: &str) -> PathBuf {
    base.join(format!("{suite}--{adapter}"))
}

fn item(text: String) -> Item {
    Item {
        text,
        id: None,
        flagged_stale: false,
    }
}

/// No memory at all.
pub struct NoMemory;

impl Adapter for NoMemory {
    fn name(&self) -> String {
        "none".into()
    }
    fn feed_facts(&mut self, _: &Ctx<'_>) -> Result<Support, String> {
        Ok(Support::Done)
    }
    fn recall(&mut self, _: &str, _: usize) -> Result<Recall, String> {
        Ok(Recall::default())
    }
}

/// Every fact, every time, no budget: the upper bound on recall and the
/// reference for token savings.
#[derive(Default)]
pub struct Dump {
    facts: Vec<String>,
}

impl Adapter for Dump {
    fn name(&self) -> String {
        "dump".into()
    }
    fn feed_facts(&mut self, ctx: &Ctx<'_>) -> Result<Support, String> {
        self.facts = ctx.suite.facts.iter().map(Fact::feed_text).collect();
        Ok(Support::Done)
    }
    fn recall(&mut self, _: &str, _: usize) -> Result<Recall, String> {
        Ok(Recall {
            items: self.facts.iter().cloned().map(item).collect(),
        })
    }
}

/// A `CLAUDE.md`-style file: facts appended in time order, newest kept when
/// the budget cuts it. No search.
#[derive(Default)]
pub struct File {
    facts: Vec<String>,
}

impl Adapter for File {
    fn name(&self) -> String {
        "file".into()
    }
    fn feed_facts(&mut self, ctx: &Ctx<'_>) -> Result<Support, String> {
        self.facts = ctx.suite.facts.iter().map(Fact::feed_text).collect();
        Ok(Support::Done)
    }
    fn recall(&mut self, _: &str, budget: usize) -> Result<Recall, String> {
        Ok(fill(self.facts.iter().rev().cloned(), budget))
    }
}

/// What an agent with `grep` would do: rank stored facts by words shared with
/// the query and fill the budget. The honest "is a memory tool needed at all"
/// baseline.
#[derive(Default)]
pub struct Grep {
    facts: Vec<String>,
}

impl Adapter for Grep {
    fn name(&self) -> String {
        "grep".into()
    }
    fn feed_facts(&mut self, ctx: &Ctx<'_>) -> Result<Support, String> {
        self.facts = ctx.suite.facts.iter().map(Fact::feed_text).collect();
        Ok(Support::Done)
    }
    fn recall(&mut self, query: &str, budget: usize) -> Result<Recall, String> {
        let q = words(query);
        let mut scored: Vec<(usize, usize, &String)> = self
            .facts
            .iter()
            .enumerate()
            .map(|(i, f)| (words(f).intersection(&q).count(), i, f))
            .filter(|(n, _, _)| *n > 0)
            .collect();
        // Most shared words first; later facts win ties so output is stable.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        Ok(fill(scored.into_iter().map(|(_, _, f)| f.clone()), budget))
    }
}

/// Takes items in order while they fit `budget` tokens.
fn fill(items: impl Iterator<Item = String>, budget: usize) -> Recall {
    let mut used = 0;
    let mut out = Vec::new();
    for text in items {
        let cost = tokens::est(&text) + 1;
        if used + cost > budget {
            break;
        }
        used += cost;
        out.push(item(text));
    }
    Recall { items: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::{Probe, RepoRef, Split, State};

    pub fn suite(texts: &[&str]) -> Suite {
        Suite {
            name: "t".into(),
            repo: RepoRef {
                url: String::new(),
                license: String::new(),
                t0: String::new(),
                t1: String::new(),
                head: String::new(),
            },
            facts: texts
                .iter()
                .enumerate()
                .map(|(i, t)| Fact {
                    id: format!("f{i}"),
                    kind: "gotcha".into(),
                    text: (*t).into(),
                    files: vec!["a.rs".into()],
                    commit: "c".into(),
                    split: Split::Tune,
                    state: State::Live,
                    survival: 1.0,
                })
                .collect(),
            probes: Vec::<Probe>::new(),
        }
    }

    fn fed<A: Adapter>(mut a: A, texts: &[&str]) -> A {
        let s = suite(texts);
        let ctx = Ctx {
            suite: &s,
            worktree: Path::new("."),
            scratch: Path::new("."),
        };
        a.feed_facts(&ctx).unwrap();
        a
    }

    #[test]
    fn none_returns_nothing_and_dump_returns_everything() {
        let mut n = fed(NoMemory, &["alpha beta gamma"]);
        assert!(n.recall("alpha", 100).unwrap().items.is_empty());
        let mut d = fed(Dump::default(), &["alpha beta gamma", "delta epsilon zeta"]);
        assert_eq!(d.recall("x", 1).unwrap().items.len(), 2);
    }

    #[test]
    fn file_keeps_newest_within_budget() {
        let long = "word ".repeat(40);
        let mut f = fed(
            File::default(),
            &["oldest fact here", &long, "newest fact here"],
        );
        let r = f.recall("x", 12).unwrap();
        assert!(r.items[0].text.starts_with("newest"));
        assert!(r.tokens() <= 12);
    }

    #[test]
    fn grep_ranks_by_shared_words_and_ignores_misses() {
        let mut g = fed(
            Grep::default(),
            &[
                "parser handles unicode escapes",
                "walker follows symlinks",
                "unrelated thing",
            ],
        );
        let r = g.recall("unicode parser escapes", 100).unwrap();
        assert_eq!(r.items.len(), 1);
        assert!(r.items[0].text.contains("unicode"));
        assert!(g.recall("zzz", 100).unwrap().items.is_empty());
    }
}
