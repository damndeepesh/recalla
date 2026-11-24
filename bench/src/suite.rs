//! Suite data: facts mined from a public repo's history, and the probes
//! (tasks) that should surface them. Suites are plain JSON so anyone can
//! inspect, diff and regenerate them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suite {
    pub name: String,
    pub repo: RepoRef,
    pub facts: Vec<Fact>,
    pub probes: Vec<Probe>,
}

/// The pinned public repository a suite was mined from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRef {
    pub url: String,
    pub license: String,
    /// History up to and including this commit is what a tool is fed.
    pub t0: String,
    /// The working tree a tool sees at task time; staleness is judged here.
    pub t1: String,
    /// Last commit probes are taken from.
    pub head: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    /// `gotcha` or `decision`.
    pub kind: String,
    pub text: String,
    pub files: Vec<String>,
    pub commit: String,
    pub split: Split,
    pub state: State,
    /// Share of the commit's added lines still present at `t1`.
    pub survival: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub id: String,
    /// The task, as the commit subject that later did the work.
    pub task: String,
    pub files: Vec<String>,
    pub commit: String,
    /// Live facts that share a file with the task: a good memory surfaces them.
    pub required: Vec<String>,
    /// Stale facts that share a file: a good memory must not serve them as current.
    pub stale: Vec<String>,
    pub split: Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    /// Used while developing the miner and the tools.
    Tune,
    /// Never looked at while tuning; the headline numbers.
    Holdout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Most of the commit's added lines still exist at `t1`.
    Live,
    /// Most of them are gone: the memory describes code that changed.
    Stale,
    /// In between; never used as ground truth either way.
    Unknown,
}

impl Fact {
    /// What every tool is told: the sentence plus the files it is about, the
    /// way an agent would naturally mention them.
    #[must_use]
    pub fn feed_text(&self) -> String {
        format!("{} (files: {})", self.text, self.files.join(", "))
    }
}

impl Probe {
    /// The query an agent would form, with or without file hints.
    #[must_use]
    pub fn query(&self, with_files: bool) -> String {
        if with_files {
            format!("{} (files: {})", self.task, self.files.join(", "))
        } else {
            self.task.clone()
        }
    }
}
