//! Performance checks for the SPEC targets, on synthetic data. Ignored by
//! default; run with `make perf`
//! (`cargo test --release --test perf -- --ignored --nocapture`).
//!
//! - `backfill_300mb`: a 300 MB mixed corpus backfilled in under 5 minutes
//! - `hook_session_start_p95`: session-start under 100 ms p95 on 100k memories
//! - `mcp_recall_p95`: `recall` tool calls over the same store

#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::similar_names,
    clippy::unchecked_time_subtraction
)]

mod common;

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{CodexWriter, Env, Writer};
use recalla::extract;
use recalla::hook::{self, Ctx, Event};
use recalla::ingest::{self, Options};
use recalla::mcp::{McpCtx, serve};
use recalla::model::{Anchor, Kind, NewMemory, Origin, Provenance, Scope};
use recalla::paths;
use recalla::store::Store;
use serde_json::{Value, json};

const MB: u64 = 1 << 20;
const CORPUS_BYTES: u64 = 300 * MB;

/// splitmix64: small, seeded, good enough for shaping data.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }

    fn chance(&mut self, per_mille: usize) -> bool {
        self.below(1000) < per_mille
    }

    fn alnum(&mut self, n: usize) -> String {
        const SET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        (0..n).map(|_| SET[self.below(SET.len())] as char).collect()
    }
}

const WORDS: &[&str] = &[
    "store", "index", "parser", "cache", "worker", "queue", "schema", "handler", "client",
    "session", "pipeline", "retry", "token", "config", "router", "lock", "batch", "stream",
    "buffer", "layout", "render", "socket", "bundle", "plugin", "audit", "metric",
];
const MULTIBYTE: &[&str] = &[
    "日本語のテキスト、設定を確認しました。",
    "Überprüfung der Konfiguration abgeschlossen",
    "пожалуйста, проверьте сборку",
    "résumé naïve café — done ✓",
    "修复了缓存失效的问题",
];
const COMMANDS: &[&str] = &[
    "cargo build --release",
    "cargo test --all-targets",
    "npm run build",
    "npm test -- --ci",
    "make check",
    "pytest -q tests/",
    "go test ./...",
    "git status --short",
    "rg -n TODO src/",
];

fn sentence(rng: &mut Rng, lo: usize, hi: usize) -> String {
    let mut s = String::new();
    for i in 0..rng.range(lo, hi) {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(rng.pick(WORDS));
    }
    s
}

fn file_in(rng: &mut Rng) -> String {
    format!("src/{}/{}.rs", rng.pick(WORDS), rng.pick(WORDS))
}

/// Fake credentials, assembled at runtime so no secret-shaped literal exists
/// in source.
fn fake_secret(rng: &mut Rng) -> String {
    match rng.below(3) {
        0 => format!("{}{}", ["gh", "p_"].concat(), rng.alnum(36)),
        1 => format!("{}{}", ["AK", "IA"].concat(), rng.alnum(16).to_uppercase()),
        _ => format!("{}{}", ["sk", "-"].concat(), rng.alnum(48)),
    }
}

fn build_log(rng: &mut Rng, lo: usize, hi: usize) -> String {
    let bytes = rng.range(lo, hi);
    let mut out = String::with_capacity(bytes + 100);
    while out.len() < bytes {
        let _ = writeln!(
            out,
            "   Compiling {}-{} v{}.{}.{} (/work/{})",
            rng.pick(WORDS),
            rng.pick(WORDS),
            rng.below(4),
            rng.below(30),
            rng.below(9),
            rng.pick(WORDS)
        );
    }
    out
}

fn blob(rng: &mut Rng, lo: usize, hi: usize) -> String {
    let bytes = rng.range(lo, hi);
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::from("data:application/octet-stream;base64,");
    out.extend((0..bytes).map(|_| B64[rng.below(B64.len())] as char));
    out
}

