//! `recalla doctor`: every check against a hermetic environment (temp dirs, injected
//! `PATH` and clock), plus the CLI's exit codes and JSON.

#![allow(clippy::unwrap_used)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::Env;
use recalla::doctor::{self, Check, Inputs, Level};
use recalla::hooks_cfg;
use recalla::model::{Kind, NewMemory, Origin, Provenance, Scope};
use recalla::ops::{self, PackArgs};
use recalla::store::Via;
use serde_json::{Value, json};

const PROJECT: &str = "/work/doctor-it";
/// One day after the fixture sessions (2026-11-01).
const NOW: i64 = 1_793_491_200_000 + 86_400_000;

struct Rig {
    env: Env,
    dir: tempfile::TempDir,
    exe: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // A real file standing in for the running binary.
        let exe = fs::canonicalize(dir.path()).unwrap().join("bin/recalla");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "#!/bin/sh\n").unwrap();
        Self {
            env: Env::new(),
            dir,
            exe,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn inputs(&self) -> Inputs {
        Inputs {
            data_dir: self.env.data.clone(),
            settings: Some(self.path("settings.json")),
            claude_json: Some(self.path("claude.json")),
            codex_config: Some(self.path("codex.toml")),
            exe: self.exe.clone(),
            path_var: Some(OsString::from(self.exe.parent().unwrap())),
            now_ms: NOW,
        }
    }

    fn run(&self) -> Vec<Check> {
        doctor::run(&self.inputs())
    }

    /// A database with one memory, one captured session and one injection.
    fn seed_db(&self) {
        let mut store = ops::open_or_create(&self.env.data).unwrap();
        store
            .insert(
                &NewMemory {
                    kind: Kind::Preference,
                    text: "Doctor tests prefer the green lane".into(),
                    scope: Scope::Project(PROJECT.into()),
                    confidence: 0.9,
                    importance: 3,
                },
                &Provenance::new(Origin::Remember),
            )
            .unwrap();
        drop(store);
        self.env.write_session(&json!({
            "id": "doc", "cwd": PROJECT,
            "turns": [{"user": "Always run make check before pushing in this repo."}],
        }));
        self.env.ingest();
        let mut store = self.env.store();
        ops::pack_with(
            &mut store,
            &PackArgs {
                budget: 1500,
                project: Some(PROJECT.into()),
                query: None,
                no_log: false,
                via: Via::Hook,
                session_id: None,
            },
        )
        .unwrap();
        recalla::mirror::sync(&mut store, &self.env.data.join("mirror"), true).unwrap();
    }

    fn install_hooks(&self) {
        hooks_cfg::install(&self.path("settings.json"), &self.exe, false).unwrap();
    }
}

fn level(checks: &[Check], name: &str) -> Level {
    find(checks, name).level
}

fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks.iter().find(|c| c.name == name).unwrap_or_else(|| {
        panic!(
            "no check {name}: {:?}",
            checks.iter().map(|c| c.name).collect::<Vec<_>>()
        )
    })
}

fn healthy() -> Rig {
    let r = Rig::new();
    r.seed_db();
    r.install_hooks();
    fs::write(
        r.path("claude.json"),
        json!({"mcpServers": {"recalla": {"command": "x"}}}).to_string(),
    )
    .unwrap();
    fs::write(r.path("codex.toml"), hooks_cfg::mcp_codex_toml(&r.exe)).unwrap();
    r
}

#[test]
fn healthy_setup_has_no_failures_or_warnings() {
    let r = healthy();
    let checks = r.run();
    for c in &checks {
        assert_eq!(
            c.level,
            Level::Ok,
            "{} -> {} ({:?})",
            c.name,
            c.detail,
            c.hint
        );
    }
    assert!(!doctor::failed(&checks));
    assert!(doctor::render_text(&checks).ends_with("0 failed, 0 warnings\n"));
}

#[test]
fn nothing_installed_fails_hooks_and_database_with_fixes() {
    let r = Rig::new();
    let checks = r.run();
    assert_eq!(level(&checks, "hooks"), Level::Fail);
    assert_eq!(
        find(&checks, "hooks").hint.as_deref(),
        Some("recalla hooks install")
    );
    assert_eq!(level(&checks, "database"), Level::Fail);
    assert!(
        find(&checks, "database")
            .hint
            .as_deref()
            .unwrap()
            .contains("backfill")
    );
    for dependent in ["last capture", "last injection", "mirror"] {
        assert_eq!(level(&checks, dependent), Level::Skip, "{dependent}");
    }
    assert!(doctor::failed(&checks));
}

