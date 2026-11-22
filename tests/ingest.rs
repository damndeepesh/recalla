//! End-to-end ingest over a copy of the synthetic Claude fixture tree.
//!
//! Everything runs in temp dirs passed through `Options`; no env vars.

#![allow(clippy::unwrap_used, clippy::cast_possible_truncation)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use recalla::archive::read_source;
use recalla::extract::{self, Options as ExtractOptions};
use recalla::harness::MAX_EVENT_BYTES;
use recalla::harness::claude::{discover, parse_bytes, parse_session};
use recalla::ingest::{Options, Report, run};
use recalla::model::{Kind, Status};
use recalla::paths;
use recalla::store::Store;
use serde_json::json;

const FIXTURES: &str = "tests/fixtures/claude";
const MAIN: &str = "projects/-tmp-demo/11111111-1111-4111-8111-111111111111.jsonl";
const SESSION: &str = "11111111-1111-4111-8111-111111111111";

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude");
        copy_tree(Path::new(FIXTURES), &root);
        // Ingest keys sources by canonical path (temp dirs may sit behind a
        // symlink, e.g. /var -> /private/var on macOS).
        let root = fs::canonicalize(root).unwrap();
        let data = tmp.path().join("data");
        Self {
            _tmp: tmp,
            root,
            data,
        }
    }

    fn opts(&self) -> Options {
        Options {
            claude_root: Some(self.root.clone()),
            codex_root: None,
            data_dir: self.data.clone(),
            archive: true,
            dry_run: false,
        }
    }

    fn run(&self) -> Report {
        run(&self.opts()).unwrap()
    }

    fn main(&self) -> PathBuf {
        self.root.join(MAIN)
    }

    fn store(&self) -> Store {
        Store::open(paths::db_path(&self.data)).unwrap()
    }

    fn append(&self, line: &str) {
        fs::OpenOptions::new()
            .append(true)
            .open(self.main())
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }
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

/// Events the harness yields for the whole fixture tree.
fn fixture_events(root: &Path) -> u64 {
    discover(root)
        .unwrap()
        .iter()
        .map(|f| parse_session(&f.path, 0).unwrap().0.len() as u64)
        .sum()
}

/// Pseudo-random alphanumerics, fresh per call, so no secret is a literal.
fn alnum(n: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            CHARS[(x % CHARS.len() as u64) as usize] as char
        })
        .collect()
}

fn gh_token() -> String {
    ["gh", "p_"].concat() + &alnum(36)
}

fn user_line(uuid: &str, text: &str) -> String {
    json!({
        "type": "user", "uuid": uuid, "parentUuid": null, "sessionId": SESSION,
        "timestamp": "2025-01-02T03:05:00Z", "cwd": "/tmp/demo", "gitBranch": "main",
        "isSidechain": false, "message": {"role": "user", "content": text}
    })
    .to_string()
        + "\n"
}