fn prose(rng: &mut Rng) -> String {
    let (a, b) = (rng.pick(WORDS), rng.pick(WORDS));
    let mut text = match rng.below(5) {
        0 => format!(
            "Decision: use the {a} {b} for {} handling.",
            rng.pick(WORDS)
        ),
        1 => format!(
            "The root cause was the {a} {b} holding a lock because the {} never flushed.",
            rng.pick(WORDS)
        ),
        2 => format!(
            "We use {a} {}.{} for {b} caching.",
            rng.below(5),
            rng.below(20)
        ),
        3 => format!("The {a} config is in {}.", file_in(rng)),
        _ => format!("I looked at the {a} module and updated the {b} tests."),
    };
    if rng.chance(300) {
        text.push(' ');
        text.push_str(&sentence(rng, 20, 120));
    }
    if rng.chance(150) {
        text.push(' ');
        text.push_str(rng.pick(MULTIBYTE));
    }
    text
}

fn directive(rng: &mut Rng) -> String {
    match rng.below(4) {
        0 => format!("Never edit {} by hand.", file_in(rng)),
        1 => format!("Always run {} before committing.", rng.pick(COMMANDS)),
        2 => format!(
            "Please fix the {} {} bug.",
            rng.pick(WORDS),
            rng.pick(WORDS)
        ),
        _ => format!("{} {}", rng.pick(MULTIBYTE), sentence(rng, 8, 8)),
    }
}

/// One turn in the shape `Writer::turns` / `CodexWriter::turns` take.
fn turn(rng: &mut Rng, codex: bool) -> Value {
    let roll = rng.below(100);
    let mut result = match roll {
        0..=9 => return json!({"user": directive(rng)}),
        10..=39 => return json!({"assistant": prose(rng)}),
        40..=69 => build_log(rng, 200, 3000),
        70..=74 => build_log(rng, 5_000, 50_000),
        75..=77 if !codex => blob(rng, 10_000, 40_000),
        _ => sentence(rng, 10, 400),
    };
    if rng.chance(1) {
        let _ = write!(result, "\nexport TOKEN={}\n", fake_secret(rng));
    }
    if codex || roll < 70 {
        let mut command = rng.pick(COMMANDS).to_owned();
        if rng.chance(5) {
            let _ = write!(command, " --token {}", fake_secret(rng));
        }
        return json!({
            "tool": "Bash", "input": {"command": command},
            "result": result, "error": rng.chance(150),
        });
    }
    let tool = if roll % 2 == 0 { "Read" } else { "Edit" };
    let input = json!({"file_path": file_in(rng), "old_string": "a", "new_string": "b"});
    json!({"tool": tool, "input": input, "result": result, "error": false})
}

struct Corpus {
    files: u64,
    events: u64,
    bytes: u64,
}

/// Fills `path` with turns until it reaches `target` bytes.
fn write_file(
    path: &Path,
    target: u64,
    rng: &mut Rng,
    mut emit: impl FnMut(&Value) -> String,
    codex: bool,
    corpus: &mut Corpus,
) {
    let mut out = BufWriter::new(File::create(path).unwrap());
    let mut written = 0u64;
    while written < target {
        let t = turn(rng, codex);
        let chunk = emit(&t);
        written += chunk.len() as u64;
        corpus.events += chunk.lines().count() as u64;
        out.write_all(chunk.as_bytes()).unwrap();
    }
    out.flush().unwrap();
    corpus.files += 1;
    corpus.bytes += written;
}

/// Real project dirs (with `.git`) so project keys and anchors resolve.
fn make_projects(base: &Path, n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            let dir = base.join(format!("proj{i}"));
            fs::create_dir_all(dir.join(".git")).unwrap();
            fs::create_dir_all(dir.join("src")).unwrap();
            dir
        })
        .collect()
}