#[test]
fn partial_hooks_warn_and_name_the_missing_events() {
    let r = healthy();
    let path = r.path("settings.json");
    let mut v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    v["hooks"].as_object_mut().unwrap().remove("Stop");
    fs::write(&path, v.to_string()).unwrap();
    let c = r.run();
    let hooks = find(&c, "hooks");
    assert_eq!(hooks.level, Level::Warn);
    assert!(hooks.detail.contains("Stop"), "{}", hooks.detail);
}

#[test]
fn malformed_or_missing_settings_fail_the_hooks_check() {
    let r = healthy();
    fs::write(r.path("settings.json"), "{ not json").unwrap();
    assert_eq!(level(&r.run(), "hooks"), Level::Fail);
    let mut inputs = r.inputs();
    inputs.settings = None;
    assert_eq!(level(&doctor::run(&inputs), "hooks"), Level::Fail);
    // A missing file is "not installed", not a crash.
    fs::remove_file(r.path("settings.json")).unwrap();
    assert_eq!(level(&r.run(), "hooks"), Level::Fail);
}

#[test]
fn binary_check_compares_path_with_the_running_exe() {
    let r = Rig::new();
    assert_eq!(level(&r.run(), "binary"), Level::Ok);

    let mut inputs = r.inputs();
    inputs.path_var = Some(OsString::from(r.path("empty")));
    let c = doctor::run(&inputs);
    assert_eq!(level(&c, "binary"), Level::Warn);
    assert!(find(&c, "binary").detail.contains("not on PATH"));

    // A different recalla earlier on PATH.
    let other = r.path("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("recalla"), "x").unwrap();
    inputs.path_var = Some(std::env::join_paths([&other, r.exe.parent().unwrap()]).unwrap());
    let c = doctor::run(&inputs);
    assert_eq!(level(&c, "binary"), Level::Warn);
    assert!(find(&c, "binary").detail.contains("resolves"));

    // A symlink to the same binary is fine.
    let link_dir = r.path("links");
    fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&r.exe, link_dir.join("recalla")).unwrap();
    inputs.path_var = Some(OsString::from(&link_dir));
    assert_eq!(level(&doctor::run(&inputs), "binary"), Level::Ok);

    inputs.path_var = None;
    assert_eq!(level(&doctor::run(&inputs), "binary"), Level::Warn);
}

#[test]
fn claude_mcp_user_project_missing_and_broken() {
    let r = healthy();
    let cj = r.path("claude.json");
    assert!(find(&r.run(), "mcp (claude)").detail.contains("user"));

    fs::write(
        &cj,
        json!({"projects": {"/p": {"mcpServers": {"recalla": {}}}}}).to_string(),
    )
    .unwrap();
    assert!(find(&r.run(), "mcp (claude)").detail.contains("project"));

    fs::write(&cj, json!({"mcpServers": {"other": {}}}).to_string()).unwrap();
    let c = r.run();
    assert_eq!(level(&c, "mcp (claude)"), Level::Warn);
    assert!(
        find(&c, "mcp (claude)")
            .hint
            .as_deref()
            .unwrap()
            .contains("claude mcp add")
    );

    fs::write(&cj, "{ broken").unwrap();
    assert_eq!(level(&r.run(), "mcp (claude)"), Level::Warn);

    fs::remove_file(&cj).unwrap();
    assert_eq!(
        level(&r.run(), "mcp (claude)"),
        Level::Skip,
        "no Claude Code is not a problem"
    );
}

#[test]
fn codex_mcp_registered_unapproved_missing_and_absent() {
    let r = healthy();
    let cfg = r.path("codex.toml");
    assert_eq!(level(&r.run(), "mcp (codex)"), Level::Ok);

    // The old snippet, without per-tool approval, is blocked by non-interactive Codex.
    fs::write(
        &cfg,
        format!(
            "[mcp_servers.recalla]\ncommand = \"{}\"\nargs = [\"mcp\"]\n",
            r.exe.display()
        ),
    )
    .unwrap();
    let c = r.run();
    assert_eq!(level(&c, "mcp (codex)"), Level::Warn);
    assert!(find(&c, "mcp (codex)").detail.contains("not auto-approved"));

    // An unrelated server must not count as ours.
    fs::write(&cfg, "[mcp_servers.recalla_other]\ncommand = \"x\"\n").unwrap();
    assert!(
        find(&r.run(), "mcp (codex)")
            .detail
            .contains("not registered")
    );

    fs::remove_file(&cfg).unwrap();
    assert_eq!(level(&r.run(), "mcp (codex)"), Level::Skip);
}

#[test]
fn newer_or_older_schema_fails_with_a_migration_hint() {
    let r = Rig::new();
    fs::create_dir_all(&r.env.data).unwrap();
    let db = recalla::paths::db_path(&r.env.data);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);
    let c = r.run();
    assert_eq!(level(&c, "database"), Level::Fail);
    assert!(find(&c, "database").detail.contains("schema v1"));
    assert!(find(&c, "database").hint.is_some());

    fs::write(&db, "not a sqlite file at all").unwrap();
    assert_eq!(level(&r.run(), "database"), Level::Fail);
}

