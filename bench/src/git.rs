//! Thin wrapper over the `git` CLI, run against a pinned checkout.

use std::path::Path;
use std::process::Command;

/// Runs `git -C repo args` and returns stdout, or the stderr on failure.
pub fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// File content at a commit, `None` when the file does not exist there.
pub fn show(repo: &Path, commit: &str, path: &str) -> Option<String> {
    git(repo, &["show", &format!("{commit}:{path}")]).ok()
}