fn generate_corpus(env: &Env, rng: &mut Rng) -> Corpus {
    let base = env.root.with_file_name("work");
    let projects = make_projects(&base, 20);
    let cwd = |rng: &mut Rng| {
        projects[rng.below(projects.len())]
            .to_string_lossy()
            .into_owned()
    };
    let mut corpus = Corpus {
        files: 0,
        events: 0,
        bytes: 0,
    };
    // Sizes: ~80% of bytes Claude (a few 5-20 MB files, many small), ~20% Codex.
    let mut plan: Vec<(bool, u64)> = Vec::new();
    let (mut claude, mut codex) = (0u64, 0u64);
    while claude < CORPUS_BYTES * 8 / 10 {
        let size = if plan.len() % 60 == 0 {
            rng.range(5, 20) as u64 * MB
        } else {
            rng.range(20, 600) as u64 * 1024
        };
        claude += size;
        plan.push((false, size));
    }
    while codex < CORPUS_BYTES * 2 / 10 {
        let size = if plan.len() % 40 == 0 {
            rng.range(5, 12) as u64 * MB
        } else {
            rng.range(20, 600) as u64 * 1024
        };
        codex += size;
        plan.push((true, size));
    }
    for (i, (is_codex, size)) in plan.into_iter().enumerate() {
        let cwd = cwd(rng);
        let session = format!("perf-{i:04}");
        if is_codex {
            let dir = env.codex_root.join("sessions/2026/11/01");
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("rollout-2026-11-01T10-00-00-{session}.jsonl"));
            let mut w = CodexWriter::new(&session);
            let mut first = true;
            let emit = |t: &Value| {
                let turns = std::slice::from_ref(t);
                if std::mem::take(&mut first) {
                    w.turns(&cwd, turns)
                } else {
                    // Only the first call emits the session_meta line.
                    w.turns(&cwd, turns)
                        .split_once('\n')
                        .map_or_else(String::new, |(_, rest)| rest.to_owned())
                }
            };
            write_file(&path, size, rng, emit, true, &mut corpus);
        } else {
            let path = env.session_path(&cwd, &session);
            let mut w = Writer::new(&session, &cwd);
            let emit = |t: &Value| w.turns(std::slice::from_ref(t));
            write_file(&path, size, rng, emit, false, &mut corpus);
        }
    }
    corpus
}

/// Current RSS is not peak, so only Linux's high-water mark is reported.
fn peak_rss() -> String {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM"))
                .map(str::to_owned)
        })
        .map_or_else(|| "n/a (Linux only)".to_owned(), |l| l.replace('\t', " "))
}

#[test]
#[ignore = "perf: run via `make perf`"]
fn backfill_300mb() {
    let env = Env::new();
    let mut rng = Rng(0x5EED_0001);
    let gen_started = Instant::now();
    let corpus = generate_corpus(&env, &mut rng);
    println!(
        "generated {:.1} MB in {} files ({} lines) in {:.1}s",
        corpus.bytes as f64 / MB as f64,
        corpus.files,
        corpus.events,
        gen_started.elapsed().as_secs_f64()
    );
    let opts = Options {
        claude_root: Some(env.root.clone()),
        codex_root: Some(env.codex_root.clone()),
        data_dir: env.data.clone(),
        archive: true,
        dry_run: false,
    };
    let started = Instant::now();
    let report = ingest::run(&opts).unwrap();
    let ingest_time = started.elapsed();
    let mut store = env.store();
    let extracted = extract::run(&mut store, &extract::Options::default()).unwrap();
    let total = started.elapsed();
    let mb = corpus.bytes as f64 / MB as f64;
    println!(
        "ingest:  {:.1}s ({} events, {} redactions, {} failed, {} malformed lines)",
        ingest_time.as_secs_f64(),
        report.events_added,
        report.redactions(),
        report.failed.len(),
        report.malformed_lines
    );
    println!(
        "extract: {:.1}s ({} events, {} new memories, {} duplicates)",
        (total - ingest_time).as_secs_f64(),
        extracted.events_processed,
        extracted.new_total(),
        extracted.duplicate_total()
    );
    println!(
        "backfill: {mb:.1} MB, {} files, {} events, {} memories, {:.1}s wall, {:.2} MB/s, peak RSS {}",
        report.files_seen,
        report.events_added,
        extracted.new_total(),
        total.as_secs_f64(),
        mb / total.as_secs_f64(),
        peak_rss()
    );
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(report.redactions() > 0, "no redaction work happened");
    assert!(extracted.new_total() > 0);
    assert!(total < Duration::from_secs(300), "backfill took {total:?}");

    let again = Instant::now();
    let second = ingest::run(&opts).unwrap();
    extract::run(&mut env.store(), &extract::Options::default()).unwrap();
    let again = again.elapsed();
    println!("no-op rerun: {:.2}s", again.as_secs_f64());
    assert_eq!(second.events_added, 0);
    assert!(again < Duration::from_secs(5), "no-op took {again:?}");
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let idx = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[idx]
}

