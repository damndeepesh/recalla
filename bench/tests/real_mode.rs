//! `membench real` mines a suite from a local repository and leaves it untouched.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 80 commits over 4 files, each explaining why, so facts and tasks exist.
fn build_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    for i in 0..80 {
        let file = format!("src/module{}.rs", i % 4);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        let path = dir.join(&file);
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        for k in 0..10 {
            let _ = writeln!(text, "fn step{i}_{k}() {{ let value{i}_{k} = {i}; }}");
        }
        std::fs::write(&path, text).expect("write");
        git(dir, &["add", "."]);
        let msg = format!(
            "Handle widget{i} overflow in module{}\n\nThe widget{i} buffer overflowed because the length was never checked before the write in module{}.\n",
            i % 4,
            i % 4
        );
        git(dir, &["commit", "-q", "-m", &msg]);
    }
}

#[test]
fn real_mode_runs_on_a_local_repo_without_touching_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    build_repo(dir.path());
    let before = git(dir.path(), &["worktree", "list"]);
    let out = Command::new(env!("CARGO_BIN_EXE_membench"))
        .args(["real", "--repo"])
        .arg(dir.path())
        .args(["--adapters", "none,grep"])
        .output()
        .expect("run membench");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let table = String::from_utf8_lossy(&out.stdout);
    assert!(table.contains("| grep | 1500 |"), "{table}");
    assert!(table.contains("| none | 1500 |"), "{table}");
    assert_eq!(
        git(dir.path(), &["worktree", "list"]),
        before,
        "repo must be untouched"
    );
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), "");
}

#[test]
fn real_mode_explains_a_history_with_nothing_to_mine() {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    for i in 0..40 {
        std::fs::write(dir.path().join("a.txt"), format!("{i}")).expect("write");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", &format!("wip {i}")]);
    }
    let out = Command::new(env!("CARGO_BIN_EXE_membench"))
        .args(["real", "--repo"])
        .arg(dir.path())
        .output()
        .expect("run membench");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("too few") || err.contains("too short"),
        "{err}"
    );
}
