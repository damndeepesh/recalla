//! Backfill over a Claude root and a Codex root together.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use recalla::cli;
use recalla::ingest::{Options, Report, run};
use recalla::paths;
use recalla::store::Store;

struct Env {
    _tmp: tempfile::TempDir,
    claude: PathBuf,
    codex: PathBuf,
    data: PathBuf,
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let (claude, codex) = (base.join("claude"), base.join("codex"));
        copy_tree(Path::new("tests/fixtures/claude"), &claude);
        copy_tree(Path::new("tests/fixtures/codex"), &codex);
        Self {
            data: base.join("data"),
            claude,
            codex,
            _tmp: tmp,
        }
    }

    fn run(&self) -> Report {
        run(&Options {
            claude_root: Some(self.claude.clone()),
            codex_root: Some(self.codex.clone()),
            opencode_root: None,
            data_dir: self.data.clone(),
            archive: true,
            dry_run: false,
        })
        .unwrap()
    }

    fn codex_file(&self) -> PathBuf {
        recalla::harness::codex::discover(&self.codex).unwrap()[0]
            .path
            .clone()
    }

    fn cli(&self, extra: &[&str]) -> (u8, String) {
        let mut args = vec!["recalla", "backfill", "--no-extract", "--json"];
        args.extend(["--root", self.claude.to_str().unwrap()]);
        args.extend(["--codex-root", self.codex.to_str().unwrap()]);
        args.extend(["--opencode-root", self.codex.to_str().unwrap()]); // no opencode.db there
        args.extend(["--data-dir", self.data.to_str().unwrap()]);
        args.extend(extra);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = cli::run(args, &mut out, &mut err);
        (code, String::from_utf8(out).unwrap())
    }
}

#[test]
fn imports_both_harnesses_and_rerun_is_a_no_op() {
    let env = Env::new();
    let r = env.run();
    assert_eq!(r.files_by_harness.get("claude-code"), Some(&3));
    assert_eq!(r.files_by_harness.get("codex"), Some(&1));
    assert_eq!((r.files_seen, r.files_new, r.failed.len()), (4, 4, 0));

    let store = Store::open(paths::db_path(&env.data)).unwrap();
    let mut harnesses: Vec<_> = store
        .sources()
        .unwrap()
        .into_iter()
        .map(|s| s.harness)
        .collect();
    harnesses.sort();
    assert_eq!(
        harnesses,
        ["claude-code", "claude-code", "claude-code", "codex"]
    );
    let before = store.event_count().unwrap();
    assert_eq!(before, r.events_added);

    let again = env.run();
    assert_eq!((again.events_added, again.files_unchanged), (0, 4));
}

#[test]
fn appended_codex_line_resumes_that_file_only() {
    let env = Env::new();
    env.run();
    let line = r#"{"timestamp":"2026-01-02T10:01:00.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"One more thing."}]}}"#;
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(env.codex_file())
        .unwrap();
    writeln!(f, "{line}").unwrap();

    let r = env.run();
    assert_eq!(
        (r.files_resumed, r.files_unchanged, r.events_added),
        (1, 3, 1)
    );
    assert_eq!(r.files_restarted, 0);
}

#[test]
fn harness_flag_selects_which_agent_is_read() {
    let files = |extra: &[&str]| {
        let env = Env::new();
        let (code, out) = env.cli(extra);
        assert_eq!(code, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v["files"]["by_harness"].clone()
    };
    assert_eq!(
        files(&["--harness", "codex"]),
        serde_json::json!({"codex": 1})
    );
    assert_eq!(
        files(&["--harness", "claude"]),
        serde_json::json!({"claude-code": 3})
    );
    assert_eq!(
        files(&[]),
        serde_json::json!({"claude-code": 3, "codex": 1})
    );
}

#[test]
fn human_report_breaks_files_down_by_harness() {
    let env = Env::new();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let args = [
        "recalla",
        "backfill",
        "--no-extract",
        "--root",
        env.claude.to_str().unwrap(),
        "--codex-root",
        env.codex.to_str().unwrap(),
        "--opencode-root",
        env.codex.to_str().unwrap(),
        "--data-dir",
        env.data.to_str().unwrap(),
    ];
    assert_eq!(cli::run(args, &mut out, &mut err), 0);
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("harnesses  claude-code 3, codex 1"), "{out}");
}