fn report(label: &str, mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let p95 = percentile(&samples, 0.95);
    println!(
        "{label}: n={} p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms",
        samples.len(),
        ms(percentile(&samples, 0.50)),
        ms(p95),
        ms(percentile(&samples, 0.99)),
        ms(*samples.last().unwrap())
    );
    p95
}

const MEMORIES: usize = 100_000;
const STORE_PROJECTS: usize = 50;

struct Fixture {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    /// The one real project dir the hook / MCP calls run in.
    project: String,
}

/// 100k memories over 50 projects (project 0 is a real dir with real files),
/// ~1% global, ~10% anchored to files of the real project.
fn build_store() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    let real = base.join("work/real");
    fs::create_dir_all(real.join(".git")).unwrap();
    fs::create_dir_all(real.join("src")).unwrap();
    let files: Vec<String> = (0..40)
        .map(|i| {
            let rel = format!("src/mod{i}.rs");
            let body = format!("pub fn handler_{i}() -> u32 {{ {i} }}\n").repeat(50);
            fs::write(real.join(&rel), body).unwrap();
            rel
        })
        .collect();
    let project = real.to_string_lossy().into_owned();
    let data = base.join("data");
    fs::create_dir_all(&data).unwrap();
    let mut store = Store::open(paths::db_path(&data)).unwrap();
    let mut rng = Rng(0x5EED_0002);
    let kinds = [
        Kind::Decision,
        Kind::Fact,
        Kind::Procedure,
        Kind::Gotcha,
        Kind::Preference,
        Kind::Task,
    ];
    let mut items = Vec::with_capacity(5000);
    for i in 0..MEMORIES {
        let p = rng.below(STORE_PROJECTS);
        let scope = if rng.chance(10) {
            Scope::Global
        } else if p == 0 {
            Scope::Project(project.clone())
        } else {
            Scope::Project(format!("/work/other{p}"))
        };
        let anchors = if p == 0 && rng.chance(100) {
            vec![Anchor::new(files[rng.below(files.len())].clone())]
        } else {
            Vec::new()
        };
        let text = format!("{} #{i}", sentence(&mut rng, 6, 30));
        let new = NewMemory {
            kind: kinds[rng.below(kinds.len())],
            text,
            scope,
            confidence: 0.5 + rng.below(50) as f32 / 100.0,
            importance: u8::try_from(rng.below(4)).unwrap(),
        };
        let mut prov = Provenance::new(Origin::Backfill);
        prov.session_id = Some(format!("s{}", i % 700));
        prov.source_path = Some(format!("/s/{}.jsonl", i % 700));
        prov.line = i64::try_from(i).ok();
        items.push((new, prov, anchors));
        if items.len() == 5000 {
            store.insert_many(&items).unwrap();
            items.clear();
        }
    }
    store.insert_many(&items).unwrap();
    drop(store);
    Fixture {
        _tmp: tmp,
        data,
        project,
    }
}

