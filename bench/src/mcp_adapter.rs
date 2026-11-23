//! Drives any MCP memory server through a store tool and a recall tool.

use serde_json::json;

use crate::adapter::{Adapter, Ctx, Item, Recall, Support};
use crate::mcp::Client;
use crate::tokens;

/// How to talk to one MCP memory server.
#[derive(Debug, Clone)]
pub struct Spec {
    pub name: String,
    /// Program followed by its arguments.
    pub command: Vec<String>,
    pub store_tool: String,
    pub recall_tool: String,
    pub text_arg: String,
    pub query_arg: String,
}

pub struct Mcp {
    spec: Spec,
    client: Option<Client>,
}

impl Mcp {
    #[must_use]
    pub fn new(spec: Spec) -> Self {
        Self { spec, client: None }
    }

    fn client(&mut self) -> Result<&mut Client, String> {
        if self.client.is_none() {
            let (prog, args) = self.spec.command.split_first().ok_or("empty MCP command")?;
            self.client = Some(Client::spawn(prog, args)?);
        }
        self.client.as_mut().ok_or_else(|| "no client".into())
    }
}

impl Adapter for Mcp {
    fn name(&self) -> String {
        self.spec.name.clone()
    }

    fn feed_facts(&mut self, ctx: &Ctx<'_>) -> Result<Support, String> {
        let (tool, arg) = (self.spec.store_tool.clone(), self.spec.text_arg.clone());
        let c = self.client()?;
        for f in &ctx.suite.facts {
            c.call(&tool, &json!({ arg.as_str(): f.feed_text() }))?;
        }
        Ok(Support::Done)
    }

    /// The server has no budget parameter, so the harness cuts the reply to
    /// the budget the same way for every tool: in the order returned.
    fn recall(&mut self, query: &str, budget: usize) -> Result<Recall, String> {
        let (tool, arg) = (self.spec.recall_tool.clone(), self.spec.query_arg.clone());
        let reply = self
            .client()?
            .call(&tool, &json!({ arg.as_str(): query }))?;
        let mut used = 0;
        let mut items = Vec::new();
        for line in reply.lines() {
            let text = line.trim().trim_start_matches(['-', '*', ' ']).trim();
            if text.is_empty() {
                continue;
            }
            used += tokens::est(text) + 1;
            if used > budget {
                break;
            }
            items.push(Item {
                text: text.to_owned(),
                id: None,
                flagged_stale: false,
            });
        }
        Ok(Recall { items })
    }
}