fn db_bytes(data: &Path) -> Vec<u8> {
    let mut all = fs::read(paths::db_path(data)).unwrap();
    for ext in ["-wal", "-shm"] {
        let mut p = paths::db_path(data).into_os_string();
        p.push(ext);
        if let Ok(b) = fs::read(PathBuf::from(p)) {
            all.extend(b);
        }
    }
    all
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn first_run_imports_everything_then_second_is_a_noop() {
    let env = Env::new();
    let n = fixture_events(&env.root);
    assert!(n > 0);

    let first = env.run();
    assert_eq!(first.files_seen, 3);
    assert_eq!(first.files_new, 3);
    assert_eq!(first.failed, []);
    assert_eq!(first.events_added, n);
    assert_eq!(env.store().event_count().unwrap(), n);
    assert!(first.archive_raw_bytes > first.archive_compressed_bytes);

    let second = env.run();
    assert_eq!(second.events_added, 0);
    assert_eq!(second.files_unchanged, 3);
    assert_eq!(
        (
            second.files_new,
            second.files_resumed,
            second.files_restarted
        ),
        (0, 0, 0)
    );
    assert_eq!(second.archive_raw_bytes, 0);
    assert_eq!(env.store().event_count().unwrap(), n);
}

#[test]
fn appended_line_is_resumed() {
    let env = Env::new();
    let n = env.run().events_added;
    env.append(&user_line("d0000001", "one more question"));
    let r = env.run();
    assert_eq!(r.events_added, 1);
    assert_eq!((r.files_resumed, r.files_unchanged), (1, 2));
    assert_eq!(env.store().event_count().unwrap(), n + 1);
}

#[test]
fn root_spellings_share_one_source_per_file() {
    let env = Env::new();
    let n = env.run().events_added;
    let parent = env.root.parent().unwrap();
    let mut spellings = vec![
        parent.join(".").join("claude"),
        env.root.join("projects").join(".."),
    ];
    #[cfg(unix)]
    {
        let link = parent.join("claude-link");
        std::os::unix::fs::symlink(&env.root, &link).unwrap();
        spellings.push(link);
    }
    for root in spellings {
        let r = run(&Options {
            claude_root: Some(root.clone()),
            codex_root: None,
            ..env.opts()
        })
        .unwrap();
        assert_eq!((r.events_added, r.files_unchanged), (0, 3), "{root:?}");
    }
    let store = env.store();
    assert_eq!(store.event_count().unwrap(), n);
    let canonical = fs::canonicalize(&env.root).unwrap();
    let sources = store.sources().unwrap();
    assert_eq!(sources.len(), 3);
    assert!(
        sources
            .iter()
            .all(|s| Path::new(&s.path).starts_with(&canonical)),
        "{sources:?}"
    );
}

#[test]
fn parse_bytes_matches_parse_session_from_any_line_start() {
    let env = Env::new();
    env.append("{\"partial\": tr"); // unterminated tail
    let bytes = fs::read(env.main()).unwrap();
    let line_starts = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i + 1);
    for start in std::iter::once(0).chain(line_starts) {
        let (ev, stats, next) = parse_session(&env.main(), start as u64).unwrap();
        let line = recalla::harness::count_newlines(&bytes[..start], start as u64).unwrap();
        let (ev2, stats2, rel) = parse_bytes(&bytes[start..], start as u64, line, &env.main());
        assert_eq!((ev2, rel), (ev, next - start as u64), "start {start}");
        assert_eq!(stats2.lines, stats.lines);
    }
}

#[test]
fn malformed_lines_are_reported() {
    let env = Env::new();
    // The fixtures hold some malformed lines already; later runs only see new ones.
    assert!(env.run().malformed_lines > 0);
    env.append("this is not json\n{\"broken\": \n");
    let r = env.run();
    assert_eq!((r.malformed_lines, r.events_added), (2, 0));
    assert_eq!(r.to_json()["malformed_lines"], 2);

    let (root, data) = (env.root.to_str().unwrap(), env.data.to_str().unwrap());
    env.append("still not json\n");
    let args = [
        "recalla",
        "backfill",
        "--root",
        root,
        "--harness",
        "claude",
        "--data-dir",
        data,
    ];
    let (_, out, _) = cli(&args);
    assert!(out.contains("1 malformed lines skipped"), "{out}");
    env.append("nope again\n");
    let (_, out, _) = cli(&[&args[..], &["--json"]].concat());
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["ingest"]["malformed_lines"], 1);
}

#[test]
fn file_whose_first_ingest_failed_is_new_next_time() {
    let env = Env::new();
    // A file where the archive dir belongs makes every first ingest fail
    // after the source row has been created.
    fs::create_dir_all(&env.data).unwrap();
    fs::write(paths::archive_dir(&env.data), b"in the way").unwrap();
    let failed = env.run();
    assert_eq!(
        (failed.failed.len(), failed.events_added),
        (3, 0),
        "{failed:?}"
    );
    assert_eq!(env.store().sources().unwrap().len(), 3);

    fs::remove_file(paths::archive_dir(&env.data)).unwrap();
    let r = env.run();
    assert_eq!(r.failed, []);
    assert_eq!((r.files_new, r.files_restarted), (3, 0), "{r:?}");
    assert_eq!(r.events_added, fixture_events(&env.root));
}

#[test]
fn rewritten_file_is_restarted() {
    let env = Env::new();
    let n = env.run().events_added;
    let key = env.main().to_string_lossy().into_owned();
    let before = env.store().source(&key).unwrap().unwrap().events;

    let replacement = user_line("e0000001", "rewritten first") + &user_line("e0000002", "second");
    fs::write(env.main(), replacement).unwrap();
    let r = env.run();
    assert_eq!(r.files_restarted, 1);
    assert_eq!(r.files_unchanged, 2);
    assert_eq!(r.events_added, 2);
    let store = env.store();
    assert_eq!(store.source(&key).unwrap().unwrap().events, 2);
    assert_eq!(store.event_count().unwrap(), n - before + 2);
    // The archive follows the new content too.
    let id = store.source(&key).unwrap().unwrap().id;
    let restored = read_source(&store, &paths::archive_dir(&env.data), id).unwrap();
    assert_eq!(restored, fs::read(env.main()).unwrap());
}

