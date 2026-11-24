//! `recalla hooks install|uninstall|status` over temp settings files.

#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use recalla::hooks_cfg;
use serde_json::{Value, json};

const EXE: &str = "/opt/recalla/bin/recalla";

fn settings(dir: &Path, text: &str) -> PathBuf {
    let p = dir.join("settings.json");
    fs::write(&p, text).unwrap();
    p
}

fn read(p: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

fn install(p: &Path) -> hooks_cfg::Outcome {
    hooks_cfg::install(p, Path::new(EXE), false).unwrap()
}

fn backups(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains(".recalla-bak-"))
        .collect()
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

const BUSY_FILE: &str = r#"{
    "model": "opus",
    "permissions": {"allow": ["Bash(ls)"]},
    "hooks": {
        "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]}],
        "Stop": [
            {"hooks": [{"type": "command", "command": "notify.sh"}]},
            {"matcher": "x", "hooks": [{"type": "command", "command": "/old/recalla hook capture", "timeout": 3},
                                       {"type": "command", "command": "other.sh"}]}
        ]
    },
    "env": {"A": "1"}
}
"#;

#[test]
fn installs_into_missing_and_empty_files() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope/settings.json");
    let o = install(&missing);
    assert!(o.changed && o.backup.is_none());
    let v = read(&missing);
    for (event, sub) in hooks_cfg::EVENTS {
        let h = &v["hooks"][event][0]["hooks"][0];
        assert_eq!(h["command"], format!("{EXE} hook {sub}"));
        assert_eq!(
            (h["type"].as_str(), h["timeout"].as_u64()),
            (Some("command"), Some(10))
        );
        assert!(v["hooks"][event][0].get("matcher").is_none());
    }
    let empty = settings(dir.path(), "");
    assert!(install(&empty).changed);
    assert_eq!(read(&empty), v);
}

#[test]
fn keeps_other_hooks_keys_order_and_indent() {
    let dir = tempfile::tempdir().unwrap();
    let p = settings(dir.path(), BUSY_FILE);
    let before = read(&p);
    let o = install(&p);
    assert!(o.changed);
    let after = read(&p);
    assert_eq!(keys(&after), keys(&before), "top-level key order kept");
    assert_eq!(
        keys(&after["hooks"]),
        ["PreToolUse", "Stop", "SessionStart", "PreCompact"]
    );
    for k in ["model", "permissions", "env"] {
        assert_eq!(after[k], before[k]);
    }
    assert_eq!(after["hooks"]["PreToolUse"], before["hooks"]["PreToolUse"]);
    // Stop: unrelated group untouched, our entry updated in place (matcher and sibling kept).
    let stop = after["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2);
    assert_eq!(stop[0], before["hooks"]["Stop"][0]);
    assert_eq!(stop[1]["matcher"], "x");
    assert_eq!(
        stop[1]["hooks"][0]["command"],
        format!("{EXE} hook capture")
    );
    assert_eq!(stop[1]["hooks"][1]["command"], "other.sh");
    // The file keeps its 4-space indent and trailing newline.
    let raw = fs::read_to_string(&p).unwrap();
    assert!(raw.contains("\n    \"model\""), "{raw}");
    assert!(raw.ends_with("}\n"));
}

#[test]
fn install_is_idempotent_and_updates_a_moved_binary() {
    let dir = tempfile::tempdir().unwrap();
    let p = settings(dir.path(), BUSY_FILE);
    install(&p);
    let once = fs::read_to_string(&p).unwrap();
    let n = backups(dir.path()).len();
    let again = install(&p);
    assert!(!again.changed && again.backup.is_none());
    assert_eq!(fs::read_to_string(&p).unwrap(), once);
    assert_eq!(backups(dir.path()).len(), n, "no-op writes no backup");

    let moved = hooks_cfg::install(&p, Path::new("/new place/recalla"), false).unwrap();
    assert!(moved.changed);
    let v = read(&p);
    assert_eq!(
        v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "\"/new place/recalla\" hook session-start"
    );
    assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
}

#[test]
fn malformed_or_wrong_shape_is_refused_and_untouched() {
    let dir = tempfile::tempdir().unwrap();
    for bad in [
        "{\"hooks\": ",
        "[1, 2]",
        "\"text\"",
        r#"{"hooks": []}"#,
        r#"{"hooks": {"Stop": {}}}"#,
    ] {
        let p = settings(dir.path(), bad);
        assert!(
            hooks_cfg::install(&p, Path::new(EXE), false).is_err(),
            "{bad}"
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), bad, "file untouched");
    }
    assert!(backups(dir.path()).is_empty());
}