#[test]
#[ignore = "perf: run via `make perf`"]
fn hook_session_start_p95() {
    let built = Instant::now();
    let fx = build_store();
    println!(
        "built {MEMORIES} memories in {:.1}s",
        built.elapsed().as_secs_f64()
    );
    let ctx = Ctx {
        data_dir: fx.data.clone(),
        root: None,
    };
    let stdin = json!({"cwd": fx.project, "session_id": "perf", "hook_event_name": "SessionStart"})
        .to_string();
    let run_once = || {
        let mut out = Vec::new();
        let t = Instant::now();
        hook::run(
            Event::SessionStart,
            Cursor::new(stdin.clone()),
            &mut out,
            &ctx,
        );
        let took = t.elapsed();
        assert!(!out.is_empty(), "session-start printed nothing");
        took
    };
    run_once(); // warm the page cache
    let in_process: Vec<_> = (0..200).map(|_| run_once()).collect();
    let p95 = report("session-start in-process (200)", in_process);

    let spawn_once = || {
        let t = Instant::now();
        let mut child = Command::new(env!("CARGO_BIN_EXE_recalla"))
            .args(["hook", "session-start", "--data-dir"])
            .arg(&fx.data)
            .env_remove("RECALLA_BUDGET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(!out.stdout.is_empty());
        t.elapsed()
    };
    // The first exec of a fresh binary pays one-off OS costs (e.g. macOS
    // code-signature checks); report it apart from the steady state.
    println!("first spawn: {:.2}ms", spawn_once().as_secs_f64() * 1000.0);
    let spawned: Vec<_> = (0..30).map(|_| spawn_once()).collect();
    report("session-start spawned (30)", spawned);
    assert!(p95 < Duration::from_millis(100), "in-process p95 {p95:?}");
}

#[test]
#[ignore = "perf: run via `make perf`"]
fn mcp_recall_p95() {
    let fx = build_store();
    let ctx = McpCtx {
        data_dir: fx.data.clone(),
        project: Some(fx.project.clone()),
    };
    let call = |i: usize, query: bool| {
        let args = if query {
            json!({"query": format!("{} {}", WORDS[i % WORDS.len()], WORDS[(i / 3) % WORDS.len()])})
        } else {
            json!({})
        };
        let req = json!({"jsonrpc": "2.0", "id": i, "method": "tools/call",
            "params": {"name": "recall", "arguments": args}});
        let mut out = Vec::new();
        let t = Instant::now();
        serve(Cursor::new(format!("{req}\n")), &mut out, &ctx).unwrap();
        let took = t.elapsed();
        let reply: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        took
    };
    call(0, false); // warm
    let plain: Vec<_> = (0..200).map(|i| call(i, false)).collect();
    let queried: Vec<_> = (0..200).map(|i| call(i, true)).collect();
    let p_plain = report("mcp recall, no query (200)", plain);
    let p_query = report("mcp recall, with query (200)", queried);
    let limit = Duration::from_millis(100);
    assert!(
        p_plain < limit && p_query < limit,
        "{p_plain:?} {p_query:?}"
    );
}

/// 700 sessions (20k events) and 2k injections on top of the 100k-memory store.
fn add_dashboard_rows(db: &Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT INTO sources (id, harness, path, identity, size, next_offset, updated_at)
         WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 699)
         SELECT i + 1, 'claude-code', '/s/' || i || '.jsonl', 'x', 1, 1, 1 FROM n;
         INSERT INTO events (source_id, harness, session_id, line, seq, byte_offset, ts, cwd,
                             sidechain, kind, text)
         WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 19999)
         SELECT i % 700 + 1, 'claude-code', 's' || (i % 700), i, 0, 0, 1700000000 + i,
                '/work/other' || (i % 50), 0, 'user_text', 'event text ' || i FROM n;
         UPDATE provenance SET harness = 'claude-code';
         INSERT INTO injections (at, project, budget, est_tokens, memory_uids, via, session_id,
                                 pack_text, candidate_count, candidate_tokens)
         WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 1999)
         SELECT 1700000000 + i, '/work/other' || (i % 50), 2000, 400, '[]', 'hook',
                's' || (i % 700), 'pack', 900, 40000 FROM n;",
    )
    .unwrap();
}

