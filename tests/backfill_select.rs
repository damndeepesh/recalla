//! `recalla backfill --dir / --session / --list`: import only part of the history.

#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

use std::fs;
use std::path::{Path, PathBuf};

use recalla::cli;
use serde_json::Value;

const A1: &str = "aaaaaaa1-1111-4111-8111-111111111111";
const A2: &str = "aaaaaaa2-2222-4222-8222-222222222222";
const B1: &str = "bbbbbbb1-3333-4333-8333-333333333333";
const C1: &str = "ccccccc1-4444-4444-8444-444444444444";
const CODEX: &str = "019b7a10-0000-7000-8000-0000000000aa";

struct Env {
    _tmp: tempfile::TempDir,
    claude: PathBuf,
    codex: PathBuf,
    data: PathBuf,
    base: PathBuf,
}

fn claude_session(root: &Path, project: &str, id: &str, cwd: &str, text: &str) {
    let dir = root.join("projects").join(project);
    fs::create_dir_all(&dir).unwrap();
    let rec = format!(
        r#"{{"type":"user","uuid":"u-{id}","sessionId":"{id}","timestamp":"2025-01-02T03:04:05.678Z","cwd":"{cwd}","message":{{"role":"user","content":"{text}"}}}}"#
    );
    fs::write(dir.join(format!("{id}.jsonl")), format!("{rec}\n")).unwrap();
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let (claude, codex) = (base.join("claude"), base.join("codex"));
        // alpha: two sessions; alpha-extra: a sibling whose name starts the same;
        // beta: unrelated.
        claude_session(&claude, "-work-alpha", A1, "/work/alpha", "alpha one");
        claude_session(
            &claude,
            "-work-alpha",
            A2,
            "/work/alpha/crates/x",
            "alpha two",
        );
        claude_session(
            &claude,
            "-work-alpha-extra",
            C1,
            "/work/alpha-extra",
            "extra",
        );
        claude_session(&claude, "-work-beta", B1, "/work/beta", "beta one");
        let d = codex.join("sessions/2026/01/02");
        fs::create_dir_all(&d).unwrap();
        let meta = format!(
            r#"{{"timestamp":"2026-01-02T10:00:00.000Z","type":"session_meta","payload":{{"id":"{CODEX}","timestamp":"2026-01-02T10:00:00.000Z","cwd":"/work/alpha/sub"}}}}"#
        );
        let msg = r#"{"timestamp":"2026-01-02T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"codex hello"}]}}"#;
        fs::write(
            d.join(format!("rollout-2026-01-02T10-00-00-{CODEX}.jsonl")),
            format!("{meta}\n{msg}\n"),
        )
        .unwrap();
        Self {
            data: base.join("data"),
            claude,
            codex,
            base,
            _tmp: tmp,
        }
    }

    fn cli(&self, extra: &[&str]) -> (u8, String, String) {
        let mut args = vec![
            "recalla",
            "backfill",
            "--no-extract",
            "--no-mirror",
            "--json",
        ];
        args.extend(["--root", self.claude.to_str().unwrap()]);
        args.extend(["--codex-root", self.codex.to_str().unwrap()]);
        args.extend(["--opencode-root", self.codex.to_str().unwrap()]);
        args.extend(["--data-dir", self.data.to_str().unwrap()]);
        args.extend(extra);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = cli::run(args, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn seen(&self, extra: &[&str]) -> (u64, u64) {
        let (code, out, err) = self.cli(extra);
        assert_eq!(code, 0, "{err}");
        let v: Value = serde_json::from_str(&out).unwrap();
        (
            v["files"]["seen"].as_u64().unwrap(),
            v["files"]["new"].as_u64().unwrap(),
        )
    }
}

#[test]
fn dir_imports_that_project_and_its_children_only() {
    let env = Env::new();
    // alpha (2 claude) + codex session run in /work/alpha/sub; not alpha-extra, not beta.
    assert_eq!(env.seen(&["--dir", "/work/alpha"]), (3, 3));
}

#[test]
fn dir_that_is_a_sibling_prefix_does_not_leak() {
    let env = Env::new();
    assert_eq!(env.seen(&["--dir", "/work/alpha-extra"]), (1, 1));
    assert_eq!(env.seen(&["--dir", "/work/beta"]), (1, 1));
}

#[test]
fn session_by_full_id_fragment_and_file_path() {
    let env = Env::new();
    assert_eq!(env.seen(&["--session", B1]), (1, 1));
    assert_eq!(env.seen(&["--session", &A1[..8]]), (1, 1));
    let file = env
        .claude
        .join("projects/-work-alpha")
        .join(format!("{A2}.jsonl"));
    assert_eq!(env.seen(&["--session", file.to_str().unwrap()]), (1, 1));
    assert_eq!(env.seen(&["--session", CODEX]), (1, 1));
}

#[test]
fn dir_and_session_combine_as_a_union() {
    let env = Env::new();
    assert_eq!(env.seen(&["--dir", "/work/beta", "--session", C1]), (2, 2));
}

#[test]
fn repeated_dirs_union_and_dedupe_overlap() {
    let env = Env::new();
    // The second dir is inside the first: each file is still imported once.
    assert_eq!(
        env.seen(&["--dir", "/work/alpha", "--dir", "/work/alpha/crates"]),
        (3, 3)
    );
}

#[test]
fn partial_import_then_full_backfill_finishes_the_rest_without_duplicates() {
    let env = Env::new();
    assert_eq!(env.seen(&["--session", B1]), (1, 1));
    let (code, out, _) = env.cli(&[]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["files"]["new"], 4);
    assert_eq!(v["files"]["unchanged"], 1);
    // Beta's event was imported by the first run, not again.
    assert_eq!(v["events_added"], 4);
}

#[test]
fn unmatched_selector_fails_before_writing_anything() {
    let env = Env::new();
    let (code, out, err) = env.cli(&[
        "--dir",
        "/work/alpha",
        "--session",
        "deadbeefdead",
        "--dir",
        "/nope",
    ]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.contains("nothing matched"), "{err}");
    assert!(
        err.contains("--session deadbeefdead") && err.contains("--dir /nope"),
        "{err}"
    );
    assert!(
        !env.data.exists(),
        "a failed selection must not create the data dir"
    );
}

#[test]
fn too_short_a_session_fragment_is_not_a_wildcard() {
    let env = Env::new();
    let (code, _, err) = env.cli(&["--session", "aaaa"]);
    assert_eq!(code, 2);
    assert!(err.contains("nothing matched"), "{err}");
}

#[test]
fn list_shows_matches_and_writes_nothing() {
    let env = Env::new();
    let (code, out, err) = env.cli(&["--list", "--dir", "/work/alpha"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let rows = v["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(
        |r| r["cwd"].as_str().unwrap().starts_with("/work/alpha/") || r["cwd"] == "/work/alpha"
    ));
    assert!(!env.data.exists());
    let (code, out, _) = env.cli(&["--list"]);
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn dry_run_with_a_selection_writes_nothing() {
    let env = Env::new();
    let (code, out, _) = env.cli(&["--dry-run", "--dir", "/work/beta"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["files"]["seen"], 1);
    assert!(!env.data.exists());
}

#[cfg(unix)] // creating symlinks needs a privilege on Windows
#[test]
fn dir_works_through_a_symlinked_path() {
    let env = Env::new();
    // A real project dir reached through a symlink must still match its sessions.
    let real = env.base.join("real-proj");
    fs::create_dir_all(&real).unwrap();
    let cwd = real.to_str().unwrap();
    claude_session(
        &env.claude,
        "-real-proj",
        "ddddddd1-5555-4555-8555-555555555555",
        cwd,
        "sym",
    );
    let link = env.base.join("link-proj");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(env.seen(&["--dir", link.to_str().unwrap()]), (1, 1));
}

#[test]
fn the_selected_sessions_can_be_extracted_and_searched() {
    let env = Env::new();
    // The extractor runs only over what was imported.
    let mut args = vec![
        "recalla",
        "backfill",
        "--no-mirror",
        "--json",
        "--dir",
        "/work/beta",
    ];
    args.extend(["--root", env.claude.to_str().unwrap()]);
    args.extend(["--codex-root", env.codex.to_str().unwrap()]);
    args.extend(["--opencode-root", env.codex.to_str().unwrap()]);
    args.extend(["--data-dir", env.data.to_str().unwrap()]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        cli::run(args, &mut out, &mut err),
        0,
        "{}",
        String::from_utf8_lossy(&err)
    );
    let v: Value = serde_json::from_str(&String::from_utf8(out).unwrap()).unwrap();
    assert!(v["ingest"]["files"]["seen"] == 1 && v["extract"].is_object());
}

fn pick(env: &Env, extra: &[&str], answers: &str) -> (i32, String, String) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_recalla"))
        .args(["backfill", "--pick", "--no-extract", "--no-mirror"])
        .args(["--root", env.claude.to_str().unwrap()])
        .args(["--codex-root", env.codex.to_str().unwrap()])
        .args(["--opencode-root", env.codex.to_str().unwrap()])
        .args(["--data-dir", env.data.to_str().unwrap()])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(answers.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    (
        o.status.code().unwrap(),
        String::from_utf8(o.stdout).unwrap(),
        String::from_utf8(o.stderr).unwrap(),
    )
}

/// The menu number printed next to the project line containing `needle`.
fn menu_number(menu: &str, needle: &str) -> usize {
    menu.lines()
        .find(|l| l.contains(needle) && l.contains(')'))
        .and_then(|l| l.split(')').next()?.trim().parse().ok())
        .unwrap_or_else(|| panic!("no menu line for {needle}:\n{menu}"))
}

#[test]
fn pick_chooses_a_tool_then_projects() {
    let env = Env::new();
    // Dry pass to read the menu (cancel at the project prompt).
    let (_, menu, _) = pick(&env, &[], "1\nq\n");
    assert!(
        menu.contains("claude-code (4 sessions)") && menu.contains("codex (1 sessions)"),
        "{menu}"
    );
    assert!(
        menu.contains("1 session ·") && menu.contains("last active"),
        "{menu}"
    );
    let beta = menu_number(&menu, "/work/beta");
    let extra = menu_number(&menu, "/work/alpha-extra");
    // Two projects with one session each: choose them all.
    let (code, out, err) = pick(&env, &[], &format!("1\n{beta},{extra}\na\n"));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("2 sessions"), "{out}");
    // Exactly two files were imported: a follow-up full run sees three new.
    let (_, json, _) = env.cli(&[]);
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["files"]["unchanged"], 2, "{json}");
    assert_eq!(v["files"]["new"], 3);
}

#[test]
fn pick_can_choose_individual_sessions() {
    let env = Env::new();
    let (_, menu, _) = pick(&env, &[], "1\nq\n");
    let beta = menu_number(&menu, "/work/beta");
    let extra = menu_number(&menu, "/work/alpha-extra");
    let (code, out, err) = pick(&env, &[], &format!("1\n{beta},{extra}\nc\n1\n"));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Import which sessions?"), "{out}");
    let (_, json, _) = env.cli(&[]);
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["files"]["unchanged"], 1, "{json}");
}

#[test]
fn pick_codex_all() {
    let env = Env::new();
    let (code, _, err) = pick(&env, &[], "2\na\n");
    assert_eq!(code, 0, "{err}");
    let (_, json, _) = env.cli(&[]);
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["files"]["unchanged"], 1);
}

#[test]
fn pick_reprompts_on_bad_input_and_cancels_cleanly() {
    let env = Env::new();
    let (code, out, _) = pick(&env, &[], "9\nzzz\nq\n");
    assert_eq!(code, 0);
    assert!(out.contains("not a number from 1 to 2"), "{out}");
    assert!(!env.data.exists());
    // End of input and an empty answer also cancel without writing.
    assert_eq!(pick(&env, &[], "").0, 0);
    assert_eq!(pick(&env, &[], "1\n\n").0, 0);
    assert!(!env.data.exists());
}

#[test]
fn pick_conflicts_with_explicit_selectors() {
    let env = Env::new();
    let (code, _, err) = pick(&env, &["--dir", "/work/beta"], "");
    assert_eq!(code, 2, "{err}");
}