#[test]
fn uninstall_restores_the_original() {
    let dir = tempfile::tempdir().unwrap();
    for original in [
        BUSY_FILE.replace("/old/recalla hook capture", "other2.sh"),
        "{}".into(),
        r#"{"model":"x"}"#.into(),
    ] {
        let p = settings(dir.path(), &original);
        let before = read(&p);
        install(&p);
        assert_ne!(read(&p), before);
        let o = hooks_cfg::uninstall(&p, false).unwrap();
        assert!(o.changed && o.backup.is_some());
        assert_eq!(read(&p), before);
        assert_eq!(keys(&read(&p)), keys(&before));
    }
}

#[test]
fn uninstall_removes_only_ours_and_prunes_empties() {
    let dir = tempfile::tempdir().unwrap();
    let p = settings(dir.path(), BUSY_FILE);
    install(&p);
    hooks_cfg::uninstall(&p, false).unwrap();
    let v = read(&p);
    // Our entry was inside a group with a sibling: group stays, sibling kept.
    assert_eq!(
        v["hooks"]["Stop"][1]["hooks"],
        json!([{"type": "command", "command": "other.sh"}])
    );
    assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
    assert!(v["hooks"].get("SessionStart").is_none() && v["hooks"].get("PreCompact").is_none());
    assert!(v["hooks"].get("PreToolUse").is_some());
    // Nothing installed: no change, no write.
    let again = hooks_cfg::uninstall(&p, false).unwrap();
    assert!(!again.changed && again.backup.is_none());
    // Missing file: nothing created.
    let gone = dir.path().join("none.json");
    assert!(!hooks_cfg::uninstall(&gone, false).unwrap().changed);
    assert!(!gone.exists());
}

#[test]
fn creates_a_backup_with_the_original_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let p = settings(dir.path(), BUSY_FILE);
    let o = install(&p);
    let bak = o.backup.unwrap();
    assert_eq!(fs::read_to_string(bak).unwrap(), BUSY_FILE);
    assert_eq!(backups(dir.path()).len(), 1);
    assert!(
        fs::read_dir(dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("tmp"))
    );
}

#[test]
fn dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let p = settings(dir.path(), BUSY_FILE);
    let o = hooks_cfg::install(&p, Path::new(EXE), true).unwrap();
    assert!(o.changed && o.backup.is_none());
    assert!(o.changes.iter().any(|l| l == "SessionStart: added"));
    assert_eq!(fs::read_to_string(&p).unwrap(), BUSY_FILE);
    assert!(backups(dir.path()).is_empty());
    let fresh = dir.path().join("fresh.json");
    hooks_cfg::install(&fresh, Path::new(EXE), true).unwrap();
    assert!(!fresh.exists());
}

#[cfg(unix)]
#[test]
fn file_mode_is_preserved_and_symlinks_are_followed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = settings(dir.path(), "{}");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
    install(&p);
    assert_eq!(
        fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o640
    );

    let real = dir.path().join("real.json");
    fs::write(&real, "{}").unwrap();
    let link = dir.path().join("link.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    install(&link);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(read(&real)["hooks"]["Stop"].is_array());
}

fn cli(args: &[&str], dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recalla"))
        .args(args)
        .arg("--data-dir")
        .arg(dir.join("data"))
        .output()
        .unwrap()
}