/// Dashboard queries on the 100k-memory store plus 700 sessions (20k events)
/// and 2k injections.
#[test]
#[ignore = "perf: run via `make perf`"]
fn dashboard_queries_p95() {
    use recalla::store::{InjectionFilter, MemoryFilter, MemorySort, SessionFilter};
    let fx = build_store();
    let db = paths::db_path(&fx.data);
    add_dashboard_rows(&db);
    let store = Store::open(&db).unwrap();
    let limit = Duration::from_millis(100);
    let time = |label: &str, f: &dyn Fn()| {
        f(); // warm
        let samples: Vec<_> = (0..50)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .collect();
        let p95 = report(label, samples);
        assert!(p95 < limit, "{label} p95 {p95:?}");
    };
    let mem = MemoryFilter::default();
    time("ui_memories first page", &|| {
        assert_eq!(store.ui_memories(&mem, None, 50).unwrap().rows.len(), 50);
    });
    let filtered = MemoryFilter {
        kinds: vec![Kind::Gotcha, Kind::Decision],
        project: Some("/work/other7".to_owned()),
        sort: MemorySort::Confidence,
        ..MemoryFilter::default()
    };
    time("ui_memories filtered (kind+project+confidence)", &|| {
        assert!(
            !store
                .ui_memories(&filtered, None, 50)
                .unwrap()
                .rows
                .is_empty()
        );
    });
    let seen = MemoryFilter {
        sort: MemorySort::Seen,
        harness: Some("claude-code".to_owned()),
        ..MemoryFilter::default()
    };
    time("ui_memories sort=seen + harness", &|| {
        assert_eq!(store.ui_memories(&seen, None, 50).unwrap().rows.len(), 50);
    });
    let fts = MemoryFilter {
        q: Some(format!("{} {}", WORDS[1], WORDS[2])),
        ..MemoryFilter::default()
    };
    time("ui_memories FTS", &|| {
        store.ui_memories(&fts, None, 50).unwrap();
    });
    time("ui_sessions first page", &|| {
        let page = store
            .ui_sessions(&SessionFilter::default(), None, 50)
            .unwrap();
        assert_eq!(page.rows.len(), 50);
    });
    time("ui_sessions q+harness", &|| {
        let f = SessionFilter {
            harness: Some("claude-code".to_owned()),
            q: Some("s12".to_owned()),
            ..SessionFilter::default()
        };
        store.ui_sessions(&f, None, 50).unwrap();
    });
    time("ui_overview", &|| {
        assert_eq!(store.ui_overview().unwrap().sessions, 700);
    });
    time("ui_injections", &|| {
        let f = InjectionFilter {
            via: Some("hook".to_owned()),
            ..InjectionFilter::default()
        };
        assert_eq!(store.ui_injections(&f, None, 50).unwrap().rows.len(), 50);
    });
}

/// `GET /api/memories` and `/api/sessions` through the real HTTP server on the
/// 100k-memory store: under 150 ms p95.
#[test]
#[ignore = "perf: run via `make perf`"]
fn dashboard_http_p95() {
    use common::http;
    use recalla::ui::UiHandler;
    use recalla::ui::security::{LOGIN_TTL, LoginCode, Token};
    use recalla::ui::server::{ServeOptions, Server};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let fx = build_store();
    add_dashboard_rows(&paths::db_path(&fx.data));
    let code = Arc::new(LoginCode::generate(LOGIN_TTL).unwrap());
    let code_text = code.as_str().to_owned();
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = ServeOptions::new(0, fx.data.clone());
    opts.stop = Some(Arc::clone(&stop));
    let server = Server::bind(opts).unwrap();
    let addr = server.local_addr().unwrap();
    let handler = UiHandler::new(
        Token::generate().unwrap(),
        Token::generate().unwrap(),
        code,
        addr.port(),
        fx.data.clone(),
    );
    let thread = std::thread::spawn(move || server.run(handler).unwrap());
    let login = http::get(addr, &format!("/?c={code_text}"), &[]);
    let cookie = http::cookie_pair(&login).unwrap();
    let limit = Duration::from_millis(150);
    for (label, target) in [
        ("GET /api/memories", "/api/memories?limit=50"),
        ("GET /api/sessions", "/api/sessions?limit=50"),
    ] {
        let fetch = || {
            let r = http::get(addr, target, &[("Cookie", &cookie)]);
            assert_eq!(r.status, 200, "{}", r.body);
        };
        fetch();
        let samples: Vec<_> = (0..50)
            .map(|_| {
                let t = Instant::now();
                fetch();
                t.elapsed()
            })
            .collect();
        let p95 = report(label, samples);
        assert!(p95 < limit, "{label} p95 {p95:?}");
    }
    stop.store(true, Ordering::SeqCst);
    thread.join().unwrap();
}
