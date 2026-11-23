//! Minimal MCP stdio client: enough to initialise a server and call tools.
//! Used for any tool that exposes memory over MCP, Recalla's included.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    /// Starts `program args` and completes the MCP handshake.
    pub fn spawn(program: &str, args: &[String]) -> Result<Self, String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {program}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let out = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let mut c = Self {
            child,
            stdin,
            out,
            next: 0,
        };
        c.request(
            "initialize",
            &json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "membench", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        c.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(c)
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        writeln!(self.stdin, "{msg}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("write to server: {e}"))
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let mut line = String::new();
            if self.out.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("server closed the connection".into());
            }
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // notification or log line
            }
            return match v.get("error") {
                Some(e) => Err(format!("{method}: {e}")),
                None => Ok(v["result"].clone()),
            };
        }
    }

    /// Calls a tool and returns its text content joined; errors on `isError`.
    pub fn call(&mut self, tool: &str, args: &Value) -> Result<String, String> {
        let r = self.request("tools/call", &json!({"name": tool, "arguments": args}))?;
        let text = r["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if r["isError"].as_bool() == Some(true) {
            return Err(format!("{tool}: {text}"));
        }
        Ok(text)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
