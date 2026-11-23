//! Recalla, driven only through its public interfaces: MCP `remember` to
//! write, `recalla pack` / `why` / `check` to read.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::adapter::{Adapter, Ctx, Item, Recall, Support};
use crate::mcp::Client;
use crate::suite::Fact;

pub struct Recalla {
    /// Feed through transcript backfill instead of `remember`.
    backfill: bool,
    bin: PathBuf,
    data: PathBuf,
    project: String,
}

impl Recalla {
    pub fn new(bin: &Path, scratch: &Path, backfill: bool) -> Result<Self, String> {
        let data = scratch.join("recalla-data");
        std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
        Ok(Self {
            backfill,
            bin: bin.to_path_buf(),
            data,
            project: "bench".into(),
        })
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new(&self.bin)
            .args(args)
            .arg("--data-dir")
            .arg(&self.data)
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.bin.display()))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }
}

impl Adapter for Recalla {
    fn name(&self) -> String {
        if self.backfill {
            "recalla-backfill"
        } else {
            "recalla"
        }
        .into()
    }

    fn feed_facts(&mut self, ctx: &Ctx<'_>) -> Result<Support, String> {
        if !self.backfill && ctx.worktree.join(".git").exists() {
            // A real directory, so `remember` can anchor to the code it names.
            self.project = ctx.worktree.display().to_string();
        }
        if self.backfill {
            let root = ctx.scratch.join("claude-root");
            let cwd = crate::sessions::cwd(ctx.suite, ctx.worktree);
            crate::sessions::write(ctx.suite, &root, &cwd)?;
            self.project = cwd;
            self.run(&[
                "backfill",
                "--root",
                &root.display().to_string(),
                "--harness",
                "claude",
                "--no-archive",
            ])?;
            return Ok(Support::Done);
        }
        let args = vec![
            "mcp".to_owned(),
            "--data-dir".into(),
            self.data.display().to_string(),
            "--project".into(),
            self.project.clone(),
        ];
        let mut c = Client::spawn(&self.bin.display().to_string(), &args)?;
        for f in &ctx.suite.facts {
            let kind = if f.kind == "decision" {
                "decision"
            } else {
                "gotcha"
            };
            c.call(
                "remember",
                &json!({"text": Fact::feed_text(f), "kind": kind}),
            )?;
        }
        Ok(Support::Done)
    }

    fn recall(&mut self, query: &str, budget: usize) -> Result<Recall, String> {
        let b = budget.to_string();
        let q = format!("--query={query}");
        let out = self.run(&[
            "pack",
            "--budget",
            &b,
            &q,
            "--project",
            &self.project,
            "--format",
            "json",
        ])?;
        let v: Value = serde_json::from_str(&out).map_err(|e| format!("pack json: {e}"))?;
        let items = v["memories"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|m| Item {
                        text: m["text"].as_str().unwrap_or_default().to_owned(),
                        id: m["uid"].as_str().map(str::to_owned),
                        flagged_stale: m["status"].as_str() == Some("needs_reverify"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Recall { items })
    }

    fn advance(&mut self, ctx: &Ctx<'_>) -> Result<(), String> {
        if ctx.worktree.join(".git").exists() {
            self.run(&["check", "--project", &self.project])?;
        }
        Ok(())
    }

    fn has_provenance(&self) -> bool {
        true
    }

    fn source(&mut self, item: &Item) -> Option<String> {
        let id = item.id.as_deref()?;
        self.run(&["why", id, "--json"]).ok()
    }

    /// Reads the markdown mirror, Recalla's documented plain-file export.
    fn exported(&mut self) -> Result<Option<Vec<String>>, String> {
        let mut out = Vec::new();
        let mut stack = vec![self.data.join("mirror")];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "md") {
                    let t = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
                    // Body follows the closing frontmatter fence.
                    let body = t.splitn(3, "---").nth(2).unwrap_or(&t).trim();
                    out.push(body.to_owned());
                }
            }
        }
        Ok(Some(out))
    }
}
