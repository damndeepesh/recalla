//! Turns a suite's facts into Claude Code session logs, one short session per
//! fact in commit order, so tools that ingest transcripts get the same
//! knowledge a developer would have left behind.

use std::fs;
use std::path::Path;

use serde_json::json;

use crate::suite::{Fact, Suite};

/// Working directory recorded in the sessions: the real checkout when there
/// is one (so anchors resolve), else a placeholder.
#[must_use]
pub fn cwd(suite: &Suite, worktree: &Path) -> String {
    if worktree.join(".git").exists() {
        worktree.display().to_string()
    } else {
        format!("/bench/{}", suite.name)
    }
}

/// Names that end up in file paths: suite files are untrusted input.
#[must_use]
pub fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s != ".."
        && s != "."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// Writes `<root>/projects/<enc>/<fact id>.jsonl` for every fact.
pub fn write(suite: &Suite, root: &Path, cwd: &str) -> Result<(), String> {
    if !safe_name(&suite.name) {
        return Err(format!("unsafe suite name {:?}", suite.name));
    }
    let dir = root.join("projects").join(cwd.replace('/', "-"));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (n, f) in suite.facts.iter().enumerate() {
        if !safe_name(&f.id) {
            return Err(format!("unsafe fact id {:?}", f.id));
        }
        let lines = session(f, cwd, n);
        fs::write(dir.join(format!("{}.jsonl", f.id)), lines).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn rec(kind: &str, sid: &str, n: usize, cwd: &str, msg: &serde_json::Value) -> String {
    json!({
        "type": kind, "uuid": format!("{sid}-{n}"), "sessionId": sid,
        "timestamp": format!("2025-01-01T00:{:02}:{:02}Z", n / 60 % 60, n % 60),
        "cwd": cwd, "gitBranch": "main", "message": msg,
    })
    .to_string()
}

fn session(f: &Fact, cwd: &str, n: usize) -> String {
    let sid = f.id.clone();
    let file = f.files.first().cloned().unwrap_or_default();
    let path = format!("{cwd}/{file}");
    let user = json!({"role": "user", "content": format!("Let's work on {file}.")});
    let said = json!({"role": "assistant", "content": [{"type": "text", "text": f.text}]});
    let edit = json!({"role": "assistant", "content": [{
        "type": "tool_use", "id": format!("t{n}"), "name": "Edit",
        "input": {"file_path": path, "old_string": "a", "new_string": "b"},
    }]});
    let done = json!({"role": "user", "content": [{
        "type": "tool_result", "tool_use_id": format!("t{n}"), "content": format!("error: edit of {file} failed"), "is_error": true,
    }]});
    [
        rec("user", &sid, 0, cwd, &user),
        rec("assistant", &sid, 1, cwd, &edit),
        rec("user", &sid, 2, cwd, &done),
        rec("assistant", &sid, 3, cwd, &said),
    ]
    .join("\n")
        + "\n"
}

#[cfg(test)]
mod tests {
    use super::safe_name;

    #[test]
    fn rejects_path_like_names() {
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "/abs", "a b"] {
            assert!(!safe_name(bad), "{bad}");
        }
        assert!(safe_name("ripgrep-e1") && safe_name("f_1.2"));
    }
}