#[test]
fn rewrite_past_the_identity_prefix_is_restarted() {
    let env = Env::new();
    let old: String = (0..40)
        .map(|i| {
            user_line(
                &format!("o{i:07}"),
                &format!("old text {i} {}", "x".repeat(120)),
            )
        })
        .collect();
    assert!(old.len() > 2 * 4096);
    fs::write(env.main(), &old).unwrap();
    env.run();

    // Same first 4 KB (so the prefix identity matches), later lines differ
    // and the file grows past the old checkpoint.
    let mut new = old.as_bytes()[..4096].to_vec();
    new.extend(b"\n");
    new.extend((0..60).flat_map(|i| {
        user_line(
            &format!("n{i:07}"),
            &format!("new text {i} {}", "y".repeat(150)),
        )
        .into_bytes()
    }));
    fs::write(env.main(), &new).unwrap();
    assert!(new.len() > old.len());

    let r = env.run();
    assert_eq!((r.files_restarted, r.files_resumed), (1, 0), "{r:?}");
    let store = env.store();
    let key = env.main().to_string_lossy().into_owned();
    let src = store.source(&key).unwrap().unwrap();
    assert_eq!(
        src.events,
        parse_session(&env.main(), 0).unwrap().0.len() as u64
    );
    let values = store.all_text_values().unwrap();
    assert!(
        !values.iter().any(|v| v.starts_with("old text 39 ")),
        "stale text"
    );
    assert!(values.iter().any(|v| v.starts_with("new text 59 ")));
    let restored = read_source(&store, &paths::archive_dir(&env.data), src.id).unwrap();
    assert_eq!(restored, new);
}

#[test]
fn runtime_secrets_never_reach_the_db() {
    let env = Env::new();
    let token = gh_token();
    let pw = alnum(20);
    let assignment = ["pass", "word="].concat() + &pw;
    env.append(&user_line(
        "f0000001",
        &format!("deploy with {token} and {assignment}"),
    ));
    let tool = json!({
        "type": "assistant", "uuid": "f0000002", "sessionId": SESSION,
        "timestamp": "2025-01-02T03:06:00Z",
        "cwd": format!("/tmp/{token}/work"), "gitBranch": format!("fix/{token}"),
        "message": {"role": "assistant", "content": [{
            "type": "tool_use", "id": "toolu_secret01", "name": token,
            "input": {"command": format!("GH_TOKEN={token} run --{assignment}")}
        }]}
    })
    .to_string()
        + "\n";
    env.append(&tool);

    let r = env.run();
    assert_eq!(r.failed, []);
    assert!(r.redactions() >= 5, "{:?}", r.redactions_by_rule);
    assert!(r.redactions_by_rule.contains_key("github-token"));

    let values = env.store().all_text_values().unwrap();
    for v in &values {
        assert!(!v.contains(&token), "github token leaked into the db");
        assert!(!v.contains(&pw), "password leaked into the db");
    }
    assert!(values.iter().any(|v| v.contains("[REDACTED:")));
    // Nothing in any page of the file either (FTS shadow tables, WAL).
    let raw = db_bytes(&env.data);
    assert!(!contains(&raw, &token));
    assert!(!contains(&raw, &pw));
}

#[test]
fn long_event_is_truncated_after_redaction() {
    let env = Env::new();
    let token = gh_token();
    let long = format!("{} {token} {}", "a ".repeat(25_000), "b ".repeat(25_000));
    assert!(long.len() > 100_000);
    env.append(&user_line("g0000001", &long));
    let r = env.run();
    assert_eq!(r.redactions_by_rule.get("github-token"), Some(&1));

    let values = env.store().all_text_values().unwrap();
    let stored = values
        .iter()
        .find(|v| v.starts_with("a a a") && v.contains("bytes omitted"))
        .unwrap();
    assert!(stored.len() <= MAX_EVENT_BYTES + 64, "{}", stored.len());
    assert!(values.iter().all(|v| !v.contains(&token)));
}