#[test]
fn data_dir_open_to_others_warns() {
    let r = healthy();
    fs::set_permissions(&r.env.data, fs::Permissions::from_mode(0o755)).unwrap();
    let c = r.run();
    assert_eq!(level(&c, "data dir mode"), Level::Warn);
    assert!(
        find(&c, "data dir mode")
            .hint
            .as_deref()
            .unwrap()
            .starts_with("chmod 700")
    );
    fs::set_permissions(&r.env.data, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(level(&r.run(), "data dir mode"), Level::Ok);
}

#[test]
fn stale_or_missing_capture_and_injection_warn() {
    let r = healthy();
    let mut inputs = r.inputs();
    // Clock 30 days later: the capture is old.
    inputs.now_ms = NOW + 30 * 86_400_000;
    let c = doctor::run(&inputs);
    assert_eq!(level(&c, "last capture"), Level::Warn);
    assert!(find(&c, "last capture").detail.contains("days ago"));

    // A database with memories but no sessions and no injections.
    let r = Rig::new();
    drop(ops::open_or_create(&r.env.data).unwrap());
    let c = r.run();
    assert_eq!(level(&c, "last capture"), Level::Warn);
    assert_eq!(level(&c, "last injection"), Level::Warn);
    assert!(find(&c, "last injection").detail.contains("no pack"));
}

#[test]
fn unsynced_mirror_warns_until_synced() {
    let r = healthy();
    let mut store = r.env.store();
    store
        .insert(
            &NewMemory {
                kind: Kind::Decision,
                text: "A fresh unmirrored decision for the doctor test".into(),
                scope: Scope::Project(PROJECT.into()),
                confidence: 0.8,
                importance: 2,
            },
            &Provenance::new(Origin::Remember),
        )
        .unwrap();
    let c = r.run();
    assert_eq!(level(&c, "mirror"), Level::Warn);
    assert!(
        find(&c, "mirror")
            .hint
            .as_deref()
            .unwrap()
            .contains("recalla mirror")
    );
    recalla::mirror::sync(&mut store, &r.env.data.join("mirror"), false).unwrap();
    assert_eq!(level(&r.run(), "mirror"), Level::Ok);
}

#[test]
fn hook_log_errors_surface_one_line_and_never_echo_control_chars() {
    let r = healthy();
    let log = r.env.data.join("log");
    fs::create_dir_all(&log).unwrap();
    fs::write(
        log.join("hook.log"),
        "1795402977 capture refused: outside root\n",
    )
    .unwrap();
    let c = r.run();
    let hl = find(&c, "hook log");
    assert_eq!(hl.level, Level::Warn);
    assert!(hl.detail.contains("outside root"), "{}", hl.detail);
    assert!(!hl.detail.contains('\n'));
}

#[test]
fn json_output_is_stable_and_has_no_missing_fields() {
    let r = healthy();
    let v = doctor::render_json(&r.run());
    assert_eq!(v["ok"], true);
    let names: Vec<&str> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "binary",
            "hooks",
            "mcp (claude)",
            "mcp (codex)",
            "database",
            "data dir mode",
            "last capture",
            "last injection",
            "mirror",
            "hook log"
        ]
    );
    for c in v["checks"].as_array().unwrap() {
        assert!(c["level"].is_string() && c["detail"].is_string() && c.get("hint").is_some());
    }
}

/// Doctor must never write: no database, mirror or log appears from running it.
#[test]
fn doctor_is_read_only() {
    let r = Rig::new();
    let _ = r.run();
    assert!(!r.env.data.exists(), "doctor created the data dir");
    let r = healthy();
    let before = snapshot(&r.env.data);
    let _ = r.run();
    assert_eq!(before, snapshot(&r.env.data));
}

fn snapshot(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else if !e.file_name().to_string_lossy().ends_with("-shm")
                && !e.file_name().to_string_lossy().ends_with("-wal")
            {
                // SQLite sidecars appear on any read-only open of a WAL database.
                out.push((e.path(), m.len()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn cli_exit_codes_and_json() {
    let r = Rig::new();
    let (code, out, _) = r.env.cli(&["doctor", "--json"]);
    // The real HOME is not trusted here, but a fresh data dir always fails the database check.
    assert_eq!(code, 1, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], false);
    let db = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "database")
        .unwrap();
    assert_eq!(db["level"], "fail");

    let (code, out, _) = r.env.cli(&["doctor"]);
    assert_eq!(code, 1);
    assert!(out.contains("fix:") && out.contains("failed"), "{out}");
}