#[test]
fn cli_install_status_uninstall() {
    let dir = tempfile::tempdir().unwrap();
    let p = settings(dir.path(), r#"{"model":"opus"}"#);
    let ps = p.to_str().unwrap();

    let out = cli(&["hooks", "status", "--settings", ps], dir.path());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        out.status.success() && text.contains("SessionStart: not installed"),
        "{text}"
    );
    assert!(text.contains("database: missing"));

    let out = cli(&["hooks", "install", "--settings", ps], dir.path());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success() && text.contains("backup:"), "{text}");
    assert!(text.contains("claude mcp add --scope user recalla --"));
    assert!(text.contains("[mcp_servers.recalla]"));
    // The command points at the real, canonical binary.
    let cmd = read(&p)["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        cmd.ends_with(" hook capture") && cmd.contains("recalla"),
        "{cmd}"
    );

    let out = cli(&["hooks", "status", "--settings", ps], dir.path());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("Stop: installed") && text.contains("PreCompact: installed"),
        "{text}"
    );

    let out = cli(&["hooks", "uninstall", "--settings", ps], dir.path());
    assert!(out.status.success());
    assert_eq!(read(&p), json!({"model": "opus"}));

    fs::write(&p, "{broken").unwrap();
    let out = cli(&["hooks", "install", "--settings", ps], dir.path());
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("not valid JSON")
    );
    assert_eq!(fs::read_to_string(&p).unwrap(), "{broken");
}

#[test]
fn customized_and_unrelated_hooks_are_never_touched() {
    // Regression for F1: only commands exactly as `command_for` writes them
    // are ours; env prefixes, extra flags and mere mentions are left alone.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let custom = "RECALLA_BUDGET=3000 /opt/recalla hook capture --data-dir /Volumes/enc/recalla";
    let unrelated = "say recalla hook capture finished";
    let original = serde_json::json!({"hooks": {"Stop": [{"hooks": [
        {"type": "command", "command": custom},
        {"type": "command", "command": unrelated}
    ]}]}});
    std::fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();
    let exe = std::path::Path::new("/usr/local/bin/recalla");
    recalla::hooks_cfg::install(&path, exe, false).unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cmds: Vec<String> = after["hooks"]["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["hooks"].as_array().unwrap().iter())
        .map(|h| h["command"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        cmds.contains(&custom.to_owned()) && cmds.contains(&unrelated.to_owned()),
        "{cmds:?}"
    );
    recalla::hooks_cfg::uninstall(&path, false).unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(restored, original);
}

#[test]
fn backups_never_overwrite_each_other() {
    // Regression for F2: install + uninstall in the same second keep both.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{\"model\": \"x\"}\n").unwrap();
    let exe = std::path::Path::new("/usr/local/bin/recalla");
    recalla::hooks_cfg::install(&path, exe, false).unwrap();
    recalla::hooks_cfg::uninstall(&path, false).unwrap();
    let baks: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".recalla-bak-"))
        .collect();
    assert_eq!(baks.len(), 2);
    let pristine = baks
        .iter()
        .any(|e| std::fs::read_to_string(e.path()).unwrap() == "{\"model\": \"x\"}\n");
    assert!(pristine, "original bytes must survive in a backup");
}

#[test]
fn file_recall_is_opt_in_idempotent_and_leaves_other_pretooluse_entries_alone() {
    let dir = tempfile::tempdir().unwrap();
    let p = settings(dir.path(), BUSY_FILE);
    let before = read(&p);
    let exe = std::path::Path::new(EXE);

    // Default install does not touch PreToolUse.
    install(&p);
    assert_eq!(
        read(&p)["hooks"]["PreToolUse"],
        before["hooks"]["PreToolUse"]
    );

    let o = hooks_cfg::install_with(&p, exe, false, true).unwrap();
    assert!(o.changed);
    let groups = read(&p)["hooks"]["PreToolUse"].as_array().unwrap().clone();
    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups[0], before["hooks"]["PreToolUse"][0],
        "guard.sh untouched"
    );
    assert_eq!(groups[1]["matcher"], "Read|Edit|Write|MultiEdit");
    assert_eq!(
        groups[1]["hooks"][0]["command"],
        format!("{EXE} hook pre-tool-use")
    );
    assert!(
        !hooks_cfg::install_with(&p, exe, false, true)
            .unwrap()
            .changed,
        "idempotent"
    );

    // Uninstall removes it and only it.
    hooks_cfg::uninstall(&p, false).unwrap();
    let after = read(&p);
    assert_eq!(after["hooks"]["PreToolUse"], before["hooks"]["PreToolUse"]);
    assert!(!after.to_string().contains("pre-tool-use"));
}