#[test]
fn archive_reproduces_sources_byte_for_byte() {
    let env = Env::new();
    env.run();
    let archive = paths::archive_dir(&env.data);
    {
        let store = env.store();
        for f in discover(&env.root).unwrap() {
            let src = store.source(&f.path.to_string_lossy()).unwrap().unwrap();
            let bytes = fs::read(&f.path).unwrap();
            assert_eq!(src.next_offset, bytes.len() as u64);
            assert_eq!(read_source(&store, &archive, src.id).unwrap(), bytes);
        }
    }

    env.append(&user_line("h0000001", "appended after the first run"));
    env.run();
    let store = env.store();
    let src = store
        .source(&env.main().to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(store.segments(src.id).unwrap().len(), 2);
    assert_eq!(
        read_source(&store, &archive, src.id).unwrap(),
        fs::read(env.main()).unwrap()
    );
}

#[test]
fn no_archive_writes_no_archive() {
    let env = Env::new();
    let r = run(&Options {
        archive: false,
        ..env.opts()
    })
    .unwrap();
    assert!(r.events_added > 0);
    assert_eq!((r.archive_raw_bytes, r.archive_compressed_bytes), (0, 0));
    assert!(!paths::archive_dir(&env.data).exists());
}

#[test]
fn dry_run_writes_nothing_but_counts() {
    let env = Env::new();
    let r = run(&Options {
        dry_run: true,
        ..env.opts()
    })
    .unwrap();
    assert_eq!(r.events_added, fixture_events(&env.root));
    assert_eq!(r.files_new, 3);
    assert!(r.archive_raw_bytes > 0);
    assert!(!env.data.exists());
}

#[cfg(unix)]
#[test]
fn data_dir_and_db_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    env.run();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&env.data), 0o700);
    assert_eq!(mode(&paths::db_path(&env.data)), 0o600);
    assert_eq!(mode(&paths::archive_dir(&env.data)), 0o700);
}

fn cli(args: &[&str]) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = recalla::cli::run(args.iter().copied(), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn cli_backfill_json_and_exit_codes() {
    let env = Env::new();
    let (root, data) = (env.root.to_str().unwrap(), env.data.to_str().unwrap());
    let base = [
        "recalla",
        "backfill",
        "--root",
        root,
        "--harness",
        "claude",
        "--data-dir",
        data,
    ];

    let (code, out, err) = cli(&[&base[..], &["--json"]].concat());
    assert_eq!((code, err.as_str()), (0, ""));
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    let report = &report["ingest"];
    assert_eq!(report["events_added"], fixture_events(&env.root));
    assert_eq!(report["files"]["new"], 3);

    let (code, out, _) = cli(&base);
    assert_eq!(code, 0);
    assert!(out.contains("3 unchanged"), "{out}");

    // Setup error: the data dir is a file.
    let file = env.root.join("not-a-dir");
    fs::write(&file, b"").unwrap();
    let bad = [
        "recalla",
        "backfill",
        "--root",
        root,
        "--harness",
        "claude",
        "--data-dir",
        file.to_str().unwrap(),
    ];
    let (code, _, err) = cli(&bad);
    assert_eq!(code, 2);
    assert!(err.starts_with("error:"));
}

#[cfg(unix)]
#[test]
fn cli_exits_1_when_a_file_fails() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    fs::set_permissions(env.main(), fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(env.main()).is_ok() {
        return; // running as root: permissions can't make the file fail
    }
    let (root, data) = (env.root.to_str().unwrap(), env.data.to_str().unwrap());
    let (code, out, _) = cli(&[
        "recalla",
        "backfill",
        "--root",
        root,
        "--harness",
        "claude",
        "--data-dir",
        data,
        "--json",
    ]);
    assert_eq!(code, 1);
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    let report = &report["ingest"];
    let failed = &report["files"]["failed"];
    assert_eq!(failed.as_array().unwrap().len(), 1);
    assert_eq!(failed[0]["error"], "PermissionDenied");
    assert_eq!(report["files"]["new"], 2);
}

#[cfg(unix)]
#[test]
fn cli_warns_about_a_loose_existing_data_dir_without_chmod() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    fs::create_dir_all(&env.data).unwrap();
    fs::set_permissions(&env.data, fs::Permissions::from_mode(0o755)).unwrap();
    let (root, data) = (env.root.to_str().unwrap(), env.data.to_str().unwrap());
    let args = [
        "recalla",
        "backfill",
        "--root",
        root,
        "--harness",
        "claude",
        "--data-dir",
        data,
    ];
    let (code, _, err) = cli(&args);
    assert_eq!(code, 0);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("warning:") && err.contains("755"), "{err}");
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&env.data), 0o755, "existing dir must not be chmod-ed");
    assert_eq!(mode(&paths::db_path(&env.data)), 0o600);

    // A private dir gets no warning.
    fs::set_permissions(&env.data, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(cli(&args).2, "");
}

#[test]
fn long_last_line_resumes_without_restart() {
    // The checkpoint tail hash must match on the next run even when the last
    // ingested line is longer than the 4 KB hash window.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let proj = root.join("projects/-tmp-long");
    fs::create_dir_all(&proj).unwrap();
    let file = proj.join("s.jsonl");
    let long = "w".repeat(10_000);
    let rec = |t: &str| {
        format!(
            r#"{{"type":"user","sessionId":"s","uuid":"{t}","message":{{"content":"{long} {t}"}}}}"#
        )
    };
    fs::write(&file, format!("{}\n", rec("a"))).unwrap();
    let data = dir.path().join("data");
    let opts = recalla::ingest::Options {
        claude_root: Some(root.clone()),
        codex_root: None,
        data_dir: data.clone(),
        archive: true,
        dry_run: false,
    };
    recalla::ingest::run(&opts).unwrap();
    let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
    std::io::Write::write_all(&mut f, format!("{}\n", rec("b")).as_bytes()).unwrap();
    let r = recalla::ingest::run(&opts).unwrap();
    assert_eq!(
        (r.files_resumed, r.files_restarted, r.events_added),
        (1, 0, 1)
    );
}

fn tool_line(uuid: &str, id: &str, name: &str, input: &serde_json::Value) -> String {
    json!({
        "type": "assistant", "uuid": uuid, "sessionId": SESSION,
        "timestamp": "2025-01-02T03:07:00Z", "cwd": "/tmp/demo", "gitBranch": "main",
        "message": {"role": "assistant", "content": [{
            "type": "tool_use", "id": id, "name": name, "input": input
        }]}
    })
    .to_string()
        + "\n"
}

fn ok_result(uuid: &str, id: &str) -> String {
    json!({
        "type": "user", "uuid": uuid, "sessionId": SESSION,
        "timestamp": "2025-01-02T03:08:00Z", "cwd": "/tmp/demo", "gitBranch": "main",
        "message": {"role": "user", "content": [{
            "type": "tool_result", "tool_use_id": id, "content": "ok", "is_error": false
        }]}
    })
    .to_string()
        + "\n"
}

#[test]
fn tool_input_secrets_with_escapes_never_reach_events_or_memories() {
    let env = Env::new();
    let pw = alnum(16);
    let assignment = format!("{}{}\t{pw}", "pass", "word=");
    env.append(&tool_line(
        "h0000001",
        "toolu_esc01",
        "Bash",
        &json!({"command": format!("cargo test --features x {assignment}")}),
    ));
    env.append(&ok_result("h0000002", "toolu_esc01"));
    let adr = format!("## D-001 Storage\n- Decision: db {assignment}\n- Why: speed\n");
    env.append(&tool_line(
        "h0000003",
        "toolu_esc02",
        "Write",
        &json!({"file_path": "/tmp/demo/DESIGN_DECISIONS.md", "content": adr}),
    ));
    let r = env.run();
    assert_eq!(r.failed, []);

    let mut store = env.store();
    let values = store.all_text_values().unwrap();
    assert!(
        values.iter().all(|v| !v.contains(&pw)),
        "leaked into events"
    );
    assert!(values.iter().any(|v| v.contains("[REDACTED:")));

    extract::run(&mut store, &ExtractOptions { rebuild: false }).unwrap();
    let mems = store.all_memories(&[Status::Active]).unwrap();
    assert!(
        mems.iter().all(|m| !m.text.contains(&pw)),
        "leaked into memories"
    );
    assert!(
        mems.iter()
            .any(|m| m.kind == Kind::Decision && m.text.contains("Storage")),
        "ADR decision should survive, redacted"
    );
}

#[test]
fn huge_adr_write_is_truncated_but_still_extracted() {
    let env = Env::new();
    let content = format!(
        "## D-001 Title\n- Decision: keep it\n\n{}",
        "filler line\n".repeat(9_000)
    );
    assert!(content.len() > 100_000);
    env.append(&tool_line(
        "i0000001",
        "toolu_big01",
        "Write",
        &json!({"file_path": "/tmp/demo/DESIGN_DECISIONS.md", "content": content}),
    ));
    assert_eq!(env.run().failed, []);

    let mut store = env.store();
    let values = store.all_text_values().unwrap();
    let stored = values
        .iter()
        .find(|v| v.contains("DESIGN_DECISIONS.md") && v.contains("keep it"))
        .unwrap();
    assert!(stored.len() <= MAX_EVENT_BYTES + 64, "{}", stored.len());
    assert!(serde_json::from_str::<serde_json::Value>(stored).is_ok());

    extract::run(&mut store, &ExtractOptions { rebuild: false }).unwrap();
    let mems = store.all_memories(&[Status::Active]).unwrap();
    assert!(
        mems.iter()
            .any(|m| m.kind == Kind::Decision && m.text.contains("keep it")),
        "{:?}",
        mems.iter().map(|m| &m.text).collect::<Vec<_>>()
    );
}
